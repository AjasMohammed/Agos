use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_capability::{PERM_USER_INTERACT, PERM_USER_NOTIFY};
use agentos_types::{
    AgentID, AgentOSError, DeliveryChannel, NotificationID, NotificationPriority,
    NotificationSource, PermissionOp, TraceID, UserMessage, UserMessageKind, UserResponse,
};
use chrono::Utc;

impl Kernel {
    /// Send a fire-and-forget notification to the user inbox.
    ///
    /// Validates `user.notify` (write) permission for the originating agent.
    pub(crate) async fn cmd_send_user_notification(
        &self,
        subject: String,
        body: String,
        priority: NotificationPriority,
        kind: Option<UserMessageKind>,
        trace_id: TraceID,
        from_agent: Option<AgentID>,
    ) -> KernelResponse {
        // Permission check for agent-sourced notifications.
        if let Some(agent_id) = from_agent {
            let registry = self.agent_registry.read().await;
            let profile = registry.get_by_id(&agent_id);
            let allowed = profile
                .map(|p| p.permissions.check(PERM_USER_NOTIFY, PermissionOp::Write))
                .unwrap_or(false);
            if !allowed {
                return KernelResponse::Error {
                    message: format!(
                        "Agent {agent_id} requires '{PERM_USER_NOTIFY}:w' permission to send notifications"
                    ),
                };
            }
        }

        let from = match from_agent {
            Some(id) => NotificationSource::Agent(id),
            None => NotificationSource::Kernel,
        };

        let msg = UserMessage {
            actions: Vec::new(),
            id: NotificationID::new(),
            from,
            task_id: None,
            trace_id,
            kind: kind.unwrap_or(UserMessageKind::Notification),
            priority,
            subject: subject.chars().take(80).collect(),
            body,
            interaction: None,
            delivery_status: std::collections::HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: None,
            reply_to_external_id: None,
            attachment: None,
        };

        let notification_id = msg.id;

        match self.notification_router.deliver(msg).await {
            Ok(_) => {
                self.audit_log(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: AuditEventType::NotificationSent,
                    agent_id: from_agent,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "notification_id": notification_id.to_string(),
                        "priority": priority.to_string(),
                        "subject": subject,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::NotificationSent {
                    id: notification_id,
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to send notification: {e}"),
            },
        }
    }

    /// Fetch a single notification by ID.
    pub(crate) async fn cmd_get_notification(
        &self,
        notification_id: NotificationID,
    ) -> KernelResponse {
        match self.notification_router.inbox().get(&notification_id).await {
            Ok(msg_opt) => KernelResponse::NotificationDetail(Box::new(msg_opt)),
            Err(e) => KernelResponse::Error {
                message: format!("Failed to fetch notification: {e}"),
            },
        }
    }

    /// List notifications from the user inbox.
    pub(crate) async fn cmd_list_notifications(
        &self,
        unread_only: bool,
        limit: u32,
    ) -> KernelResponse {
        match self
            .notification_router
            .inbox()
            .list(unread_only, limit as usize)
            .await
        {
            Ok(msgs) => KernelResponse::NotificationList(msgs),
            Err(e) => KernelResponse::Error {
                message: format!("Failed to list notifications: {e}"),
            },
        }
    }

    /// Mark a notification as read and audit it.
    pub(crate) async fn cmd_mark_notification_read(&self, id: NotificationID) -> KernelResponse {
        match self.notification_router.inbox().mark_read(&id).await {
            Ok(_) => {
                self.audit_log(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: AuditEventType::NotificationRead,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "notification_id": id.to_string(),
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to mark notification as read: {e}"),
            },
        }
    }

    /// Submit a user response to an interactive (Question) notification.
    ///
    /// Validation (exists, is a Question, not already responded) is performed
    /// atomically inside `route_response` / `UserInbox::set_response` — no
    /// duplicate pre-checks here to avoid widening the TOCTOU window.
    pub(crate) async fn cmd_respond_to_notification(
        &self,
        notification_id: NotificationID,
        response_text: String,
        channel: DeliveryChannel,
    ) -> KernelResponse {
        // Fetch task_id for the audit entry; non-fatal if not found (route_response
        // will return the authoritative error if the notification doesn't exist).
        let task_id = self
            .notification_router
            .inbox()
            .get(&notification_id)
            .await
            .ok()
            .flatten()
            .and_then(|m| m.task_id);

        let response = UserResponse {
            text: response_text.clone(),
            responded_at: Utc::now(),
            channel: channel.clone(),
        };

        match self
            .notification_router
            .route_response(notification_id, response)
            .await
        {
            Ok(()) => {
                self.audit_log(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: AuditEventType::UserResponseReceived,
                    agent_id: None,
                    task_id,
                    tool_id: None,
                    details: serde_json::json!({
                        "notification_id": notification_id.to_string(),
                        "channel": channel.to_string(),
                        "response_length": response_text.len(),
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "notification_id": notification_id.to_string(),
                        "status": "response_routed",
                    })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to route response: {e}"),
            },
        }
    }

    /// Validate that an agent has `user.interact` (execute) permission.
    ///
    /// Used by the `ask-user` tool (Phase 3) before creating a blocking question.
    #[allow(dead_code)]
    pub(crate) async fn check_user_interact_permission(
        &self,
        agent_id: AgentID,
    ) -> Result<(), AgentOSError> {
        let registry = self.agent_registry.read().await;
        let allowed = registry
            .get_by_id(&agent_id)
            .map(|p| {
                p.permissions
                    .check(PERM_USER_INTERACT, PermissionOp::Execute)
            })
            .unwrap_or(false);
        if !allowed {
            return Err(AgentOSError::PermissionDenied {
                resource: PERM_USER_INTERACT.to_string(),
                operation: "execute".to_string(),
            });
        }
        Ok(())
    }
    // ── Notification routing matrix ─────────────────────────────────────────

    /// Read the routing matrix: axes, current rules, and panel presence.
    pub(crate) async fn cmd_get_notification_routes(&self) -> KernelResponse {
        let rules: Vec<serde_json::Value> = self
            .notification_routes
            .rules()
            .into_iter()
            .map(|(event, channel, mode)| {
                serde_json::json!({
                    "event": event.as_str(),
                    "channel": channel,
                    "mode": mode.as_str(),
                })
            })
            .collect();
        let channels: Vec<serde_json::Value> = self
            .notification_router
            .adapter_targets()
            .await
            .into_iter()
            .map(|(key, kind, available)| {
                serde_json::json!({ "key": key, "kind": kind, "available": available })
            })
            .collect();
        KernelResponse::Success {
            data: Some(serde_json::json!({
                "events": agentos_types::NotificationEvent::ALL
                    .iter()
                    .map(|e| e.as_str())
                    .collect::<Vec<_>>(),
                "channels": channels,
                "rules": rules,
                "panel_connected": self.notification_routes.panel_connected(),
            })),
        }
    }

    /// Set one cell. Unknown event/mode strings are rejected rather than
    /// silently dropped — a typo must not look like a saved rule.
    pub(crate) async fn cmd_set_notification_route(
        &self,
        event: String,
        channel: String,
        mode: String,
    ) -> KernelResponse {
        let Some(parsed_event) = agentos_types::NotificationEvent::parse(&event) else {
            return KernelResponse::Error {
                message: format!(
                    "Unknown notification event '{event}' — expected one of: {}",
                    agentos_types::NotificationEvent::ALL
                        .iter()
                        .map(|e| e.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
        };
        let Some(parsed_mode) = crate::notification_routes::RouteMode::parse(&mode) else {
            return KernelResponse::Error {
                message: format!(
                    "Unknown route mode '{mode}' — expected always, never, or when_away"
                ),
            };
        };
        if channel.trim().is_empty() {
            return KernelResponse::Error {
                message: "Channel must not be empty".to_string(),
            };
        }

        match self
            .notification_routes
            .set(parsed_event, channel.trim(), parsed_mode)
            .await
        {
            Ok(()) => {
                self.audit_log(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: AuditEventType::KernelConfigChanged,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "source": "notification_routes",
                        "event": parsed_event.as_str(),
                        "channel": channel.trim(),
                        "mode": parsed_mode.as_str(),
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "event": parsed_event.as_str(),
                        "channel": channel.trim(),
                        "mode": parsed_mode.as_str(),
                    })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to save notification route: {e}"),
            },
        }
    }
}
