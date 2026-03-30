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
            Ok(()) => {
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
}
