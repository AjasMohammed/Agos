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

/// How often to re-assert the channel's "typing…" indicator while a turn runs.
///
/// Telegram clears the indicator after ~5s, so this must stay under that or the
/// signal flickers. Even at the [`CHANNEL_CHAT_TIMEOUT_SECS`] ceiling that is
/// only ~165 pings, and `sendChatAction` is not a message — it does not count
/// against the per-chat message rate limit.
const TYPING_PING_INTERVAL_SECS: u64 = 4;

// `tokio::time::interval` panics on a zero period, and the keepalive's handle is
// never joined — that panic would be swallowed and the indicator would simply
// never appear again, with nothing in the log. Make it a compile error instead.
const _: () = assert!(TYPING_PING_INTERVAL_SECS > 0);

/// Aborts the typing keepalive when the turn ends, however it ends.
///
/// Dropping a bare `JoinHandle` detaches the task rather than cancelling it, so
/// without this an early `return Err(..)` would leave a loop pinging Telegram
/// forever about a turn that is already over.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

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
        // providers, so neither is replayed; nor are stored non-answers.
        let mut history: Vec<(String, String)> = prior
            .into_iter()
            .filter(|m| match m.role.as_str() {
                "user" => !m.content.trim().is_empty(),
                "assistant" => !crate::kernel::is_unreplayable_assistant_turn(&m.content),
                _ => false,
            })
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

    /// Ping the channel's "typing…" indicator until the returned guard drops.
    ///
    /// Best-effort and entirely off the critical path: the first ping is sent
    /// from the spawned task, not awaited here, so a wedged channel delays the
    /// reply by exactly nothing. Adapters without an indicator (the trait
    /// default) turn every ping into a cheap no-op.
    fn spawn_typing_indicator(
        router: Arc<crate::notification_router::NotificationRouter>,
        instance_id: String,
    ) -> AbortOnDrop {
        AbortOnDrop(tokio::spawn(async move {
            // A ticker, not `ping().await; sleep(N)` — the latter has a real
            // period of `N + round-trip`, which against a ~5s expiry drifts the
            // refresh past the deadline it exists to beat. The first tick
            // completes immediately, so the eager first ping is preserved.
            let mut ticker = tokio::time::interval(Duration::from_secs(TYPING_PING_INTERVAL_SECS));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                // `false` means the channel has no indicator, or pinging it has
                // stopped working. Either way it will not start working before
                // the turn ends, so stop instead of retrying ~165 times.
                if !router.typing_on_channel(&instance_id).await {
                    break;
                }
            }
        }))
    }

    /// Record a failed turn as the session's assistant reply.
    ///
    /// Keeps the transcript alternating and, more importantly, distinguishes
    /// "answered with an error" from "killed mid-inference" — the latter is what
    /// `Kernel::interrupted_channel_turns` looks for.
    async fn persist_failed_turn(k: &Arc<Kernel>, session_id: Option<&str>, error: &str) {
        let Some(sid) = session_id else {
            return;
        };
        let store = Arc::clone(&k.chat_store);
        let (sid_owned, text) = (
            sid.to_string(),
            format!("{} {error})", crate::kernel::FAILED_TURN_PREFIX),
        );
        match tokio::task::spawn_blocking(move || {
            store.add_assistant_message(&sid_owned, &text, None, None)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "Failed to persist channel turn failure"),
            Err(e) => tracing::warn!(error = %e, "spawn_blocking panicked persisting turn failure"),
        }
        Self::notify_panel(k, Some(sid), "assistant").await;
    }

    /// Wake open panel tabs on the `chat` realtime channel. The panel refetches
    /// the transcript on this event; the API path already emits it, and without
    /// the same call here a channel turn only showed up after a reload.
    async fn notify_panel(k: &Arc<Kernel>, session_id: Option<&str>, role: &str) {
        let Some(session_id) = session_id else {
            return;
        };
        k.emit_event(
            agentos_types::EventType::ChatMessageAdded,
            agentos_types::EventSource::AgentMessageBus,
            agentos_types::EventSeverity::Info,
            serde_json::json!({ "session_id": session_id, "role": role }),
            0,
        )
        .await;
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

        // Tell the channel an agent is working on this. Slow backends run for
        // minutes across several tool iterations, and from the user's side that
        // is indistinguishable from a dead bot — they re-send, which queues yet
        // another multi-minute turn. Started before the history load and the
        // user-row write, because those are two `chat.db` round-trips that under
        // contention are the first seconds the user spends waiting. Held until
        // this function returns, however it returns.
        let _typing = Self::spawn_typing_indicator(
            Arc::clone(&k.notification_router),
            channel_id.to_string(),
        );

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
            Self::notify_panel(&k, session_id.as_deref(), "user").await;
        }

        let result = match tokio::time::timeout(
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
        {
            Ok(Ok(r)) => r,
            // A failed turn must still close its transcript entry. Left as a bare
            // trailing `user` row it reads exactly like a turn the kernel was
            // killed in the middle of, and the boot sweep would then tell the user
            // to resend a message that was already answered with an error.
            Ok(Err(e)) => {
                Self::persist_failed_turn(&k, session_id.as_deref(), &e).await;
                return Err(e);
            }
            Err(_) => {
                let e = format!(
                    "Chat inference timed out after {CHANNEL_CHAT_TIMEOUT_SECS}s — try again"
                );
                Self::persist_failed_turn(&k, session_id.as_deref(), &e).await;
                return Err(e);
            }
        };

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
            Self::notify_panel(&k, session_id.as_deref(), "assistant").await;
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

    #[test]
    fn stored_non_answers_are_not_replayable() {
        use crate::kernel::{is_unreplayable_assistant_turn as bad, EMPTY_LLM_ANSWER_PLACEHOLDER};
        assert!(bad("   "));
        assert!(bad(EMPTY_LLM_ANSWER_PLACEHOLDER));
        assert!(bad(&format!(
            "{EMPTY_LLM_ANSWER_PLACEHOLDER}\n\n[Note: limit]"
        )));
        assert!(bad("(turn failed: provider timeout)"));
        assert!(bad(
            "analysisUser wants audio.assistantcommentary to=functions.audio json{\"action\":\"list\"}"
        ));
        assert!(bad("We need to check.assistantfinalDone!"));
        assert!(!bad("Hey! What would you like to do next?"));
        assert!(!bad("The audio file has been played. Enjoy!"));
    }

    /// The whole point of the guard: a dropped `JoinHandle` detaches its task,
    /// so without `Drop` an early `return Err(..)` would leave the keepalive
    /// pinging the channel forever about a turn that already ended.
    #[tokio::test]
    async fn abort_on_drop_stops_the_keepalive() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pings = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&pings);
        let guard = AbortOnDrop(tokio::spawn(async move {
            loop {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }));

        tokio::time::sleep(Duration::from_millis(40)).await;
        let while_running = pings.load(Ordering::SeqCst);
        assert!(while_running > 0, "keepalive never ran");

        drop(guard);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            pings.load(Ordering::SeqCst),
            while_running,
            "keepalive kept running after the guard dropped"
        );
    }

    /// A `DeliveryAdapter` whose only job is to count indicator pings.
    struct CountingAdapter {
        instance_id: String,
        pings: Arc<std::sync::atomic::AtomicUsize>,
        /// When set, every ping fails — used to prove the keepalive gives up.
        fail: bool,
    }

    #[async_trait::async_trait]
    impl crate::notification_router::DeliveryAdapter for CountingAdapter {
        fn channel_id(&self) -> agentos_types::DeliveryChannel {
            agentos_types::DeliveryChannel::custom("telegram".to_string())
        }
        async fn deliver(
            &self,
            _msg: &agentos_types::UserMessage,
        ) -> Result<(), crate::notification_router::DeliveryError> {
            Ok(())
        }
        async fn is_available(&self) -> bool {
            true
        }
        fn adapter_instance_id(&self) -> Option<String> {
            Some(self.instance_id.clone())
        }
        async fn typing(&self) -> Result<(), crate::notification_router::DeliveryError> {
            self.pings.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                return Err(crate::notification_router::DeliveryError("nope".into()));
            }
            Ok(())
        }
    }

    async fn router_with(
        adapter: Arc<CountingAdapter>,
    ) -> Arc<crate::notification_router::NotificationRouter> {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = Arc::new(
            crate::user_inbox::UserInbox::new(&dir.path().join("inbox.db"), 100).expect("inbox"),
        );
        let audit =
            Arc::new(agentos_audit::AuditLog::open(&dir.path().join("audit.db")).expect("audit"));
        Box::leak(Box::new(dir));
        let router = Arc::new(crate::notification_router::NotificationRouter::new(
            inbox, audit,
        ));
        router.register_adapter(adapter).await;
        router
    }

    /// The load-bearing claim behind moving the spawn ahead of `load_session`:
    /// the indicator must be up *before* the first `chat.db` round-trip, which
    /// only holds if `interval`'s first tick fires immediately. If it did not,
    /// every turn would start with a full interval of silence and no other test
    /// in this file would notice.
    #[tokio::test(start_paused = true)]
    async fn typing_indicator_pings_immediately_then_every_interval() {
        let pings = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = Arc::new(CountingAdapter {
            instance_id: "chan-1".to_string(),
            pings: Arc::clone(&pings),
            fail: false,
        });
        let router = router_with(adapter).await;

        let _guard = KernelChatBridge::spawn_typing_indicator(router, "chan-1".to_string());

        tokio::task::yield_now().await;
        assert_eq!(
            pings.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "first tick must be immediate — the indicator has to beat the session load"
        );

        tokio::time::advance(Duration::from_secs(TYPING_PING_INTERVAL_SECS)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            pings.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "indicator must refresh once per interval or it expires mid-turn"
        );
    }

    /// A ping that fails will keep failing for the rest of the turn (revoked
    /// token, blocked bot, flood control). Retrying it ~165 times is what
    /// lengthens a flood ban on the token that also delivers the replies.
    #[tokio::test(start_paused = true)]
    async fn typing_indicator_gives_up_after_a_failed_ping() {
        let pings = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = Arc::new(CountingAdapter {
            instance_id: "chan-1".to_string(),
            pings: Arc::clone(&pings),
            fail: true,
        });
        let router = router_with(adapter).await;

        let _guard = KernelChatBridge::spawn_typing_indicator(router, "chan-1".to_string());

        tokio::task::yield_now().await;
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(TYPING_PING_INTERVAL_SECS)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(
            pings.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "keepalive kept pinging an adapter that already rejected it"
        );
    }

    /// An unknown channel — every `ChannelManager`-stack kind (Discord, Slack,
    /// WhatsApp…) — must not leave a loop spinning for the whole turn.
    #[tokio::test(start_paused = true)]
    async fn typing_indicator_stops_when_no_adapter_owns_the_channel() {
        let pings = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = Arc::new(CountingAdapter {
            instance_id: "chan-1".to_string(),
            pings: Arc::clone(&pings),
            fail: false,
        });
        let router = router_with(adapter).await;

        let _guard =
            KernelChatBridge::spawn_typing_indicator(router, "some-other-chan".to_string());

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(TYPING_PING_INTERVAL_SECS * 4)).await;
        tokio::task::yield_now().await;
        assert_eq!(pings.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
