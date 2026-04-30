//! Per-connection WebSocket session — tracks subscriptions and handles frames.

use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::error::ApiError;
use crate::service::KernelService;

use super::broadcaster::WsBroadcaster;
use super::protocol::{ClientFrame, ServerFrame};

/// Per-connection state for a WebSocket client.
pub struct WsSession {
    subscriptions: HashMap<String, String>, // sub_id → channel
    next_sub_id: u64,
    outbound_tx: mpsc::Sender<ServerFrame>,
}

impl WsSession {
    pub fn new(outbound_tx: mpsc::Sender<ServerFrame>) -> Self {
        Self {
            subscriptions: HashMap::new(),
            next_sub_id: 0,
            outbound_tx,
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
        let id = format!("sub_{}", self.next_sub_id);
        self.next_sub_id += 1;
        id
    }
}
