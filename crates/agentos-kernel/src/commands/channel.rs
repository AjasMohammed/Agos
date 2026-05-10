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
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn cmd_connect_channel(
        &self,
        kind: ChannelKind,
        external_id: String,
        display_name: String,
        credential_key: String,
        reply_topic: Option<String>,
        server_url: Option<String>,
        webhook_url: Option<String>,
        active_agent_name: Option<String>,
    ) -> KernelResponse {
        let now = Utc::now();
        let active_agent_name = active_agent_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let ch = RegisteredChannel {
            id: ChannelInstanceID::new(),
            kind: kind.clone(),
            external_id: external_id.clone(),
            display_name: display_name.clone(),
            credential_key: credential_key.clone(),
            reply_topic: reply_topic.clone(),
            server_url: server_url.clone(),
            webhook_url: webhook_url.clone(),
            active_agent_name,
            connected_at: now,
            last_active: now,
            active: true,
        };
        let ch_id = ch.id;

        if let Some(ref n) = ch.active_agent_name {
            let reg = self.agent_registry.read().await;
            if reg.get_by_name(n).is_none() {
                return KernelResponse::Error {
                    message: format!(
                        "Unknown agent '{n}' — omit --active-agent or use an existing agent name."
                    ),
                };
            }
        }

        // Persist to registry.
        if let Err(e) = self.channel_registry.register(ch).await {
            return KernelResponse::Error {
                message: format!("Failed to register channel: {e}"),
            };
        }
        // Refresh the agent-facing snapshot so subsequent tasks see the new channel.
        self.refresh_connected_channels_snapshot().await;

        // Build and register the delivery adapter.
        let adapter_result = self
            .build_channel_adapter(
                &kind,
                &external_id,
                &credential_key,
                &reply_topic,
                &server_url,
                &webhook_url,
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

        let status_msg = if external_id.is_empty() {
            "Channel connected (waiting for /start — send a message to the bot to complete setup)"
        } else {
            "Channel connected successfully"
        };

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "channel_id": ch_id.to_string(),
                "kind": kind.to_string(),
                "display_name": display_name,
                "message": status_msg,
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

        let channel = match self.channel_registry.get_by_id(&id).await {
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
            Ok(Some(ch)) => ch,
        };

        // If this was a webhook-mode Telegram channel, delete the webhook
        // and remove the stored secret.
        if channel.kind == ChannelKind::Telegram && channel.webhook_url.is_some() {
            if let Ok(bot_token) = self.vault.get(&channel.credential_key).await {
                let adapter = crate::adapters::telegram::TelegramDeliveryAdapter::new(
                    bot_token.as_str().to_string(),
                    String::new(),
                    id,
                    None,
                );
                if let Err(e) = adapter.delete_webhook().await {
                    tracing::warn!(error = %e, "Failed to delete Telegram webhook on disconnect");
                }
            }
            self.webhook_secrets.write().await.remove(&id);
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
        self.refresh_connected_channels_snapshot().await;

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

    pub(crate) async fn cmd_set_channel_active_agent(
        &self,
        channel_id: String,
        agent_name: Option<String>,
    ) -> KernelResponse {
        let id: ChannelInstanceID = match channel_id.parse() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid channel ID: '{channel_id}'"),
                }
            }
        };

        if self
            .channel_registry
            .get_by_id(&id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            return KernelResponse::Error {
                message: format!("Channel '{channel_id}' not found"),
            };
        }

        let normalized = agent_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(ref name) = normalized {
            let reg = self.agent_registry.read().await;
            if reg.get_by_name(name).is_none() {
                return KernelResponse::Error {
                    message: format!("Unknown agent '{name}'"),
                };
            }
        }

        if let Err(e) = self
            .channel_registry
            .update_active_agent_name(&id, normalized.as_deref())
            .await
        {
            return KernelResponse::Error {
                message: format!("Failed to update channel: {e}"),
            };
        }

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "channel_id": channel_id,
                "active_agent_name": normalized,
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build_channel_adapter(
        &self,
        kind: &ChannelKind,
        external_id: &str,
        credential_key: &str,
        reply_topic: &Option<String>,
        server_url: &Option<String>,
        webhook_url: &Option<String>,
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

                // When external_id (chat_id) is empty, enable auto-discovery:
                // the adapter will capture the chat_id from the first inbound
                // message and notify us so we can persist it in the registry.
                let discovery_tx = if external_id.is_empty() {
                    let (tx, mut rx) =
                        tokio::sync::mpsc::channel::<crate::adapters::telegram::ChatDiscovered>(1);
                    let registry = self.channel_registry.clone();
                    tokio::spawn(async move {
                        if let Some(ev) = rx.recv().await {
                            if let Err(e) = registry
                                .update_external_id(&ev.channel_instance_id, &ev.chat_id)
                                .await
                            {
                                tracing::warn!(
                                    error = %e,
                                    "Failed to persist auto-discovered Telegram chat_id"
                                );
                            }
                        }
                    });
                    Some(tx)
                } else {
                    None
                };

                let mut adapter = crate::adapters::telegram::TelegramDeliveryAdapter::new(
                    bot_token.as_str().to_string(),
                    external_id.to_string(),
                    channel_instance_id,
                    discovery_tx,
                );

                // Webhook mode: call setWebhook and store the secret for the API handler.
                if let Some(wh_url) = webhook_url {
                    // Generate a 64-char hex secret from two UUIDs (no extra rand dependency).
                    let secret = format!(
                        "{}{}",
                        uuid::Uuid::new_v4().as_simple(),
                        uuid::Uuid::new_v4().as_simple()
                    );

                    let full_url = format!(
                        "{}/api/v1/webhooks/telegram/{channel_instance_id}",
                        wh_url.trim_end_matches('/')
                    );
                    adapter
                        .register_webhook(&full_url, &secret)
                        .await
                        .map_err(|e| format!("Failed to register Telegram webhook: {e}"))?;
                    adapter.set_webhook_mode();

                    self.webhook_secrets
                        .write()
                        .await
                        .insert(channel_instance_id, secret);

                    tracing::info!(
                        channel_id = %channel_instance_id,
                        url = %full_url,
                        "Telegram webhook registered"
                    );
                }

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
            ChannelKind::Discord
            | ChannelKind::Slack
            | ChannelKind::WhatsApp
            | ChannelKind::Webhook => {
                // These channel kinds are handled by agentos-channels ChannelManager,
                // not by the notification-router DeliveryAdapter path.
                Ok(None)
            }
            ChannelKind::Custom(_) => {
                // Custom channel kinds have no built-in adapter.
                Ok(None)
            }
        }
    }
}
