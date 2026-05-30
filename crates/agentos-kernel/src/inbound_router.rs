use crate::channel_chat_bridge::KernelChatBridge;
use crate::escalation::EscalationManager;
use crate::notification_router::{InboundMessage, NotificationRouter};
use crate::scheduler::TaskScheduler;
use crate::user_channel_registry::UserChannelRegistry;
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_channels::pairing::PairingManager;
use agentos_types::{
    AgentOSError, ChannelInstanceID, ChannelKind, DeliveryChannel, NotificationID,
    NotificationPriority, NotificationSource, TaskState, TraceID, UserMessage, UserMessageKind,
    UserResponse,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

const HELP_TEXT: &str = "\
AgentOS commands:
  /tasks      — list active tasks
  /status     — system status (alias for /tasks)
  /stop <id>  — cancel a task (first 8 chars of task ID)
  /approve <id> — approve a pending escalation (paired senders only)
  /deny <id>  — deny a pending escalation (paired senders only)
  /pair <code> — authorise this channel sender to issue /approve and /deny.
                 Codes are issued the first time you try /approve or /deny.
  /help       — show this message
  /agents     — list agents available for chat
  /agent      — show the default chat agent for this channel
  /agent <name> — set default chat agent (use web/CLI to clear)
  /chat <name> <message> — send one message to an agent

When a default agent is set (via /agent or `channel connect --active-agent`),
plain text is sent to that agent like the web chat.";

/// Maximum inbound messages accepted per channel per minute.
const INBOUND_RATE_LIMIT: u32 = 30;

/// Routes inbound messages from external bidirectional channels to the
/// appropriate kernel subsystem.
///
/// Runs as a background task consuming `InboundMessage`s forwarded by
/// `ChannelListenerRegistry`.  Handles:
/// 1. **Question replies** — routes to `NotificationRouter::route_response`.
/// 2. **Slash commands** — `/tasks`, `/status`, `/stop`, `/help`, `/agent`, `/chat`, …
/// 3. **Channel chat** — free-text to the configured default agent (`active_agent_name`).
/// 4. **Free-text fallback** — acknowledges when no agent is configured.
pub struct InboundRouter {
    notification_router: Arc<NotificationRouter>,
    channel_registry: Arc<UserChannelRegistry>,
    scheduler: Arc<TaskScheduler>,
    chat_bridge: Arc<KernelChatBridge>,
    audit: Arc<AuditLog>,
    /// Resolves `/approve <id>` and `/deny <id>` channel commands.
    escalation_manager: Arc<EscalationManager>,
    /// Verifies that a channel sender is paired before honoring approval
    /// commands. Without this, anyone who can DM the bot could resolve
    /// pending escalations.
    pairing_manager: Arc<PairingManager>,
    /// Vault, for resolving a channel's bot-token credential when downloading
    /// inbound media (Telegram getFile needs the token).
    vault: Arc<agentos_vault::SecretsVault>,
    /// Persists downloaded inbound media; shared slot with the kernel so a
    /// post-boot `set_attachment_sink` is honored.
    attachment_sink: Arc<std::sync::RwLock<Arc<dyn crate::attachment_sink::AttachmentSink>>>,
    /// HTTP client for media downloads (getFile + file fetch) and transcription.
    http_client: reqwest::Client,
    /// Speech-to-text settings for inbound voice/audio (disabled by default).
    transcription: crate::config::TranscriptionSettings,
    rx: mpsc::Receiver<InboundMessage>,
    /// Per-channel rate limiter: (message count, window start instant).
    rate_limiter: HashMap<ChannelInstanceID, (u32, Instant)>,
    /// Last time the rate limiter map was pruned of stale entries.
    last_prune: Instant,
}

impl InboundRouter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        notification_router: Arc<NotificationRouter>,
        channel_registry: Arc<UserChannelRegistry>,
        scheduler: Arc<TaskScheduler>,
        chat_bridge: Arc<KernelChatBridge>,
        audit: Arc<AuditLog>,
        escalation_manager: Arc<EscalationManager>,
        pairing_manager: Arc<PairingManager>,
        vault: Arc<agentos_vault::SecretsVault>,
        attachment_sink: Arc<std::sync::RwLock<Arc<dyn crate::attachment_sink::AttachmentSink>>>,
        transcription: crate::config::TranscriptionSettings,
        rx: mpsc::Receiver<InboundMessage>,
    ) -> Self {
        Self {
            notification_router,
            channel_registry,
            scheduler,
            chat_bridge,
            audit,
            escalation_manager,
            pairing_manager,
            vault,
            attachment_sink,
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
            transcription,
            rx,
            rate_limiter: HashMap::new(),
            last_prune: Instant::now(),
        }
    }

    /// Run the router loop until the sender side is dropped (kernel shutdown).
    pub async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            if let Err(e) = self.route(msg).await {
                tracing::warn!("InboundRouter: routing error: {e}");
            }
        }
    }

    /// Best-effort: if an inbound Telegram message carries media, download it
    /// (using the channel's vaulted bot token), persist it via the attachment
    /// sink, and append a stored-file reference to `msg.text`. Any failure
    /// (no token, download error, no sink configured) is logged and skipped —
    /// the descriptive media note added at parse time still reaches the agent.
    async fn enrich_inbound_media(&self, msg: &mut InboundMessage) {
        use crate::adapters::telegram::{
            download_telegram_file, ext_for_mime, telegram_media_ref, TelegramMessage,
            TELEGRAM_MAX_DOWNLOAD_BYTES,
        };

        if msg.channel != DeliveryChannel::custom(DeliveryChannel::TELEGRAM) {
            return;
        }
        // `raw` holds the serialized TelegramMessage for message updates; for
        // callback queries it won't deserialize, so this returns early.
        let tg: TelegramMessage = match serde_json::from_value(msg.raw.clone()) {
            Ok(m) => m,
            Err(_) => return,
        };
        let media = match telegram_media_ref(&tg) {
            Some(m) => m,
            None => return,
        };

        let credential_key = match self
            .channel_registry
            .get_by_id(&msg.channel_instance_id)
            .await
        {
            Ok(Some(ch)) => ch.credential_key,
            _ => return,
        };
        if credential_key.is_empty() {
            return;
        }
        let token = match self.vault.get(&credential_key).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "inbound media: bot token unavailable; skipping download");
                return;
            }
        };

        let (bytes, mime) = match download_telegram_file(
            &self.http_client,
            token.as_str(),
            &media.file_id,
            TELEGRAM_MAX_DOWNLOAD_BYTES,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "inbound media download failed");
                return;
            }
        };

        let name = media
            .filename
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "telegram-{}.{}",
                    media.kind_label.replace(' ', "-"),
                    ext_for_mime(&mime)
                )
            });

        // Transcribe voice/audio when enabled, so the agent reads the words.
        // (Hermes-style "transcribe, don't drop".) Best-effort: on failure the
        // media note + stored file still reach the agent.
        let is_audio = matches!(media.kind_label.as_str(), "voice message" | "audio");
        if is_audio && self.transcription.enabled {
            // Self-bounded inner timeout so the transcription call is capped
            // regardless of the caller's wrapper (the outer enrich timeout).
            let fut = crate::transcription::transcribe_audio(
                &self.http_client,
                &self.transcription,
                bytes.clone(),
                &name,
            );
            match tokio::time::timeout(std::time::Duration::from_secs(15), fut).await {
                Ok(Ok(transcript)) => {
                    msg.text
                        .push_str(&format!("\n[Voice transcript]: {transcript}"));
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "inbound voice transcription failed");
                }
                Err(_) => {
                    tracing::warn!("inbound voice transcription timed out");
                }
            }
        }

        let sink = self
            .attachment_sink
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let byte_len = bytes.len();
        match sink.store(&name, &mime, bytes).await {
            Ok(file_id) => {
                // Audit: external bytes were downloaded and persisted to disk.
                let _ = self.audit.append(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: AuditEventType::InboundMessageReceived,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "kind": "inbound_media_stored",
                        "channel_id": msg.channel_instance_id.to_string(),
                        "file_id": file_id.clone(),
                        "name": name,
                        "mime": mime,
                        "bytes": byte_len,
                        "media_kind": media.kind_label,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                if mime.starts_with("image/") {
                    // Carried into the chat context as a ContentPart::Image so
                    // vision-capable agents see it (adapter resolves the FileRef;
                    // non-vision agents get an automatic text stub).
                    msg.media_file_ids.push((file_id, mime.clone()));
                } else {
                    // Non-image files have no vision path yet — note the stored id
                    // so the agent can reference it.
                    msg.text.push_str(&format!(
                        "\n[Attachment stored — file id: {file_id}, name: {name}, type: {mime}]"
                    ));
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "attachment sink declined; media not persisted");
            }
        }
    }

    async fn route(&mut self, mut msg: InboundMessage) -> Result<(), AgentOSError> {
        // Prune stale rate-limiter entries at most once per minute regardless of map size.
        if self.last_prune.elapsed().as_secs() >= 60 {
            self.rate_limiter
                .retain(|_, (_, ts)| ts.elapsed().as_secs() < 300);
            self.last_prune = Instant::now();
        }

        let now = Instant::now();
        let entry = self
            .rate_limiter
            .entry(msg.channel_instance_id)
            .or_insert((0, now));
        if entry.1.elapsed().as_secs() >= 60 {
            *entry = (0, now);
        }
        if entry.0 >= INBOUND_RATE_LIMIT {
            tracing::warn!(
                channel_id = %msg.channel_instance_id,
                "Inbound rate limit exceeded; dropping message"
            );
            return Ok(());
        }
        entry.0 += 1;

        self.channel_registry
            .update_last_active(&msg.channel_instance_id)
            .await
            .ok();

        if msg.channel == DeliveryChannel::custom(DeliveryChannel::TELEGRAM)
            && !msg.external_sender_id.is_empty()
        {
            if let Ok(Some(ch)) = self
                .channel_registry
                .get_by_id(&msg.channel_instance_id)
                .await
            {
                if matches!(ch.kind, ChannelKind::Telegram)
                    && ch.external_id.is_empty()
                    && self
                        .channel_registry
                        .update_external_id(&msg.channel_instance_id, &msg.external_sender_id)
                        .await
                        .is_ok()
                {
                    self.notification_router
                        .hydrate_discovered_recipient(
                            &msg.channel_instance_id,
                            &msg.external_sender_id,
                        )
                        .await;
                }
            }
        }

        // Download + persist any inbound media (best-effort) and annotate the
        // message text with a stored file reference. Runs before routing so the
        // enriched text reaches questions, chat, and the inbox alike. Bounded by
        // a timeout so a slow/large fetch cannot stall the shared inbound loop
        // (which also carries /stop, /approve, and question replies); on timeout
        // the parse-time media note still reaches the agent. (Fully off-loop
        // enrichment is a planned follow-up — see telegram-media-pipeline.)
        if tokio::time::timeout(
            std::time::Duration::from_secs(20),
            self.enrich_inbound_media(&mut msg),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                channel_id = %msg.channel_instance_id,
                "inbound media enrichment timed out; forwarding text-only note"
            );
        }

        if let Some(notif_id) = msg.reply_to_notification_id {
            let response = UserResponse {
                text: msg.text.clone(),
                responded_at: msg.received_at,
                channel: msg.channel.clone(),
            };
            self.notification_router
                .route_response(notif_id, response)
                .await?;
            self.send_reply(
                &msg,
                "Your response has been sent to the agent.".to_string(),
            )
            .await;
            return Ok(());
        }

        if msg.text.starts_with('/') {
            return self.handle_slash_command(msg).await;
        }

        let waiting_ids = self.notification_router.waiting_question_ids().await;
        if waiting_ids.len() == 1 {
            let channel_authenticated = self
                .channel_registry
                .get_by_id(&msg.channel_instance_id)
                .await
                .ok()
                .flatten()
                .map(|ch| !ch.credential_key.is_empty())
                .unwrap_or(false);

            if !channel_authenticated {
                tracing::warn!(
                    channel_id = %msg.channel_instance_id,
                    "Rejecting auto-route from unauthenticated channel"
                );
                self.send_reply(
                    &msg,
                    "This channel is not authenticated. Please reply via the web UI or CLI."
                        .to_string(),
                )
                .await;
                return Ok(());
            }

            let notif_id = waiting_ids[0];
            let response = UserResponse {
                text: msg.text.clone(),
                responded_at: msg.received_at,
                channel: msg.channel.clone(),
            };
            if self
                .notification_router
                .route_response(notif_id, response)
                .await
                .is_ok()
            {
                self.send_reply(
                    &msg,
                    "Your response has been sent to the agent.".to_string(),
                )
                .await;
                return Ok(());
            }
        } else if waiting_ids.len() > 1 {
            self.send_reply(
                &msg,
                format!(
                    "{} agents are waiting for your response. \
                     Reply via the web inbox or CLI to answer a specific question.",
                    waiting_ids.len()
                ),
            )
            .await;
            return Ok(());
        }

        // Default-agent channel chat (same inference path as the web UI).
        if let Ok(Some(ch)) = self
            .channel_registry
            .get_by_id(&msg.channel_instance_id)
            .await
        {
            if !ch.active {
                // Channel was deregistered; silently drop the message.
                return Ok(());
            }
            if let Some(agent) = ch
                .active_agent_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if !ch.credential_key.is_empty() {
                    let text = msg.text.trim();
                    if !text.is_empty() {
                        // Carry any stored inbound images into the chat as vision
                        // parts so a vision-capable agent can see them.
                        let user_parts = if msg.media_file_ids.is_empty() {
                            None
                        } else {
                            let mut parts = vec![agentos_types::ContentPart::Text {
                                text: text.to_string(),
                            }];
                            for (file_id, mime) in &msg.media_file_ids {
                                parts.push(agentos_types::ContentPart::Image {
                                    mime: mime.clone(),
                                    source: agentos_types::ImageSource::FileRef {
                                        file_id: file_id.clone(),
                                    },
                                });
                            }
                            Some(parts)
                        };
                        match self
                            .chat_bridge
                            .channel_chat(msg.channel_instance_id, agent, text, user_parts)
                            .await
                        {
                            Ok(answer) => {
                                self.send_agent_chat_reply(&msg, agent, &answer).await;
                                return Ok(());
                            }
                            Err(e) => {
                                tracing::warn!(
                                    channel_id = %msg.channel_instance_id,
                                    agent,
                                    error = %e,
                                    "channel chat inference failed"
                                );
                                self.send_reply(
                                    &msg,
                                    "Sorry — the agent could not respond. Try again or use /help."
                                        .to_string(),
                                )
                                .await;
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }

        self.send_reply(
            &msg,
            "Message received. Use /help for commands, /agents to list agents, \
             `/agent <name>` to choose a default, or `channel set-agent` from the CLI."
                .to_string(),
        )
        .await;
        Ok(())
    }

    async fn handle_slash_command(&self, msg: InboundMessage) -> Result<(), AgentOSError> {
        let parts: Vec<&str> = msg.text.splitn(3, ' ').collect();
        let cmd = parts[0].to_ascii_lowercase();
        let cmd = cmd.as_str();

        match cmd {
            "/tasks" | "/status" => {
                let tasks = self.scheduler.list_tasks().await;
                let reply = if tasks.is_empty() {
                    "No active tasks.".to_string()
                } else {
                    let lines: Vec<String> = tasks
                        .iter()
                        .map(|t| {
                            format!(
                                "[{}] {:?} — {}",
                                &t.id.to_string()[..8],
                                t.state,
                                t.prompt_preview.chars().take(60).collect::<String>(),
                            )
                        })
                        .collect();
                    format!("Tasks ({}):\n{}", tasks.len(), lines.join("\n"))
                };
                self.send_reply(&msg, reply).await;
            }

            "/stop" if parts.len() > 1 => {
                let prefix = parts[1];
                let tasks = self.scheduler.list_tasks().await;
                let found = tasks.iter().find(|t| t.id.to_string().starts_with(prefix));
                match found {
                    Some(task) => {
                        let id = task.id;
                        match self
                            .scheduler
                            .update_state_if_not_terminal(&id, TaskState::Cancelled)
                            .await
                        {
                            Ok(true) => {
                                self.send_reply(&msg, format!("Task {prefix}… cancelled."))
                                    .await;
                            }
                            Ok(false) => {
                                self.send_reply(
                                    &msg,
                                    format!("Task {prefix}… is already in a terminal state."),
                                )
                                .await;
                            }
                            Err(e) => {
                                self.send_reply(&msg, format!("Failed to cancel: {e}"))
                                    .await;
                            }
                        }
                    }
                    None => {
                        self.send_reply(&msg, format!("No task found with prefix '{prefix}'."))
                            .await;
                    }
                }
            }

            "/help" => {
                self.send_reply(&msg, HELP_TEXT.to_string()).await;
            }

            "/agents" => {
                let reply = match self.chat_bridge.list_online_agent_names().await {
                    Some(names) if !names.is_empty() => {
                        format!("Online agents:\n{}", names.join("\n"))
                    }
                    Some(_) => "No agents are online.".to_string(),
                    None => "Chat bridge is not ready yet.".to_string(),
                };
                self.send_reply(&msg, reply).await;
            }

            "/agent" => {
                let ch = self
                    .channel_registry
                    .get_by_id(&msg.channel_instance_id)
                    .await
                    .ok()
                    .flatten();
                if parts.len() < 2 {
                    let current = ch
                        .as_ref()
                        .and_then(|c| c.active_agent_name.as_deref())
                        .unwrap_or("(none)");
                    let names = self
                        .chat_bridge
                        .list_online_agent_names()
                        .await
                        .unwrap_or_default();
                    let list = if names.is_empty() {
                        "(none online)".to_string()
                    } else {
                        names.join(", ")
                    };
                    self.send_reply(
                        &msg,
                        format!(
                            "Default chat agent for this channel: {current}\nOnline: {list}\n\nUse `/agent <name>` to set."
                        ),
                    )
                    .await;
                } else {
                    let name = parts[1].trim();
                    if name.is_empty() {
                        self.send_reply(&msg, "Agent name cannot be empty.".to_string())
                            .await;
                        return Ok(());
                    }
                    if self.chat_bridge.agent_id_for_name(name).await.is_none() {
                        self.send_reply(
                            &msg,
                            format!("Unknown or offline agent '{name}'. Try /agents."),
                        )
                        .await;
                        return Ok(());
                    }
                    let prev = self
                        .channel_registry
                        .get_by_id(&msg.channel_instance_id)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|c| c.active_agent_name);
                    if let Err(e) = self
                        .channel_registry
                        .update_active_agent_name(&msg.channel_instance_id, Some(name))
                        .await
                    {
                        self.send_reply(&msg, format!("Failed to save: {e}")).await;
                        return Ok(());
                    }
                    // Clear history only when the bound agent actually changes.
                    // Re-binding the same agent preserves the in-progress conversation.
                    if prev.as_deref() != Some(name) {
                        if let Some(ref old) = prev {
                            self.chat_bridge
                                .clear_history(msg.channel_instance_id, old)
                                .await;
                        }
                        self.chat_bridge
                            .clear_history(msg.channel_instance_id, name)
                            .await;
                    }
                    self.send_reply(
                        &msg,
                        format!("Default chat agent set to '{name}'. Send plain text to chat."),
                    )
                    .await;
                }
            }

            "/chat" if parts.len() >= 3 => {
                let agent = parts[1].trim();
                let prompt = parts[2].trim();
                if agent.is_empty() || prompt.is_empty() {
                    self.send_reply(&msg, "Usage: /chat <agent> <message>".to_string())
                        .await;
                    return Ok(());
                }
                let ch = self
                    .channel_registry
                    .get_by_id(&msg.channel_instance_id)
                    .await
                    .ok()
                    .flatten();
                if ch
                    .as_ref()
                    .map(|c| c.credential_key.is_empty())
                    .unwrap_or(true)
                {
                    self.send_reply(
                        &msg,
                        "This channel is not authenticated; chat is disabled.".to_string(),
                    )
                    .await;
                    return Ok(());
                }
                match self
                    .chat_bridge
                    .channel_chat(msg.channel_instance_id, agent, prompt, None)
                    .await
                {
                    Ok(answer) => {
                        self.send_agent_chat_reply(&msg, agent, &answer).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            channel_id = %msg.channel_instance_id,
                            agent,
                            error = %e,
                            "channel /chat inference failed"
                        );
                        self.send_reply(
                            &msg,
                            "Sorry — the agent could not respond. Try again or use /help."
                                .to_string(),
                        )
                        .await;
                    }
                }
            }

            "/start" => {
                self.send_reply(
                    &msg,
                    "Welcome to AgentOS. Your chat is linked; send /help for commands.".to_string(),
                )
                .await;
            }

            "/approve" | "/deny" if parts.len() >= 2 => {
                self.handle_approval_command(&msg, cmd, parts[1].trim())
                    .await;
            }

            "/approve" | "/deny" => {
                self.send_reply(&msg, format!("Usage: {cmd} <escalation-id>"))
                    .await;
            }

            "/pair" if parts.len() >= 2 => {
                // Approve a pairing code generated when an unknown sender
                // first DMed the bot. After this succeeds, the channel
                // sender can use `/approve <id>` and `/deny <id>` on
                // pending escalations (R3 finding C1: without this arm
                // the entire approval-channel-fanout feature is a no-op
                // because `PairingManager.list_approved` stays empty).
                let code = parts[1].trim();
                match self.pairing_manager.approve_code(code).await {
                    Ok(sender) => {
                        tracing::info!(
                            channel_id = %msg.channel_instance_id,
                            sender_id = %sender.sender_id,
                            "Pairing approved via /pair"
                        );
                        let _ = self.audit.append(AuditEntry {
                            timestamp: Utc::now(),
                            trace_id: TraceID::new(),
                            event_type: AuditEventType::PermissionGranted,
                            agent_id: None,
                            task_id: None,
                            tool_id: None,
                            details: serde_json::json!({
                                "subsystem": "inbound_router",
                                "kind": "channel_pairing_approved",
                                "channel_instance_id": msg.channel_instance_id.to_string(),
                                "channel_sender": sender.sender_id,
                            }),
                            severity: AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        });
                        self.send_reply(
                            &msg,
                            "✅ Paired. You can now use `/approve <id>` and \
                             `/deny <id>` to resolve pending escalations."
                                .to_string(),
                        )
                        .await;
                    }
                    Err(reason) => {
                        // Uniform error from PairingManager — does NOT
                        // distinguish wrong code from expired so attackers
                        // cannot probe for valid prefixes.
                        self.send_reply(&msg, reason).await;
                    }
                }
            }

            "/pair" => {
                self.send_reply(
                    &msg,
                    "Usage: /pair <code>. Codes are issued the first time you \
                     DM the bot from an unpaired channel."
                        .to_string(),
                )
                .await;
            }

            _ => {
                self.send_reply(
                    &msg,
                    format!("Unknown command '{cmd}'. Send /help for available commands."),
                )
                .await;
            }
        }

        Ok(())
    }

    /// Handle `/approve <id>` and `/deny <id>` commands from a paired
    /// channel sender. The sender MUST be on the pairing allowlist for
    /// this channel; otherwise the command is rejected without consulting
    /// the escalation store. Already-resolved escalations return a clear
    /// "already resolved" reply (idempotent).
    async fn handle_approval_command(&self, msg: &InboundMessage, cmd: &str, id_str: &str) {
        // Parse the escalation id first so a malformed id doesn't leak
        // the existence of paired senders.
        let id: u64 = match id_str.parse() {
            Ok(v) => v,
            Err(_) => {
                self.send_reply(
                    msg,
                    format!("Invalid escalation id '{id_str}' — must be a number."),
                )
                .await;
                return;
            }
        };

        // Pairing check. We use `external_sender_id` as the sender
        // identity and `channel_instance_id` as the channel scope. An
        // unpaired sender gets a uniform error that does NOT confirm
        // the escalation exists.
        let channel_id_str = msg.channel_instance_id.to_string();
        if !self
            .pairing_manager
            .is_allowed(&channel_id_str, &msg.external_sender_id)
            .await
        {
            tracing::warn!(
                channel_id = %channel_id_str,
                sender = %msg.external_sender_id,
                escalation_id = id,
                command = cmd,
                "Rejecting approval command from unpaired sender"
            );
            let _ = self.audit.append(AuditEntry {
                timestamp: Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::ActionForbidden,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "subsystem": "inbound_router",
                    "command": cmd,
                    "escalation_id": id,
                    "reason": "channel_sender_not_paired",
                    "channel_instance_id": channel_id_str,
                }),
                severity: AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            });
            // Issue a fresh pairing code so the operator can self-
            // onboard with `/pair <code>` from this same channel.
            // Without this UX, paired-sender enforcement is a dead-end
            // and the entire approval flow becomes a no-op (R3 finding C1).
            let code = self
                .pairing_manager
                .generate_code(&channel_id_str, &msg.external_sender_id)
                .await;
            self.send_reply(
                msg,
                format!(
                    "🔒 This sender is not paired with AgentOS. Reply \
                     `/pair {code}` to authorise approval commands. Code \
                     expires in 10 minutes."
                ),
            )
            .await;
            return;
        }

        // Look up the escalation. Idempotent: already-resolved → friendly
        // reply, no state change.
        let existing = self.escalation_manager.get(id).await;
        let Some(esc) = existing else {
            self.send_reply(msg, format!("No escalation #{id} found."))
                .await;
            return;
        };
        if esc.resolved {
            self.send_reply(
                msg,
                format!(
                    "Escalation #{id} is already resolved ({}).",
                    esc.resolution.as_deref().unwrap_or("unknown")
                ),
            )
            .await;
            return;
        }

        let resolution = if cmd == "/approve" {
            "approved"
        } else {
            "denied"
        };
        match self.escalation_manager.resolve(id, resolution.into()).await {
            Some((_task_id, _agent_id, _blocking)) => {
                tracing::info!(
                    escalation_id = id,
                    resolution,
                    channel_id = %channel_id_str,
                    sender = %msg.external_sender_id,
                    "Escalation resolved via channel"
                );
                let event = if resolution == "approved" {
                    AuditEventType::PermissionGranted
                } else {
                    AuditEventType::PermissionDenied
                };
                let _ = self.audit.append(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: event,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "subsystem": "inbound_router",
                        "command": cmd,
                        "escalation_id": id,
                        "resolution": resolution,
                        "channel_instance_id": channel_id_str,
                        "channel_sender": msg.external_sender_id,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                let symbol = if resolution == "approved" {
                    "✅"
                } else {
                    "🚫"
                };
                // ACF Phase 4: `EscalationManager::resolve` now sends on
                // a oneshot resolution channel that `task_executor`
                // parks on whenever ApprovalHook returns
                // `approval_pending:<id>`. The agent's tool call resumes
                // automatically — no need for the operator to re-issue.
                let suffix = if resolution == "approved" {
                    " The agent's tool call is resuming."
                } else {
                    ""
                };
                self.send_reply(
                    msg,
                    format!("{symbol} Escalation #{id} {resolution}.{suffix}"),
                )
                .await;
            }
            None => {
                // Race with another approver (web UI, sweeper) — already resolved.
                self.send_reply(
                    msg,
                    format!("Escalation #{id} is already resolved or expired."),
                )
                .await;
            }
        }
    }

    async fn send_reply(&self, original: &InboundMessage, text: String) {
        let trace_id = TraceID::new();
        let preview: String = text.chars().take(120).collect();
        let text_len = text.chars().count();
        let subject: String = text.chars().take(80).collect();
        let reply = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Kernel,
            task_id: None,
            trace_id,
            kind: UserMessageKind::Notification,
            priority: NotificationPriority::Info,
            subject,
            body: text,
            interaction: None,
            delivery_status: Default::default(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: Some(format!("channel:{}", original.channel_instance_id)),
            reply_to_external_id: None,
            attachment: None,
        };
        // Route back to the originating channel only — not all registered adapters.
        let instance_id = original.channel_instance_id.to_string();
        match self
            .notification_router
            .deliver_to_channel(reply, &instance_id)
            .await
        {
            Ok(()) => {
                let _ = self.audit.append(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: AuditEventType::ChannelMessageSent,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "channel_id": instance_id,
                        "channel": original.channel,
                        "source": "kernel_command_reply",
                        "text_preview": preview,
                        "text_len": text_len,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
            }
            Err(e) => {
                tracing::warn!(
                    channel = %original.channel,
                    error = %e,
                    "InboundRouter: failed to deliver reply"
                );
            }
        }
    }

    async fn send_agent_chat_reply(&self, original: &InboundMessage, agent_name: &str, body: &str) {
        let agent_id_opt = self.chat_bridge.agent_id_for_name(agent_name).await;
        let from = match agent_id_opt {
            Some(id) => NotificationSource::Agent(id),
            None => NotificationSource::Kernel,
        };
        let trace_id = TraceID::new();
        let preview: String = body.chars().take(120).collect();
        let text_len = body.chars().count();
        let subject = format!("[{agent_name}]")
            .chars()
            .take(80)
            .collect::<String>();
        let reply = UserMessage {
            id: NotificationID::new(),
            from,
            task_id: None,
            trace_id,
            kind: UserMessageKind::Notification,
            priority: NotificationPriority::Info,
            subject,
            body: body.to_string(),
            interaction: None,
            delivery_status: Default::default(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: Some(format!("channel:{}", original.channel_instance_id)),
            reply_to_external_id: None,
            attachment: None,
        };
        // Route back to the originating channel only — not all registered adapters.
        let instance_id = original.channel_instance_id.to_string();
        match self
            .notification_router
            .deliver_to_channel(reply, &instance_id)
            .await
        {
            Ok(()) => {
                let _ = self.audit.append(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: AuditEventType::ChannelMessageSent,
                    agent_id: agent_id_opt,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "channel_id": instance_id,
                        "channel": original.channel,
                        "source": "agent_chat_reply",
                        "agent_name": agent_name,
                        "text_preview": preview,
                        "text_len": text_len,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
            }
            Err(e) => {
                tracing::warn!(
                    channel = %original.channel,
                    error = %e,
                    "InboundRouter: failed to deliver agent chat reply"
                );
            }
        }
    }
}
