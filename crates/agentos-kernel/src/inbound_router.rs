use crate::notification_router::{InboundMessage, NotificationRouter};
use crate::scheduler::TaskScheduler;
use crate::user_channel_registry::UserChannelRegistry;
use agentos_types::{
    AgentOSError, ChannelInstanceID, NotificationID, NotificationPriority, NotificationSource,
    TaskState, TraceID, UserMessage, UserMessageKind, UserResponse,
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

Free-text messages are acknowledged and stored in your inbox.";

/// Maximum inbound messages accepted per channel per minute.
const INBOUND_RATE_LIMIT: u32 = 30;

/// Routes inbound messages from external bidirectional channels to the
/// appropriate kernel subsystem.
///
/// Runs as a background task consuming `InboundMessage`s forwarded by
/// `ChannelListenerRegistry`.  Handles:
/// 1. **Question replies** — routes to `NotificationRouter::route_response`.
/// 2. **Slash commands** — `/tasks`, `/status`, `/stop`, `/help`.
/// 3. **Free-text** — acknowledges the message and stores it in the inbox.
pub struct InboundRouter {
    notification_router: Arc<NotificationRouter>,
    channel_registry: Arc<UserChannelRegistry>,
    scheduler: Arc<TaskScheduler>,
    rx: mpsc::Receiver<InboundMessage>,
    /// Per-channel rate limiter: (message count, window start instant).
    rate_limiter: HashMap<ChannelInstanceID, (u32, Instant)>,
}

impl InboundRouter {
    pub fn new(
        notification_router: Arc<NotificationRouter>,
        channel_registry: Arc<UserChannelRegistry>,
        scheduler: Arc<TaskScheduler>,
        rx: mpsc::Receiver<InboundMessage>,
    ) -> Self {
        Self {
            notification_router,
            channel_registry,
            scheduler,
            rx,
            rate_limiter: HashMap::new(),
        }
    }

    /// Run the router loop until the sender side is dropped (kernel shutdown).
    pub async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            if let Err(e) = self.route(msg).await {
                tracing::warn!("InboundRouter: routing error: {e}");
            }
        }
        // Periodically prune stale rate limiter entries. The map is small in practice
        // (one entry per channel) so we prune inline after the loop ends; the entries
        // are also pruned on every message receive above.
    }

    async fn route(&mut self, msg: InboundMessage) -> Result<(), AgentOSError> {
        // Prune stale rate limiter entries (older than 5 minutes) to bound memory use.
        const PRUNE_THRESHOLD: usize = 64;
        if self.rate_limiter.len() > PRUNE_THRESHOLD {
            self.rate_limiter
                .retain(|_, (_, ts)| ts.elapsed().as_secs() < 300);
        }

        // Per-channel rate limiting: drop messages exceeding INBOUND_RATE_LIMIT/min.
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

        // Update last_active timestamp for the channel.
        self.channel_registry
            .update_last_active(&msg.channel_instance_id)
            .await
            .ok(); // non-fatal

        // Check if this is a reply to a pending Question notification.
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

        // Slash command handling.
        if msg.text.starts_with('/') {
            return self.handle_slash_command(msg).await;
        }

        // Auto-route free-text to a waiting task when exactly one blocking question
        // is outstanding.  External channel adapters cannot set reply_to_notification_id,
        // so this fallback is the primary path for answering ask_user questions via
        // Telegram or ntfy.
        //
        // Security: only auto-route from channels that have a credential_key (i.e. are
        // authenticated). Unauthenticated channels (e.g. ntfy without an access token)
        // could allow third-party answer injection.
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

        // Free-text fallback: acknowledge and store as an inbox notification.
        self.send_reply(
            &msg,
            "Message received. Use /help for available commands.".to_string(),
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

    /// Send a reply back to the user via the inbox (visible on all channels).
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
        if let Err(e) = self.notification_router.deliver(reply).await {
            tracing::warn!(
                channel = %original.channel,
                error = %e,
                "InboundRouter: failed to deliver reply"
            );
        }
    }
}
