//! Per-connection WebSocket session — tracks subscriptions and handles frames.

use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::error::ApiError;
use crate::service::KernelService;

use super::broadcaster::WsBroadcaster;
use super::protocol::{ClientFrame, ServerFrame};

/// Maximum channels a single connection may subscribe to (fail-closed against
/// fan-out abuse / memory growth).
const MAX_SUBSCRIPTIONS: usize = 64;

/// Per-connection state for a WebSocket client.
pub struct WsSession {
    subscriptions: HashMap<String, String>, // sub_id → channel
    /// Per-connection unique prefix so subscription IDs never collide with those
    /// of another connection in the process-wide broadcaster map.
    connection_id: String,
    next_sub_id: u64,
    outbound_tx: mpsc::Sender<ServerFrame>,
    /// Scopes of the API key that authenticated this connection (e.g. `"audit:r"`,
    /// `"*:rw"`). Empty = full access (bootstrap key). Used to gate `subscribe`.
    permissions: Vec<String>,
}

impl WsSession {
    pub fn new(outbound_tx: mpsc::Sender<ServerFrame>, permissions: Vec<String>) -> Self {
        Self {
            subscriptions: HashMap::new(),
            connection_id: uuid::Uuid::new_v4().to_string(),
            next_sub_id: 0,
            outbound_tx,
            permissions,
        }
    }

    /// All subscription IDs owned by this session (for cleanup on disconnect).
    pub fn subscription_ids(&self) -> Vec<String> {
        self.subscriptions.keys().cloned().collect()
    }

    /// Process an incoming client frame.
    pub async fn handle_frame(
        &mut self,
        frame: ClientFrame,
        service: &dyn KernelService,
        broadcaster: &WsBroadcaster,
    ) {
        match frame {
            ClientFrame::Subscribe { channel, .. } => {
                // Cap per-connection subscriptions.
                if self.subscriptions.len() >= MAX_SUBSCRIPTIONS {
                    let _ = self
                        .send(ServerFrame::Error {
                            code: "SUBSCRIPTION_LIMIT".into(),
                            message: format!("Subscription limit ({MAX_SUBSCRIPTIONS}) reached"),
                        })
                        .await;
                    return;
                }
                // Scope check: subscribing to a channel requires the matching read
                // scope (e.g. `audit` needs `audit:r`). Empty key permissions =
                // full access (bootstrap). Mirrors REST `require_permission`.
                let required = channel_required_scope(&channel);
                if !permissions_grant(&self.permissions, &required) {
                    let _ = self
                        .send(ServerFrame::Error {
                            code: "FORBIDDEN".into(),
                            message: format!(
                                "Missing permission '{required}' for channel '{channel}'"
                            ),
                        })
                        .await;
                    return;
                }
                let sub_id = self.alloc_sub_id();
                self.subscriptions.insert(sub_id.clone(), channel.clone());
                broadcaster
                    .register(sub_id.clone(), channel.clone(), self.outbound_tx.clone())
                    .await;
                let _ = self
                    .send(ServerFrame::Subscribed {
                        channel,
                        subscription_id: sub_id,
                    })
                    .await;
            }

            ClientFrame::Unsubscribe { subscription_id } => {
                if self.subscriptions.remove(&subscription_id).is_some() {
                    broadcaster.unregister(&subscription_id).await;
                    let _ = self
                        .send(ServerFrame::Unsubscribed { subscription_id })
                        .await;
                } else {
                    let _ = self
                        .send(ServerFrame::Error {
                            code: "UNKNOWN_SUBSCRIPTION".into(),
                            message: format!("No subscription '{subscription_id}'"),
                        })
                        .await;
                }
            }

            ClientFrame::ChatSend {
                session_id,
                message,
                agent_name,
            } => {
                // Non-streaming for now — send the full response as ChatDone.
                let req = crate::types::ChatRequest {
                    session_id: session_id.clone(),
                    agent_name,
                    message,
                    history: Vec::new(),
                    parts: Vec::new(),
                };
                match service.chat_send(req).await {
                    Ok(resp) => {
                        let _ = self
                            .send(ServerFrame::ChatDone {
                                session_id,
                                tool_calls: resp.tool_calls,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = self
                            .send(ServerFrame::Error {
                                code: e.error_code().to_string(),
                                message: e.to_string(),
                            })
                            .await;
                    }
                }
            }

            ClientFrame::ChatCancel { session_id } => {
                // Cancellation not yet wired — acknowledge and move on.
                let _ = self.send(ServerFrame::ChatCancelled { session_id }).await;
            }

            ClientFrame::TaskCancel { task_id } => {
                let parsed: Result<agentos_types::TaskID, _> = task_id.parse();
                match parsed {
                    Ok(id) => match service.cancel_task(id).await {
                        Ok(()) => {
                            let _ = self
                                .send(ServerFrame::Event {
                                    channel: "tasks".into(),
                                    event: "task.cancelled".into(),
                                    data: serde_json::json!({ "task_id": task_id }),
                                })
                                .await;
                        }
                        Err(e) => {
                            let _ = self
                                .send(ServerFrame::Error {
                                    code: e.error_code().to_string(),
                                    message: e.to_string(),
                                })
                                .await;
                        }
                    },
                    Err(_) => {
                        let _ = self
                            .send(ServerFrame::Error {
                                code: "BAD_REQUEST".into(),
                                message: format!("Invalid task ID: {task_id}"),
                            })
                            .await;
                    }
                }
            }

            ClientFrame::NotificationRespond { id, text } => {
                let parsed: Result<agentos_types::NotificationID, _> = id.parse();
                match parsed {
                    Ok(nid) => match service.respond_to_notification(nid, text).await {
                        Ok(()) => {
                            let _ = self
                                .send(ServerFrame::Event {
                                    channel: "notifications".into(),
                                    event: "notification.responded".into(),
                                    data: serde_json::json!({ "id": id }),
                                })
                                .await;
                        }
                        Err(e) => {
                            let _ = self
                                .send(ServerFrame::Error {
                                    code: e.error_code().to_string(),
                                    message: e.to_string(),
                                })
                                .await;
                        }
                    },
                    Err(_) => {
                        let _ = self
                            .send(ServerFrame::Error {
                                code: "BAD_REQUEST".into(),
                                message: format!("Invalid notification ID: {id}"),
                            })
                            .await;
                    }
                }
            }

            ClientFrame::Ping => {
                let _ = self.send(ServerFrame::Pong).await;
            }
        }
    }

    /// Send a frame to the client. Returns Err if the channel is closed.
    async fn send(&self, frame: ServerFrame) -> Result<(), ApiError> {
        self.outbound_tx
            .send(frame)
            .await
            .map_err(|_| ApiError::Internal("WebSocket outbound channel closed".into()))
    }

    fn alloc_sub_id(&mut self) -> String {
        // Prefix with the per-connection id so two connections' counters can
        // never produce the same key in the shared broadcaster map.
        let id = format!("{}:{}", self.connection_id, self.next_sub_id);
        self.next_sub_id += 1;
        id
    }
}

/// The read scope required to subscribe to `channel`. The base (before any
/// `:id` suffix) maps to a `<resource>:r` scope; `agent-chat` maps to `chat`.
/// Shared by the WS subscribe path and the SSE handler so both gate identically.
pub(crate) fn channel_required_scope(channel: &str) -> String {
    let base = channel.split(':').next().unwrap_or(channel);
    let resource = match base {
        "agent-chat" => "chat",
        other => other,
    };
    format!("{resource}:r")
}

/// Whether `permissions` grant `required` (`resource:op`). Empty permissions =
/// full access (bootstrap key). Mirrors `handlers::require_permission`.
fn permissions_grant(permissions: &[String], required: &str) -> bool {
    if permissions.is_empty() {
        return true;
    }
    let req_res = required.split(':').next().unwrap_or(required);
    let req_op = required
        .split(':')
        .nth(1)
        .and_then(|o| o.chars().next())
        .unwrap_or('r');
    permissions.iter().any(|p| {
        let res = p.split(':').next().unwrap_or(p);
        let op = p.split(':').nth(1).unwrap_or("r");
        (res == req_res || res == "*") && op.contains(req_op)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_ids_do_not_collide_across_connections() {
        let (tx_a, _ra) = mpsc::channel(4);
        let (tx_b, _rb) = mpsc::channel(4);
        let mut a = WsSession::new(tx_a, vec![]);
        let mut b = WsSession::new(tx_b, vec![]);
        // Two connections both start their counter at 0; the per-connection UUID
        // prefix is what keeps their sub_ids distinct in the shared broadcaster map.
        let a0 = a.alloc_sub_id();
        let b0 = b.alloc_sub_id();
        assert_ne!(a0, b0, "sub_ids from distinct connections must not collide");
        assert!(a0.starts_with(&a.connection_id));
        assert!(b0.starts_with(&b.connection_id));
        // And they remain unique as each connection allocates more.
        assert_ne!(a.alloc_sub_id(), b.alloc_sub_id());
    }

    #[test]
    fn channel_scope_mapping_matches_ws_and_sse() {
        assert_eq!(channel_required_scope("audit"), "audit:r");
        assert_eq!(channel_required_scope("tasks"), "tasks:r");
        // agent-chat is gated on the `chat` resource…
        assert_eq!(channel_required_scope("agent-chat"), "chat:r");
        // …and a parameterized channel uses its base resource, not the id.
        assert_eq!(channel_required_scope("chat:abc-123"), "chat:r");
        assert_eq!(channel_required_scope("agent-chat:xyz"), "chat:r");
    }
}
