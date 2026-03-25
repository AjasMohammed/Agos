use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::{
    ChannelInstanceID, ChannelKind, NotificationID, NotificationPriority, NotificationSource,
    RegisteredChannel, TraceID, UserMessage, UserMessageKind,
};
use chrono::Utc;
use std::sync::Arc;

impl Kernel {
    /// Register a new bidirectional channel and start its listener.
    pub(crate) async fn cmd_connect_channel(
        &self,
        kind: ChannelKind,
        external_id: String,
        display_name: String,
        credential_key: String,
        reply_topic: Option<String>,
        server_url: Option<String>,
    ) -> KernelResponse {
        let now = Utc::now();
        let ch = RegisteredChannel {
            id: ChannelInstanceID::new(),
            kind: kind.clone(),
            external_id: external_id.clone(),
            display_name: display_name.clone(),
            credential_key: credential_key.clone(),
            reply_topic: reply_topic.clone(),
            server_url: server_url.clone(),
            connected_at: now,
            last_active: now,
            active: true,
        };
        let ch_id = ch.id;

        // Persist to registry.
        if let Err(e) = self.channel_registry.register(ch).await {
            return KernelResponse::Error {
                message: format!("Failed to register channel: {e}"),
            };
        }

        // Build and register the delivery adapter.
        let adapter_result = self
            .build_channel_adapter(
                &kind,
                &external_id,
                &credential_key,
                &reply_topic,
                &server_url,
                ch_id,
            )
            .await;

        match adapter_result {
            Ok(Some(adapter)) => {
                let adapter: Arc<dyn crate::notification_router::DeliveryAdapter> =
                    Arc::from(adapter);
                // Register with NotificationRouter for outbound delivery.
                self.notification_router
                    .register_adapter(adapter.clone())
                    .await;
                // Start the inbound listener (no-op for outbound-only adapters).
                self.channel_listener_registry
                    .start(ch_id, adapter, self.inbound_tx.clone())
                    .await;
            }
            Ok(None) => {
                // No adapter available (e.g. email stub).
                tracing::info!(
                    channel_id = %ch_id,
                    kind = %kind,
                    "Channel registered but no runtime adapter available for this kind"
                );
            }
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to build channel adapter: {e}"),
                };
            }
        }

        let _ = self.audit.append(AuditEntry {
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::ChannelConnected,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "channel_id": ch_id.to_string(),
                "kind": kind.to_string(),
                "display_name": display_name,
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "channel_id": ch_id.to_string(),
                "kind": kind.to_string(),
                "display_name": display_name,
                "message": "Channel connected successfully",
            })),
        }
    }

    /// Deregister a channel and stop its listener.
    pub(crate) async fn cmd_disconnect_channel(&self, channel_id: String) -> KernelResponse {
        let id: ChannelInstanceID = match channel_id.parse() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid channel ID: '{channel_id}'"),
                }
            }
        };

        match self.channel_registry.get_by_id(&id).await {
            Ok(None) => {
                return KernelResponse::Error {
                    message: format!("Channel '{channel_id}' not found"),
                }
            }
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to look up channel: {e}"),
                }
            }
            Ok(Some(_)) => {}
        }

        self.channel_listener_registry.stop(&id).await;
        // Remove the delivery adapter from NotificationRouter so outbound deliveries
        // stop and the adapter Vec doesn't grow unboundedly on repeated connect/disconnect.
        self.notification_router
            .deregister_adapter(&id.to_string())
            .await;

        if let Err(e) = self.channel_registry.deregister(&id).await {
            return KernelResponse::Error {
                message: format!("Failed to deregister channel: {e}"),
            };
        }

        let _ = self.audit.append(AuditEntry {
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::ChannelDisconnected,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "channel_id": channel_id }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "message": format!("Channel '{channel_id}' disconnected"),
            })),
        }
    }

    /// Return all registered channels.
    pub(crate) async fn cmd_list_channels(&self) -> KernelResponse {
        match self.channel_registry.list_active().await {
            Ok(channels) => KernelResponse::ChannelList(channels),
            Err(e) => KernelResponse::Error {
                message: format!("Failed to list channels: {e}"),
            },
        }
    }

    /// Send a test notification to a registered channel.
    pub(crate) async fn cmd_test_channel(&self, channel_id: String) -> KernelResponse {
        let id: ChannelInstanceID = match channel_id.parse() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid channel ID: '{channel_id}'"),
                }
            }
        };

        match self.channel_registry.get_by_id(&id).await {
            Ok(None) => {
                return KernelResponse::Error {
                    message: format!("Channel '{channel_id}' not found"),
                }
            }
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to look up channel: {e}"),
                }
            }
            Ok(Some(_)) => {}
        }

        let test_msg = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Kernel,
            task_id: None,
            trace_id: TraceID::new(),
            kind: UserMessageKind::Notification,
            priority: NotificationPriority::Info,
            subject: "AgentOS test notification".to_string(),
            body: "This is a test notification from AgentOS to verify your channel is working."
                .to_string(),
            interaction: None,
            delivery_status: Default::default(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: Some(format!("channel:{id}")),
            reply_to_external_id: None,
        };

        match self.notification_router.deliver(test_msg).await {
            Ok(_) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "message": "Test notification delivered",
                })),
            },
            Err(e) => KernelResponse::Error {
                message: format!("Test notification failed: {e}"),
            },
        }
    }

    /// Build a `DeliveryAdapter` for the given channel kind.
    pub(crate) async fn build_channel_adapter(
        &self,
        kind: &ChannelKind,
        external_id: &str,
        credential_key: &str,
        reply_topic: &Option<String>,
        server_url: &Option<String>,
        channel_instance_id: ChannelInstanceID,
    ) -> Result<Option<Box<dyn crate::notification_router::DeliveryAdapter>>, String> {
        match kind {
            ChannelKind::Telegram => {
                // Retrieve bot token from vault using the credential_key.
                if credential_key.is_empty() {
                    return Err(
                        "Telegram channel requires a bot token stored in vault (credential_key)"
                            .to_string(),
                    );
                }
                let bot_token = self
                    .vault
                    .get(credential_key)
                    .await
                    .map_err(|e| format!("Failed to retrieve bot token from vault: {e}"))?;
                let adapter = crate::adapters::telegram::TelegramDeliveryAdapter::new(
                    bot_token.as_str().to_string(),
                    external_id.to_string(),
                    channel_instance_id,
                );
                Ok(Some(Box::new(adapter)))
            }
            ChannelKind::Ntfy => {
                let surl = server_url
                    .clone()
                    .unwrap_or_else(|| "https://ntfy.sh".to_string());
                crate::network_safety::validate_server_url(&surl)
                    .map_err(|e| format!("Invalid ntfy server URL: {e}"))?;
                let rtopic = reply_topic
                    .clone()
                    .unwrap_or_else(|| format!("{external_id}-reply"));
                let access_token = if credential_key.is_empty() {
                    None
                } else {
                    Some(
                        self.vault
                            .get(credential_key)
                            .await
                            .map_err(|e| format!("Failed to retrieve ntfy token from vault: {e}"))?
                            .as_str()
                            .to_string(),
                    )
                };
                let adapter = crate::adapters::ntfy::NtfyDeliveryAdapter::new(
                    surl,
                    external_id.to_string(),
                    rtopic,
                    access_token,
                    channel_instance_id,
                );
                Ok(Some(Box::new(adapter)))
            }
            ChannelKind::Email => {
                // Email adapter is stubbed — register it but it won't deliver.
                let adapter = crate::adapters::email::EmailDeliveryAdapter;
                Ok(Some(Box::new(adapter)))
            }
            ChannelKind::Custom(_) => {
                // Custom channel kinds have no built-in adapter.
                Ok(None)
            }
        }
    }
}
