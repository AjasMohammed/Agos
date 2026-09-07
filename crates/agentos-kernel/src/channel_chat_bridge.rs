//! Bridges external channel inbound chat to `Kernel::chat_infer_with_tools`.
//!
//! `InboundRouter` holds an `Arc<KernelChatBridge>` created before `Kernel` is
//! wrapped in `Arc`. After `Arc::new(kernel)`, call [`Kernel::wire_inbound_chat_bridge`]
//! so [`KernelChatBridge::set_kernel`] can resolve a `Weak<Kernel>` for inference.
//!
//! Channel conversations are persisted as ordinary `ChatStore` sessions (one per
//! channel instance + bound agent, see [`ChatStore::get_or_create_channel_session`]).
//! They used to live in a process-local map, which meant a Telegram/Discord thread
//! was forgotten on every kernel restart and was invisible to the panel and to the
//! agent's own `chat-search`. Persisting them also gives the turn a real
//! `session_id`, which is what session-scoped tool state keys off.

use crate::Kernel;
use agentos_types::{AgentID, ChannelInstanceID};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// Max (user, assistant) pairs replayed to the LLM per channel turn.
///
/// A storage cap, this is not: the full transcript stays in `chat.db`. It only
/// bounds how much of it is re-sent as context each turn.
const MAX_HISTORY_ROUNDS: usize = 12;
/// Per-call inference timeout for channel chat. Sized to accommodate slower
/// backends (e.g. the Claude Code subprocess adapter, which spawns a process
/// and re-sends the full context per turn). Leaner context (see the claude-code
/// adapter's capped window) keeps most replies well under this.
///
/// Must also exceed the approval-escalation window: a tool call from a channel
/// turn can park waiting for the operator, and abandoning it here would report
/// a timeout while the escalation is still live and about to run the tool.
const CHANNEL_CHAT_TIMEOUT_SECS: u64 = 660;

/// Stable identity of one channel conversation: the channel instance plus the
/// agent bound to it. Rebinding the channel to another agent therefore starts a
/// new thread rather than splicing two agents into one transcript.
fn channel_key(channel_id: ChannelInstanceID, agent_name: &str) -> String {
    format!("channel:{channel_id}:{agent_name}")
}

pub struct KernelChatBridge {
    kernel: Mutex<Option<Weak<Kernel>>>,
}

impl Default for KernelChatBridge {
    fn default() -> Self {
        Self {
            kernel: Mutex::new(None),
        }
    }
}

impl KernelChatBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire the running kernel (call once after `Arc::new(kernel)`).
    pub fn set_kernel(&self, k: Weak<Kernel>) {
        *self.kernel.lock().unwrap_or_else(|e| e.into_inner()) = Some(k);
    }

    fn upgrade_kernel(&self) -> Option<Arc<Kernel>> {
        self.kernel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()?
            .upgrade()
    }

    /// Online agent names for `/agents` and validation.
    pub async fn list_online_agent_names(&self) -> Option<Vec<String>> {
        let k = self.upgrade_kernel()?;
        let reg = k.agent_registry.read().await;
        Some(
            reg.list_online()
                .into_iter()
                .map(|a| a.name.clone())
                .collect(),
        )
    }

    pub async fn agent_id_for_name(&self, name: &str) -> Option<AgentID> {
        let k = self.upgrade_kernel()?;
        let reg = k.agent_registry.read().await;
        reg.get_by_name(name).map(|a| a.id)
    }

    /// A human-readable session title, e.g. `"Ops bot (telegram) · assistant"`.
    async fn session_title(
        k: &Arc<Kernel>,
        channel_id: ChannelInstanceID,
        agent_name: &str,
    ) -> String {
        match k.channel_registry.get_by_id(&channel_id).await {
            Ok(Some(c)) => format!("{} ({}) · {}", c.display_name, c.kind, agent_name),
            _ => format!("Channel chat · {agent_name}"),
        }
    }

    /// Resolve the channel's session id and the history to replay into it.
    async fn load_session(
        k: &Arc<Kernel>,
        channel_id: ChannelInstanceID,
        agent_name: &str,
    ) -> Result<(String, Vec<(String, String)>), String> {
        let title = Self::session_title(k, channel_id, agent_name).await;
        let key = channel_key(channel_id, agent_name);
        let store = Arc::clone(&k.chat_store);
        let agent = agent_name.to_string();
        let session_id = tokio::task::spawn_blocking(move || {
            store.get_or_create_channel_session(&key, &agent, &title)
        })
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(|e| e.to_string())?;

        let store = Arc::clone(&k.chat_store);
        let sid = session_id.clone();
        let prior = tokio::task::spawn_blocking(move || store.get_messages(&sid))
            .await
            .map_err(|e| format!("join error: {e}"))?
            .map_err(|e| e.to_string())?;

        // Tool rows are machine payloads and blank turns are rejected by several
        // providers, so neither is replayed.
        let mut history: Vec<(String, String)> = prior
            .into_iter()
            .filter(|m| (m.role == "user" || m.role == "assistant") && !m.content.trim().is_empty())
            .map(|m| (m.role, m.content))
            .collect();
        let cap = MAX_HISTORY_ROUNDS * 2;
        if history.len() > cap {
            history.drain(0..history.len() - cap);
        }
        // The transcript is not perfectly alternating — a turn whose inference
        // failed leaves an orphan `user` row — so the trim above can land on an
        // `assistant` turn. Anthropic rejects a history that opens with one.
        if history.first().is_some_and(|(role, _)| role == "assistant") {
            history.remove(0);
        }
        Ok((session_id, history))
    }

    /// Run chat inference for a channel message, persisting the turn.
    ///
    /// Inference is bounded by [`CHANNEL_CHAT_TIMEOUT_SECS`]; times out with an
    /// error message rather than blocking the InboundRouter indefinitely.
    pub async fn channel_chat(
        &self,
        channel_id: ChannelInstanceID,
        agent_name: &str,
        user_message: &str,
        user_parts: Option<Vec<agentos_types::ContentPart>>,
    ) -> Result<String, String> {
        let k = self
            .upgrade_kernel()
            .ok_or_else(|| "Kernel is not ready for channel chat".to_string())?;

        // A store hiccup must not cost the user their reply — degrade to a
        // stateless turn instead of failing the message outright.
        let (session_id, history) = match Self::load_session(&k, channel_id, agent_name).await {
            Ok((sid, hist)) => (Some(sid), hist),
            Err(e) => {
                tracing::warn!(
                    channel_id = %channel_id,
                    agent = agent_name,
                    error = %e,
                    "Channel chat history unavailable; running this turn without it"
                );
                (None, Vec::new())
            }
        };

        // Persist the user turn before inference, mirroring the web/API path so a
        // crash mid-inference still leaves the question in the transcript.
        if let Some(sid) = &session_id {
            let store = Arc::clone(&k.chat_store);
            let (sid, text) = (sid.clone(), user_message.to_string());
            match tokio::task::spawn_blocking(move || store.add_message(&sid, "user", &text, None))
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!(error = %e, "Failed to persist channel user turn"),
                Err(e) => {
                    tracing::error!(error = %e, "spawn_blocking panicked persisting user turn")
                }
            }
        }

        let result = tokio::time::timeout(
            Duration::from_secs(CHANNEL_CHAT_TIMEOUT_SECS),
            k.chat_infer_with_tools(
                agent_name,
                &history,
                user_message,
                user_parts,
                session_id.as_deref(),
            ),
        )
        .await
        .map_err(|_| {
            format!("Chat inference timed out after {CHANNEL_CHAT_TIMEOUT_SECS}s — try again")
        })??;

        if let Some(sid) = &session_id {
            // Tool rows before the assistant turn so the timeline orders
            // user → tool… → assistant (mirrors the web UI + streaming path).
            if !result.tool_calls.is_empty() {
                let store = Arc::clone(&k.chat_store);
                let (sid, calls) = (sid.clone(), result.tool_calls.clone());
                match tokio::task::spawn_blocking(move || store.add_tool_calls(&sid, &calls)).await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "Failed to persist channel tool calls")
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "spawn_blocking panicked saving tool calls")
                    }
                }
            }

            let store = Arc::clone(&k.chat_store);
            let (sid, answer) = (sid.clone(), result.answer.clone());
            let tokens = result.tokens_used;
            let cost = result.cost_usd;
            match tokio::task::spawn_blocking(move || {
                store.add_assistant_message(
                    &sid,
                    &answer,
                    Some(tokens),
                    if cost.is_finite() && cost > 0.0 {
                        Some(cost)
                    } else {
                        None
                    },
                )
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "Failed to persist channel assistant turn")
                }
                Err(e) => {
                    tracing::error!(error = %e, "spawn_blocking panicked persisting assistant turn")
                }
            }
        }

        Ok(result.answer)
    }

    /// End the channel's current thread (e.g. when the bound agent changes).
    ///
    /// The transcript itself is kept — see [`ChatStore::rotate_channel_session`].
    pub async fn clear_history(&self, channel_id: ChannelInstanceID, agent_name: &str) {
        let Some(k) = self.upgrade_kernel() else {
            return;
        };
        let store = Arc::clone(&k.chat_store);
        let key = channel_key(channel_id, agent_name);
        match tokio::task::spawn_blocking(move || store.rotate_channel_session(&key)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "Failed to rotate channel chat session"),
            Err(e) => {
                tracing::warn!(error = %e, "spawn_blocking panicked rotating channel session")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_store::ChatStore;

    #[test]
    fn channel_key_is_scoped_to_channel_and_agent() {
        let a = ChannelInstanceID::new();
        let b = ChannelInstanceID::new();
        assert_ne!(channel_key(a, "ops"), channel_key(a, "research"));
        assert_ne!(channel_key(a, "ops"), channel_key(b, "ops"));
        assert_eq!(channel_key(a, "ops"), channel_key(a, "ops"));
    }

    #[test]
    fn channel_session_persists_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(&dir.path().join("chat.db")).unwrap();
        let key = "channel:test:ops";

        let sid = store
            .get_or_create_channel_session(key, "ops", "Ops · ops")
            .unwrap();
        store
            .add_message(&sid, "user", "remember: my cat is Mia", None)
            .unwrap();
        store
            .add_assistant_message(&sid, "Noted.", None, None)
            .unwrap();

        // Same key resolves to the same session — this is what makes a channel
        // conversation survive a kernel restart.
        assert_eq!(
            store
                .get_or_create_channel_session(key, "ops", "Ops · ops")
                .unwrap(),
            sid
        );
        assert_eq!(store.get_messages(&sid).unwrap().len(), 2);

        // Rotation starts a new thread but must not destroy the old transcript.
        store.rotate_channel_session(key).unwrap();
        let sid2 = store
            .get_or_create_channel_session(key, "ops", "Ops · ops")
            .unwrap();
        assert_ne!(sid2, sid);
        assert_eq!(store.get_messages(&sid).unwrap().len(), 2);
        assert!(store.get_messages(&sid2).unwrap().is_empty());
    }
}
