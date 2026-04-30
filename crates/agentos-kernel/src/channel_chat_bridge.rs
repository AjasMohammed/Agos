//! Bridges external channel inbound chat to `Kernel::chat_infer_with_tools`.
//!
//! `InboundRouter` holds an `Arc<KernelChatBridge>` created before `Kernel` is
//! wrapped in `Arc`. After `Arc::new(kernel)`, call [`Kernel::wire_inbound_chat_bridge`]
//! so [`KernelChatBridge::set_kernel`] can resolve a `Weak<Kernel>` for inference.

use crate::Kernel;
use agentos_types::{AgentID, ChannelInstanceID};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// Max (user, assistant) pairs retained per channel+agent transcript.
const MAX_HISTORY_ROUNDS: usize = 12;
/// Maximum number of distinct (channel, agent) transcripts kept in memory.
/// Oldest entry is evicted when the cap is reached.
const MAX_TRANSCRIPT_ENTRIES: usize = 256;
/// Maximum chars stored per user message or assistant answer in transcript history.
/// Prevents a single large exchange from consuming unbounded heap.
const MAX_MSG_CHARS: usize = 2_000;
/// Per-call inference timeout for channel chat.
const CHANNEL_CHAT_TIMEOUT_SECS: u64 = 60;

type TranscriptKey = (ChannelInstanceID, String);

/// LRU-style transcript store: evicts the oldest entry when the cap is reached.
struct TranscriptStore {
    map: HashMap<TranscriptKey, Vec<(String, String)>>,
    /// Insertion-order queue used for eviction (front = oldest).
    order: VecDeque<TranscriptKey>,
}

impl TranscriptStore {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, key: &TranscriptKey) -> Option<&Vec<(String, String)>> {
        self.map.get(key)
    }

    fn remove(&mut self, key: &TranscriptKey) {
        self.map.remove(key);
        self.order.retain(|k| k != key);
    }

    fn push_pair(&mut self, key: TranscriptKey, user_msg: &str, answer: &str) {
        if !self.map.contains_key(&key) {
            if self.order.len() >= MAX_TRANSCRIPT_ENTRIES {
                if let Some(oldest) = self.order.pop_front() {
                    self.map.remove(&oldest);
                }
            }
            self.order.push_back(key.clone());
        }
        let buf = self.map.entry(key).or_default();
        let user_stored: String = user_msg.chars().take(MAX_MSG_CHARS).collect();
        let answer_stored: String = answer.chars().take(MAX_MSG_CHARS).collect();
        buf.push(("user".into(), user_stored));
        buf.push(("assistant".into(), answer_stored));
        while buf.len() > MAX_HISTORY_ROUNDS * 2 {
            buf.drain(0..2);
        }
    }
}

pub struct KernelChatBridge {
    kernel: Mutex<Option<Weak<Kernel>>>,
    history: tokio::sync::RwLock<TranscriptStore>,
}

impl Default for KernelChatBridge {
    fn default() -> Self {
        Self {
            kernel: Mutex::new(None),
            history: tokio::sync::RwLock::new(TranscriptStore::new()),
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

    /// Run chat inference for a channel message and update rolling history.
    ///
    /// Inference is bounded by [`CHANNEL_CHAT_TIMEOUT_SECS`]; times out with an
    /// error message rather than blocking the InboundRouter indefinitely.
    pub async fn channel_chat(
        &self,
        channel_id: ChannelInstanceID,
        agent_name: &str,
        user_message: &str,
    ) -> Result<String, String> {
        let k = self
            .upgrade_kernel()
            .ok_or_else(|| "Kernel is not ready for channel chat".to_string())?;

        let key = (channel_id, agent_name.to_string());
        let hist: Vec<(String, String)> = {
            let g = self.history.read().await;
            g.get(&key).cloned().unwrap_or_default()
        };

        let result = tokio::time::timeout(
            Duration::from_secs(CHANNEL_CHAT_TIMEOUT_SECS),
            k.chat_infer_with_tools(agent_name, &hist, user_message, None),
        )
        .await
        .map_err(|_| {
            format!("Chat inference timed out after {CHANNEL_CHAT_TIMEOUT_SECS}s — try again")
        })??;

        let answer = result.answer.clone();
        let mut g = self.history.write().await;
        g.push_pair(key, user_message, &answer);
        Ok(answer)
    }

    /// Drop transcript for a channel+agent (e.g. when unsetting active agent).
    pub async fn clear_history(&self, channel_id: ChannelInstanceID, agent_name: &str) {
        let mut g = self.history.write().await;
        g.remove(&(channel_id, agent_name.to_string()));
    }
}
