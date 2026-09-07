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
    pub async fn cmd_connect_channel(
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
                if let Err(e) = self.register_channel_manager_adapter(&ch_id).await {
                    return Self::adapter_setup_failed(ch_id, &kind, &display_name, &e);
                }
            }
            Err(e) => {
                return Self::adapter_setup_failed(ch_id, &kind, &display_name, &e);
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

    /// Vault key for a channel that arrives with a secret but no key of its own.
    ///
    /// Two channels named alike would derive the same key and the second token
    /// would silently overwrite the first, so take the next free suffix.
    pub(crate) async fn derive_channel_credential_key(
        &self,
        kind: &ChannelKind,
        display_name: &str,
    ) -> String {
        let slug: String = display_name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let base = format!("channel.{kind}.{}", slug.trim_matches('-'));
        let mut key = base.clone();
        let mut n = 2;
        while self.vault.get(&key).await.is_ok() {
            key = format!("{base}-{n}");
            n += 1;
        }
        key
    }

    /// Put a channel back the way it was after a failed edit.
    ///
    /// An edit tears the live adapter down before it knows the new settings
    /// work; without this, one bad character would leave a working channel
    /// dead, recoverable only by disconnect + reconnect — which mints a new
    /// `ChannelInstanceID` and drops every DM pairing, i.e. exactly the cost
    /// this feature exists to remove. A rotated secret is *not* rolled back
    /// (the old one is gone from the vault); the restored adapter is rebuilt
    /// with whatever the key now holds, and simply fails closed if that is the
    /// thing that was wrong.
    pub(crate) async fn restore_channel_adapter(&self, previous: &RegisteredChannel) {
        let id = previous.id;
        if let Err(e) = self.channel_registry.register(previous.clone()).await {
            tracing::error!(channel_id = %id, error = %e, "Failed to restore channel row after a failed edit");
            return;
        }
        self.refresh_connected_channels_snapshot().await;
        match self
            .build_channel_adapter(
                &previous.kind,
                &previous.external_id,
                &previous.credential_key,
                &previous.reply_topic,
                &previous.server_url,
                &previous.webhook_url,
                id,
            )
            .await
        {
            Ok(Some(adapter)) => {
                let adapter: Arc<dyn crate::notification_router::DeliveryAdapter> =
                    Arc::from(adapter);
                self.notification_router
                    .register_adapter(adapter.clone())
                    .await;
                self.channel_listener_registry
                    .start(id, adapter, self.inbound_tx.clone())
                    .await;
            }
            Ok(None) => {
                if let Err(e) = self.register_channel_manager_adapter(&id).await {
                    tracing::error!(channel_id = %id, error = %e, "Channel restored but its adapter could not be rebuilt — sends will fail closed");
                }
            }
            Err(e) => {
                tracing::error!(channel_id = %id, error = %e, "Channel restored but its adapter could not be rebuilt — sends will fail closed");
            }
        }
    }

    /// Failure path for `cmd_update_channel`: unlike a failed connect, the row
    /// here was working a moment ago, so it is put back and the operator is
    /// told the edit — not the channel — is what was lost.
    fn edit_rolled_back(
        ch_id: ChannelInstanceID,
        kind: &ChannelKind,
        display_name: &str,
        error: &str,
    ) -> KernelResponse {
        tracing::warn!(
            channel_id = %ch_id,
            kind = %kind,
            display_name = %display_name,
            error = %error,
            "Channel edit failed to set up its adapter — the previous settings were restored"
        );
        KernelResponse::Error {
            message: format!(
                "Failed to set up adapter for channel '{display_name}' ({kind}): {error}. \
                 The edit was rolled back and the previous settings restored."
            ),
        }
    }

    /// Shared failure path for both adapter-setup arms of `cmd_connect_channel`.
    ///
    /// The registry row stays `active: true` on purpose. `deregister` is not
    /// reversible — the row vanishes from `channel list`, there is no
    /// reactivate command, and re-connecting mints a new `ChannelInstanceID` —
    /// so a transient failure (Telegram webhook registration is a live HTTPS
    /// POST with no retry) used to destroy the channel. The send path now
    /// fails closed on its own (`send_to_channel` finds no adapter → the
    /// `ChannelManager` → `Err`), so an active-but-undeliverable row cannot
    /// silently swallow a message; the operator fixes the credential and
    /// reconnects, or runs `channel disconnect`.
    fn adapter_setup_failed(
        ch_id: ChannelInstanceID,
        kind: &ChannelKind,
        display_name: &str,
        error: &str,
    ) -> KernelResponse {
        tracing::warn!(
            channel_id = %ch_id,
            kind = %kind,
            display_name = %display_name,
            error = %error,
            "Channel connect failed to set up its outbound adapter — the channel row is \
             kept (and will refuse sends) so it stays visible in `channel list`; fix the \
             credential and reconnect, or run `agentos channel disconnect`"
        );
        KernelResponse::Error {
            message: format!(
                "Failed to set up adapter for channel '{display_name}' ({kind}): {error}. \
                 The channel is registered but cannot send — reconnect after fixing this, \
                 or run `agentos channel disconnect {ch_id}`."
            ),
        }
    }

    /// Connect every enabled channel declared in the `[gateway]` config block.
    ///
    /// Used by `agentos gateway run` to bring channels up declaratively at boot,
    /// reusing the exact same path as `agentos channel connect`. Idempotent: a
    /// channel already active with the same `(kind, display_name)` — e.g. one
    /// restored from a prior `channel connect` — is skipped. Fails closed: an
    /// invalid kind, or a token-bearing channel with an empty `credential_key`,
    /// aborts boot rather than starting a partially-configured bot.
    pub async fn connect_configured_channels(&self) -> Result<(), anyhow::Error> {
        let gw = &self.config.gateway;
        if !gw.enabled {
            return Ok(());
        }

        // Snapshot already-active channels for idempotency.
        let existing = self
            .channel_registry
            .list_active()
            .await
            .map_err(|e| anyhow::anyhow!("gateway: failed to list active channels: {e}"))?;

        let mut connected = 0usize;
        for ch in &gw.channels {
            if !ch.enabled {
                continue;
            }
            // `ChannelKind::from_str` is infallible (an unknown string parses to
            // `Custom`), so validate explicitly: a typo'd / unsupported kind must
            // FAIL CLOSED rather than register a dead `Custom` channel that has no
            // adapter yet falsely reports "connected".
            let kind: ChannelKind = ch
                .kind
                .parse()
                .unwrap_or_else(|_| ChannelKind::Custom(ch.kind.clone()));
            if matches!(kind, ChannelKind::Custom(_)) {
                anyhow::bail!(
                    "gateway channel '{}': unsupported kind '{}' (supported: telegram, ntfy, email, discord, slack, whatsapp, webhook)",
                    if ch.display_name.trim().is_empty() {
                        ch.kind.as_str()
                    } else {
                        ch.display_name.as_str()
                    },
                    ch.kind
                );
            }

            let display_name = if ch.display_name.trim().is_empty() {
                ch.kind.clone()
            } else {
                ch.display_name.clone()
            };

            // Fail closed: token-bearing channels need a vault credential_key.
            let needs_token = matches!(
                kind,
                ChannelKind::Telegram | ChannelKind::Discord | ChannelKind::Slack
            );
            if needs_token && ch.credential_key.trim().is_empty() {
                anyhow::bail!(
                    "gateway channel '{display_name}' ({}) requires a non-empty credential_key (vault key)",
                    ch.kind
                );
            }

            // Idempotent: skip if an active channel with this kind+display_name exists.
            if existing
                .iter()
                .any(|e| e.kind == kind && e.display_name == display_name)
            {
                // The gateway only ADDS missing channels — it never reconciles a
                // config change (active_agent, credential_key, …) onto an already
                // active channel. Disconnect it first to re-apply. Warn so this
                // isn't silent.
                tracing::warn!(
                    channel = %display_name,
                    "Gateway: a channel with this kind+name is already active — skipping (gateway adds only, never reconciles)"
                );
                continue;
            }

            let resp = self
                .cmd_connect_channel(
                    kind,
                    ch.external_id.clone().unwrap_or_default(),
                    display_name.clone(),
                    ch.credential_key.clone(),
                    ch.reply_topic.clone(),
                    ch.server_url.clone(),
                    ch.webhook_url.clone(),
                    ch.active_agent.clone(),
                )
                .await;

            match resp {
                KernelResponse::Success { .. } => {
                    connected += 1;
                    tracing::info!(channel = %display_name, "Gateway: connected channel");
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("gateway channel '{display_name}' failed to connect: {message}");
                }
                _ => {
                    anyhow::bail!(
                        "gateway channel '{display_name}' failed to connect (unexpected kernel response)"
                    );
                }
            }
        }

        if connected > 0 {
            tracing::info!("Gateway: connected {connected} configured channel(s)");
        }
        Ok(())
    }

    /// Update a connected channel in place and rebuild its adapter.
    ///
    /// The optional fields are a three-state patch: `None` leaves it alone,
    /// `Some("")` clears it, `Some(v)` sets it. Three are exempt, because
    /// "clear" is not a state they have: `display_name` is required (blank is
    /// refused), `external_id` is a plain value (blank re-arms Telegram's
    /// chat-id auto-discovery rather than clearing anything), and
    /// `credential_key` is a secret pointer where blank must mean *keep* — so a
    /// channel cannot be un-credentialed through this path. `kind` is immutable:
    /// a different kind is a different adapter, so that is a disconnect +
    /// connect.
    ///
    /// `new_credential` is written to the vault *after* the old adapter is torn
    /// down, so the Telegram webhook is always deleted with the token that
    /// registered it.
    #[allow(clippy::too_many_arguments)]
    pub async fn cmd_update_channel(
        &self,
        channel_id: String,
        display_name: Option<String>,
        external_id: Option<String>,
        credential_key: Option<String>,
        new_credential: Option<zeroize::Zeroizing<String>>,
        reply_topic: Option<String>,
        server_url: Option<String>,
        webhook_url: Option<String>,
        active_agent_name: Option<String>,
    ) -> KernelResponse {
        let id: ChannelInstanceID = match channel_id.parse() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid channel ID: '{channel_id}'"),
                }
            }
        };
        let existing = match self.channel_registry.get_by_id(&id).await {
            Ok(Some(ch)) => ch,
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
        };

        /// `Some("")` clears an optional field, `None` leaves it as it was.
        fn patch_opt(current: &Option<String>, next: Option<String>) -> Option<String> {
            match next {
                None => current.clone(),
                Some(v) if v.trim().is_empty() => None,
                Some(v) => Some(v.trim().to_string()),
            }
        }

        let display_name = match display_name {
            Some(v) if v.trim().is_empty() => {
                return KernelResponse::Error {
                    message: "display_name cannot be empty".to_string(),
                }
            }
            Some(v) => v.trim().to_string(),
            None => existing.display_name.clone(),
        };
        let external_id = external_id
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| existing.external_id.clone());
        let mut credential_key = credential_key
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| existing.credential_key.clone());
        // A channel connected without a secret (a public ntfy topic, say) has no
        // key at all. Deriving one here is what lets the operator add a token
        // later instead of being told to pass a `credential_key` by hand.
        if credential_key.is_empty() && new_credential.is_some() {
            credential_key = self
                .derive_channel_credential_key(&existing.kind, &display_name)
                .await;
        }
        let reply_topic = patch_opt(&existing.reply_topic, reply_topic);
        let server_url = patch_opt(&existing.server_url, server_url);
        let webhook_url = patch_opt(&existing.webhook_url, webhook_url);
        let active_agent_name = patch_opt(&existing.active_agent_name, active_agent_name);

        if let Some(ref n) = active_agent_name {
            let reg = self.agent_registry.read().await;
            if reg.get_by_name(n).is_none() {
                return KernelResponse::Error {
                    message: format!("Unknown agent '{n}'"),
                };
            }
        }

        // Tear the old adapter down before the row changes — the Telegram
        // webhook is deleted with the *old* URL/token, and a listener left
        // running would keep polling with stale credentials.
        if existing.kind == ChannelKind::Telegram && existing.webhook_url.is_some() {
            if let Ok(bot_token) = self.vault.get(&existing.credential_key).await {
                let adapter = crate::adapters::telegram::TelegramDeliveryAdapter::new(
                    bot_token.as_str().to_string(),
                    String::new(),
                    id,
                    None,
                );
                if let Err(e) = adapter.delete_webhook().await {
                    tracing::warn!(error = %e, "Failed to delete Telegram webhook on update");
                }
            }
            self.webhook_secrets.write().await.remove(&id);
        }
        self.channel_listener_registry.stop(&id).await;
        self.notification_router
            .deregister_adapter(&id.to_string())
            .await;
        // Also drop the ChannelManager half: if the rebuild below fails, an
        // adapter left running on the old settings would keep delivering to the
        // old destination while the row on screen says otherwise. No adapter
        // means the send path fails closed, which is what a broken channel owes
        // the operator.
        self.channel_manager.deregister(&id.to_string()).await;

        // Only now — the teardown above had to read the *old* token.
        if let Some(secret) = new_credential {
            if let Err(e) = self
                .vault
                .set(
                    &credential_key,
                    secret.as_str(),
                    agentos_types::SecretOwner::Kernel,
                    agentos_types::SecretScope::Global,
                )
                .await
            {
                self.restore_channel_adapter(&existing).await;
                return KernelResponse::Error {
                    message: format!("Failed to store channel credential: {e}"),
                };
            }
        }

        let updated = RegisteredChannel {
            id,
            kind: existing.kind.clone(),
            external_id: external_id.clone(),
            display_name: display_name.clone(),
            credential_key: credential_key.clone(),
            reply_topic: reply_topic.clone(),
            server_url: server_url.clone(),
            webhook_url: webhook_url.clone(),
            active_agent_name,
            connected_at: existing.connected_at,
            last_active: Utc::now(),
            active: true,
        };
        // `register` upserts on id, so the instance id, its pairings and its
        // history all survive the edit.
        if let Err(e) = self.channel_registry.register(updated).await {
            return KernelResponse::Error {
                message: format!("Failed to update channel: {e}"),
            };
        }
        self.refresh_connected_channels_snapshot().await;

        let adapter_result = self
            .build_channel_adapter(
                &existing.kind,
                &external_id,
                &credential_key,
                &reply_topic,
                &server_url,
                &webhook_url,
                id,
            )
            .await;
        match adapter_result {
            Ok(Some(adapter)) => {
                let adapter: Arc<dyn crate::notification_router::DeliveryAdapter> =
                    Arc::from(adapter);
                self.notification_router
                    .register_adapter(adapter.clone())
                    .await;
                self.channel_listener_registry
                    .start(id, adapter, self.inbound_tx.clone())
                    .await;
            }
            Ok(None) => {
                if let Err(e) = self.register_channel_manager_adapter(&id).await {
                    self.restore_channel_adapter(&existing).await;
                    return Self::edit_rolled_back(id, &existing.kind, &display_name, &e);
                }
            }
            Err(e) => {
                self.restore_channel_adapter(&existing).await;
                return Self::edit_rolled_back(id, &existing.kind, &display_name, &e);
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
                "channel_id": channel_id,
                "kind": existing.kind.to_string(),
                "display_name": display_name,
                "updated": true,
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "channel_id": channel_id,
                "display_name": display_name,
                "message": "Channel updated",
            })),
        }
    }

    pub async fn cmd_disconnect_channel(&self, channel_id: String) -> KernelResponse {
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

    pub async fn cmd_set_channel_active_agent(
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
    pub async fn cmd_test_channel(&self, channel_id: String) -> KernelResponse {
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
            attachment: None,
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

    /// List the DM pairing allowlist: approved senders + pending requests.
    pub async fn cmd_list_pairings(&self) -> KernelResponse {
        let approved = self
            .pairing_manager
            .list_approved()
            .await
            .into_iter()
            .map(|s| agentos_bus::PairingEntry {
                channel_id: s.channel_id,
                sender_id: s.sender_id,
                approved_at: s.approved_at.to_rfc3339(),
                label: s.label,
            })
            .collect();
        let pending = self
            .pairing_manager
            .list_pending()
            .await
            .into_iter()
            .map(|p| agentos_bus::PendingPairingEntry {
                channel_id: p.channel_id,
                sender_id: p.sender_id,
                expires_at: p.expires_at.to_rfc3339(),
            })
            .collect();
        KernelResponse::PairingList { approved, pending }
    }

    /// Approve a pending pairing code, allowlisting the sender.
    ///
    /// Uses `approve_code_trusted`: the caller already authenticated to the
    /// kernel bus, so it must not share the failed-guess budget with the
    /// `/pair` path that is open to unpaired channel senders — a stranger
    /// spamming wrong codes there kept the budget exhausted and locked the
    /// operator out of pairing entirely.
    pub async fn cmd_approve_pairing(&self, code: String) -> KernelResponse {
        // Normalize to match the `/pair` arm, which upper-cases codes.
        match self
            .pairing_manager
            .approve_code_trusted(&code.trim().to_uppercase())
            .await
        {
            Ok(sender) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "message": "Pairing approved",
                    "channel_id": sender.channel_id,
                    "sender_id": sender.sender_id,
                })),
            },
            Err(e) => KernelResponse::Error { message: e },
        }
    }

    /// Approve a pending pairing request by `(channel_id, sender_id)`.
    ///
    /// The code-based path needs the operator to read it out of the kernel log,
    /// because it is withheld from every introspection surface on purpose. That
    /// is circular when the operator is the sender, so this approves the row
    /// `cmd_list_pairings` already returns. Operator-authenticated callers only
    /// — it takes no secret, so it is never reachable from inbound `/pair`.
    pub async fn cmd_approve_pending_pairing(
        &self,
        channel_id: String,
        sender_id: String,
    ) -> KernelResponse {
        match self
            .pairing_manager
            .approve_pending(&channel_id, &sender_id)
            .await
        {
            Ok(sender) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "message": "Pairing approved",
                    "channel_id": sender.channel_id,
                    "sender_id": sender.sender_id,
                })),
            },
            Err(e) => KernelResponse::Error { message: e },
        }
    }

    /// Revoke an approved sender from a channel's DM allowlist.
    pub async fn cmd_revoke_pairing(
        &self,
        channel_id: String,
        sender_id: String,
    ) -> KernelResponse {
        if self.pairing_manager.revoke(&channel_id, &sender_id).await {
            KernelResponse::Success {
                data: Some(serde_json::json!({
                    "message": "Pairing revoked",
                    "channel_id": channel_id,
                    "sender_id": sender_id,
                })),
            }
        } else {
            KernelResponse::Error {
                message: format!("No approved sender '{sender_id}' on channel '{channel_id}'"),
            }
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
        // Outbound sends must be able to reach *both* stacks (this function
        // decides which one a kind lands on), but the router and the escalation
        // sink are built at boot before their counterparts exist. Every connect
        // and every boot-time restore funnels through here, so this is the
        // earliest point at which a send is possible — bind the handles now.
        // Both setters are idempotent.
        self.notification_router
            .attach_channel_manager(Arc::clone(&self.channel_manager));
        self.escalation_manager
            .attach_notification_router(Arc::clone(&self.notification_router))
            .await;

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
                let adapter =
                    crate::adapters::email::EmailDeliveryAdapter::new(channel_instance_id);
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

    pub(crate) async fn register_channel_manager_adapter(
        &self,
        channel_id: &ChannelInstanceID,
    ) -> Result<(), String> {
        use agentos_channels::ChannelAdapter;

        let ch = self
            .channel_registry
            .get_by_id(channel_id)
            .await
            .map_err(|e| format!("failed to look up channel: {e}"))?
            .ok_or_else(|| format!("channel '{channel_id}' not found"))?;

        let adapter: Option<Arc<dyn ChannelAdapter>> = match ch.kind {
            ChannelKind::Discord => {
                if ch.credential_key.trim().is_empty() || ch.external_id.trim().is_empty() {
                    return Err(
                        "Discord requires credential_key (bot token vault key) and external_id (channel id)"
                            .to_string(),
                    );
                }
                let token = self
                    .vault
                    .get(&ch.credential_key)
                    .await
                    .map_err(|e| format!("failed to retrieve Discord token from vault: {e}"))?;
                Some(Arc::new(agentos_channels::discord::DiscordAdapter::new(
                    token.as_str().to_string(),
                    ch.external_id.clone(),
                    channel_id.to_string(),
                )))
            }
            ChannelKind::Slack => {
                if ch.credential_key.trim().is_empty() || ch.external_id.trim().is_empty() {
                    return Err(
                        "Slack requires credential_key (bot token vault key) and external_id (channel id)"
                            .to_string(),
                    );
                }
                let token = self
                    .vault
                    .get(&ch.credential_key)
                    .await
                    .map_err(|e| format!("failed to retrieve Slack token from vault: {e}"))?;
                Some(Arc::new(agentos_channels::slack::SlackAdapter::new(
                    token.as_str().to_string(),
                    ch.external_id.clone(),
                    channel_id.to_string(),
                )))
            }
            ChannelKind::Webhook => {
                let target_url = ch
                    .webhook_url
                    .clone()
                    .ok_or_else(|| "Webhook requires webhook_url target".to_string())?;
                agentos_channels::webhook::validate_webhook_url(&target_url)
                    .map_err(|e| e.to_string())?;
                let secret = if ch.credential_key.trim().is_empty() {
                    return Err("Webhook requires credential_key (signing secret vault key)".into());
                } else {
                    self.vault
                        .get(&ch.credential_key)
                        .await
                        .map_err(|e| format!("failed to retrieve webhook secret from vault: {e}"))?
                        .as_str()
                        .to_string()
                };
                Some(Arc::new(agentos_channels::webhook::WebhookAdapter::new(
                    target_url,
                    secret,
                    channel_id.to_string(),
                )))
            }
            ChannelKind::WhatsApp => {
                if ch.credential_key.trim().is_empty()
                    || ch.external_id.trim().is_empty()
                    || ch.reply_topic.as_deref().unwrap_or("").trim().is_empty()
                {
                    return Err(
                        "WhatsApp requires credential_key (access token vault key), external_id (recipient phone), and reply_topic (phone_number_id)"
                            .to_string(),
                    );
                }
                let token =
                    self.vault.get(&ch.credential_key).await.map_err(|e| {
                        format!("failed to retrieve WhatsApp token from vault: {e}")
                    })?;
                let phone_number_id = ch.reply_topic.clone().unwrap_or_default();
                let adapter = agentos_channels::whatsapp::WhatsAppAdapter::new(
                    token.as_str().to_string(),
                    phone_number_id,
                    ch.external_id.clone(),
                    channel_id.to_string(),
                )
                .map_err(|e| e.to_string())?;
                Some(Arc::new(adapter))
            }
            _ => None,
        };

        if let Some(adapter) = adapter {
            self.channel_manager
                .register(&channel_id.to_string(), adapter)
                .await
                .map_err(|e| e.to_string())?;
            tracing::info!(
                channel_id = %channel_id,
                kind = %ch.kind,
                "Registered channel-manager adapter"
            );
        } else {
            tracing::info!(
                channel_id = %channel_id,
                kind = %ch.kind,
                "Channel registered without runtime adapter"
            );
        }
        Ok(())
    }
}
