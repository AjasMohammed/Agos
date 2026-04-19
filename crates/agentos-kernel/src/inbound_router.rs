use crate::channel_chat_bridge::KernelChatBridge;
use crate::notification_router::{InboundMessage, NotificationRouter};
use crate::scheduler::TaskScheduler;
use crate::user_channel_registry::UserChannelRegistry;
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
    rx: mpsc::Receiver<InboundMessage>,
    /// Per-channel rate limiter: (message count, window start instant).
    rate_limiter: HashMap<ChannelInstanceID, (u32, Instant)>,
    /// Last time the rate limiter map was pruned of stale entries.
    last_prune: Instant,
}

impl InboundRouter {
    pub fn new(
        notification_router: Arc<NotificationRouter>,
        channel_registry: Arc<UserChannelRegistry>,
        scheduler: Arc<TaskScheduler>,
        chat_bridge: Arc<KernelChatBridge>,
        rx: mpsc::Receiver<InboundMessage>,
    ) -> Self {
        Self {
            notification_router,
            channel_registry,
            scheduler,
            chat_bridge,
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

    async fn route(&mut self, msg: InboundMessage) -> Result<(), AgentOSError> {
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
                        match self
                            .chat_bridge
                            .channel_chat(msg.channel_instance_id, agent, text)
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
                    .channel_chat(msg.channel_instance_id, agent, prompt)
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

    async fn send_reply(&self, original: &InboundMessage, text: String) {
        let subject: String = text.chars().take(80).collect();
        let reply = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Kernel,
            task_id: None,
            trace_id: TraceID::new(),
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
        };
        // Route back to the originating channel only — not all registered adapters.
        let instance_id = original.channel_instance_id.to_string();
        if let Err(e) = self
            .notification_router
            .deliver_to_channel(reply, &instance_id)
            .await
        {
            tracing::warn!(
                channel = %original.channel,
                error = %e,
                "InboundRouter: failed to deliver reply"
            );
        }
    }

    async fn send_agent_chat_reply(&self, original: &InboundMessage, agent_name: &str, body: &str) {
        let from = if let Some(id) = self.chat_bridge.agent_id_for_name(agent_name).await {
            NotificationSource::Agent(id)
        } else {
            NotificationSource::Kernel
        };
        let subject = format!("[{agent_name}]")
            .chars()
            .take(80)
            .collect::<String>();
        let reply = UserMessage {
            id: NotificationID::new(),
            from,
            task_id: None,
            trace_id: TraceID::new(),
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
        };
        // Route back to the originating channel only — not all registered adapters.
        let instance_id = original.channel_instance_id.to_string();
        if let Err(e) = self
            .notification_router
            .deliver_to_channel(reply, &instance_id)
            .await
        {
            tracing::warn!(
                channel = %original.channel,
                error = %e,
                "InboundRouter: failed to deliver agent chat reply"
            );
        }
    }
}
