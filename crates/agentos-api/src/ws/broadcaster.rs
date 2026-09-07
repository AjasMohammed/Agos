//! Fan-out from kernel events to subscribed WebSocket sessions.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use super::protocol::ServerFrame;
use crate::util::task_state_str;

/// Entry tracking a single subscription.
struct BroadcastEntry {
    channel: String,
    sender: mpsc::Sender<ServerFrame>,
}

/// Routes kernel events to subscribed WebSocket sessions.
#[derive(Clone)]
pub struct WsBroadcaster {
    subscriptions: Arc<RwLock<HashMap<String, BroadcastEntry>>>,
}

impl Default for WsBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl WsBroadcaster {
    pub fn new() -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a subscription. Events matching `channel` will be forwarded to `sender`.
    pub async fn register(
        &self,
        sub_id: String,
        channel: String,
        sender: mpsc::Sender<ServerFrame>,
    ) {
        self.subscriptions
            .write()
            .await
            .insert(sub_id, BroadcastEntry { channel, sender });
    }

    /// Remove a subscription.
    pub async fn unregister(&self, sub_id: &str) {
        self.subscriptions.write().await.remove(sub_id);
    }

    /// Remove all subscriptions whose sender has been dropped.
    pub async fn remove_all_for_sender(&self, sub_ids: &[String]) {
        let mut subs = self.subscriptions.write().await;
        for id in sub_ids {
            subs.remove(id);
        }
    }

    /// Broadcast an event to all subscribers matching the channel.
    pub async fn broadcast(&self, channel: &str, event_name: &str, data: serde_json::Value) {
        let subs = self.subscriptions.read().await;
        let mut dead = Vec::new();

        for (sub_id, entry) in subs.iter() {
            if channel_matches(&entry.channel, channel) {
                let frame = ServerFrame::Event {
                    channel: channel.to_string(),
                    event: event_name.to_string(),
                    data: data.clone(),
                };
                match entry.sender.try_send(frame) {
                    Ok(()) => {}
                    // Channel closed — the connection is gone; reap it.
                    Err(mpsc::error::TrySendError::Closed(_)) => dead.push(sub_id.clone()),
                    // Channel full — the consumer is momentarily slow. Shed *this*
                    // event rather than disconnecting (the per-connection channel is
                    // bounded at 256; a transient burst must not kill the client).
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::debug!(
                            sub_id = %sub_id,
                            channel = %channel,
                            "WS subscriber lagging; dropping event (connection kept)"
                        );
                    }
                }
            }
        }

        drop(subs);

        if !dead.is_empty() {
            let mut subs = self.subscriptions.write().await;
            for id in dead {
                subs.remove(&id);
            }
        }
    }

    /// Start a background task that reads `StatusUpdate` messages from the kernel
    /// and broadcasts them as task events.
    pub fn start_status_relay(
        self,
        mut status_rx: tokio::sync::broadcast::Receiver<agentos_bus::StatusUpdate>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match status_rx.recv().await {
                    Ok(update) => {
                        let state = task_state_str(&update.state);
                        let data = serde_json::json!({
                            "task_id": update.task_id.to_string(),
                            "state": state,
                            "message": update.message,
                        });
                        let event_name = format!("task.{state}");
                        self.broadcast("tasks", &event_name, data.clone()).await;

                        // Also broadcast to the specific task channel
                        let task_channel = format!("tasks:{}", update.task_id);
                        self.broadcast(&task_channel, &event_name, data).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "WsBroadcaster lagged on status updates");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("Status update channel closed, stopping WS broadcaster");
                        break;
                    }
                }
            }
        })
    }

    /// Start a background task that reads coarse `RealtimeEvent`s from the kernel
    /// and fans them out to their channel (`agents`/`audit`/`schedules`/`system`/
    /// `events`, plus `tasks` from non-status sources). Complements
    /// [`Self::start_status_relay`], which carries the richer task-status stream.
    pub fn start_realtime_relay(
        self,
        mut rx: tokio::sync::broadcast::Receiver<agentos_types::RealtimeEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        self.broadcast(&ev.channel, &ev.event, ev.data).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "WsBroadcaster lagged on realtime events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("Realtime event channel closed, stopping relay");
                        break;
                    }
                }
            }
        })
    }
}

/// Check if a subscribed channel matches an event channel.
///
/// - Exact match: "tasks" matches "tasks"
/// - Prefix match: "tasks" matches "tasks:abc-123"
fn channel_matches(subscribed: &str, event_channel: &str) -> bool {
    if subscribed == event_channel {
        return true;
    }
    // "tasks" matches "tasks:abc-123"
    event_channel.starts_with(subscribed)
        && event_channel.as_bytes().get(subscribed.len()) == Some(&b':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_matching() {
        assert!(channel_matches("tasks", "tasks"));
        assert!(channel_matches("tasks", "tasks:abc-123"));
        assert!(!channel_matches("tasks", "tasks_extra"));
        assert!(!channel_matches("tasks", "task"));
        assert!(channel_matches("agents", "agents"));
        assert!(!channel_matches("agents", "agents2"));
    }
}
