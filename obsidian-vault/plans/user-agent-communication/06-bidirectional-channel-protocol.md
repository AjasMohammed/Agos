---
title: "Phase 6: Bidirectional Channel Protocol"
tags:
  - kernel
  - tools
  - telegram
  - ntfy
  - plan
  - phase-6
date: 2026-03-24
status: complete
effort: 4d
priority: high
---

# Phase 6: Bidirectional Channel Protocol

> The critical phase that transforms UNIS from a notification system into a true communication channel. Introduces `UserChannelRegistry` (ecosystem-wide channel awareness), `ChannelListener` (inbound message reception), `InboundRouter` (user message → kernel command mapping), and the Telegram + ntfy + email bidirectional adapters. After this phase, users can message the AgentOS ecosystem from their phone, and agents know their channel exists before deciding to reach out.

**Depends on**: [[01-user-message-type-and-router]] (Phase 1), [[05-pluggable-delivery-adapters]] (Phase 5 — adapters must exist)
**Blocks**: Nothing (final leaf)

---

## Why This Phase

Phases 1–5 build an outbound notification system. They solve "agents can push to user". This phase solves the other half: "user can push to agents, and the entire ecosystem knows the user has a channel".

Without Phase 6:
- Agents cannot know if a channel exists before trying to notify
- Users must always be at a terminal or browser to respond — no mobile response
- Users cannot initiate: "run agent X on file Y" from Telegram is impossible
- The channel is static config (`default.toml`), not a dynamic connected relationship

With Phase 6:
- User runs `agentctl channel connect telegram` once → permanent bidirectional channel
- Agent calls `notify_user(...)` → kernel checks `UserChannelRegistry` → delivers to Telegram
- User replies in Telegram → `ChannelListener` receives → `InboundRouter` routes → task resumes or new task created
- User sends `/tasks` from Telegram → gets live task list back in the same chat
- Everything is ecosystem-aware, not config-file-dependent

---

## Current State vs. Target

| Item | Current | Target |
|------|---------|--------|
| `UserChannelRegistry` | Does not exist | Kernel entity, vault-persisted, queried by all agents |
| `ChannelListener` trait | Does not exist | Background task per channel (Telegram long-poll, ntfy SSE, IMAP IDLE) |
| `InboundRouter` | Does not exist | Maps user messages to KernelCommand or new Task |
| `agentctl channel` | Does not exist | `connect`, `list`, `disconnect`, `status` subcommands |
| `TelegramAdapter` (bidirectional) | Phase 5: outbound webhook only | Full bidirectional via `teloxide` or raw Bot API |
| `NtfyAdapter` (bidirectional) | Phase 5: outbound via HTTP POST | Bidirectional via action button callbacks + topic subscription |
| `EmailAdapter` (bidirectional) | Phase 5: outbound SMTP | Bidirectional via IMAP IDLE reply detection |
| `DeliveryAdapter` trait | Outbound only | Extended with `listen()` returning inbound stream |

---

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                       AGENTOS KERNEL                          │
│                                                               │
│  ┌──────────────────────────────────────────────────────┐   │
│  │              UserChannelRegistry                      │   │
│  │  { user_id: UserID →                                  │   │
│  │      channels: Vec<RegisteredChannel> }               │   │
│  │                                                       │   │
│  │  RegisteredChannel {                                  │   │
│  │    id: ChannelInstanceID,                             │   │
│  │    kind: ChannelKind,        // Telegram/Ntfy/Email   │   │
│  │    external_id: String,      // chat_id / topic / addr│   │
│  │    display_name: String,     // e.g. "@username"      │   │
│  │    connected_at: DateTime,                            │   │
│  │    last_active: DateTime,                             │   │
│  │    active: bool,                                      │   │
│  │  }                                                    │   │
│  └────────────────────────────────────────────────────── ┘   │
│           │                          │                        │
│    (outbound)                  (inbound)                      │
│    NotificationRouter          ChannelListenerRegistry        │
│           │                          │                        │
│    DeliveryAdapter::deliver()  DeliveryAdapter::listen()      │
│           │                          │                        │
│    TelegramAdapter            InboundRouter                   │
│    NtfyAdapter                  │                             │
│    EmailAdapter              ┌──┴───────────────────────┐    │
│                              │  /tasks → ListTasks       │    │
│                              │  /stop X → CancelTask     │    │
│                              │  /run X → NewTask         │    │
│                              │  "yes"/"no" → UserResponse│    │
│                              │  free text → LLM intent   │    │
│                              └───────────────────────────┘    │
└──────────────────────────────────────────────────────────────┘
```

---

## Detailed Subtasks

### 6.1 — `UserChannelRegistry`

**File**: `crates/agentos-kernel/src/user_channel_registry.rs` (new file)

```rust
pub struct UserChannelRegistry {
    db: SqlitePool,   // reuse agentos-audit's sqlx pattern
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredChannel {
    pub id: ChannelInstanceID,
    pub kind: ChannelKind,
    pub external_id: String,    // Telegram: chat_id; ntfy: topic; email: address
    pub display_name: String,   // human-readable: "@johndoe" or "john@example.com"
    pub bot_token: Option<String>, // stored encrypted in vault, not here
    pub connected_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelKind {
    Telegram,
    Ntfy,
    Email,
    Custom(String),
}

impl UserChannelRegistry {
    pub async fn register(&self, channel: RegisteredChannel) -> Result<(), AgentOSError>;
    pub async fn deregister(&self, id: &ChannelInstanceID) -> Result<(), AgentOSError>;
    pub async fn list_active(&self) -> Result<Vec<RegisteredChannel>, AgentOSError>;
    pub async fn has_any_active(&self) -> Result<bool, AgentOSError>;
    pub async fn get_by_kind(&self, kind: &ChannelKind) -> Result<Option<RegisteredChannel>, AgentOSError>;
    pub async fn update_last_active(&self, id: &ChannelInstanceID) -> Result<(), AgentOSError>;
}
```

Schema:
```sql
CREATE TABLE IF NOT EXISTS user_channels (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    external_id  TEXT NOT NULL,
    display_name TEXT NOT NULL,
    connected_at TEXT NOT NULL,
    last_active  TEXT NOT NULL,
    active       INTEGER NOT NULL DEFAULT 1
);
```

Sensitive data (bot tokens, passwords) is stored in `agentos-vault`, not this table. The registry stores only non-sensitive routing metadata.

---

### 6.2 — Extend `DeliveryAdapter` trait with `listen()`

**File**: `crates/agentos-kernel/src/notification_router.rs`

```rust
#[async_trait]
pub trait DeliveryAdapter: Send + Sync {
    fn channel_id(&self) -> DeliveryChannel;

    // OUTBOUND (existing)
    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError>;
    async fn is_available(&self) -> bool;

    // INBOUND (new in Phase 6)
    /// Whether this adapter supports receiving inbound messages.
    fn supports_inbound(&self) -> bool { false }

    /// Start the listener. Returns a stream of inbound messages.
    /// Only called if supports_inbound() returns true.
    async fn listen(&self) -> Result<Pin<Box<dyn Stream<Item = InboundMessage> + Send>>, DeliveryError> {
        Err(DeliveryError::NotSupported)
    }
}

#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub channel: DeliveryChannel,
    pub external_sender_id: String,   // Telegram chat_id, email address, etc.
    pub text: String,
    pub reply_to_notification_id: Option<NotificationID>,  // if this is a reply to a known message
    pub received_at: DateTime<Utc>,
    pub raw: serde_json::Value,        // adapter-specific raw data for debugging
}
```

---

### 6.3 — `ChannelListenerRegistry` + listener lifecycle

**File**: `crates/agentos-kernel/src/user_channel_registry.rs`

The kernel maintains a registry of running listener tasks. When a channel is connected, a listener task is spawned. When disconnected, it's cancelled.

```rust
pub struct ChannelListenerRegistry {
    listeners: Arc<RwLock<HashMap<ChannelInstanceID, tokio::task::JoinHandle<()>>>>,
    inbound_tx: mpsc::Sender<InboundMessage>,
}

impl ChannelListenerRegistry {
    pub async fn start_listener(
        &self,
        channel_id: ChannelInstanceID,
        adapter: Arc<dyn DeliveryAdapter>,
    ) -> Result<(), AgentOSError> {
        if !adapter.supports_inbound() { return Ok(()); }
        let tx = self.inbound_tx.clone();
        let handle = tokio::spawn(async move {
            match adapter.listen().await {
                Ok(mut stream) => {
                    while let Some(msg) = stream.next().await {
                        if tx.send(msg).await.is_err() { break; }
                    }
                }
                Err(e) => tracing::error!("Channel listener failed: {e}"),
            }
        });
        self.listeners.write().await.insert(channel_id, handle);
        Ok(())
    }

    pub async fn stop_listener(&self, channel_id: &ChannelInstanceID) {
        if let Some(handle) = self.listeners.write().await.remove(channel_id) {
            handle.abort();
        }
    }
}
```

The `inbound_tx` feeds into a single `mpsc::Receiver<InboundMessage>` consumed by the `InboundRouter`.

---

### 6.4 — `InboundRouter`

**File**: `crates/agentos-kernel/src/inbound_router.rs` (new file)

The inbound router runs as a background task, consuming messages from all channel listeners and routing them to the appropriate kernel handler.

```rust
pub struct InboundRouter {
    kernel: Arc<Kernel>,
    rx: mpsc::Receiver<InboundMessage>,
}

impl InboundRouter {
    pub async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            if let Err(e) = self.route(msg).await {
                tracing::warn!("InboundRouter: failed to route message: {e}");
            }
        }
    }

    async fn route(&self, msg: InboundMessage) -> Result<(), AgentOSError> {
        // 1. Update channel last_active
        self.kernel.channel_registry.update_last_active_by_external(&msg.channel, &msg.external_sender_id).await?;

        // 2. Check if this is a reply to a pending notification
        if let Some(notif_id) = self.find_pending_question(&msg).await? {
            return self.handle_question_reply(msg, notif_id).await;
        }

        // 3. Try parsing as a slash command
        if msg.text.starts_with('/') {
            return self.handle_slash_command(msg).await;
        }

        // 4. Natural language fallback → LLM intent parse → new task or query
        self.handle_natural_language(msg).await
    }

    async fn handle_slash_command(&self, msg: InboundMessage) -> Result<(), AgentOSError> {
        let parts: Vec<&str> = msg.text.splitn(3, ' ').collect();
        match parts[0] {
            "/tasks" | "/status" => {
                let tasks = self.kernel.list_tasks().await?;
                let reply = format_task_list(&tasks);
                self.reply_to_channel(&msg, reply).await
            }
            "/stop" if parts.len() > 1 => {
                let task_id: TaskID = parts[1].parse()?;
                self.kernel.cancel_task(task_id).await?;
                self.reply_to_channel(&msg, "Task cancelled.".into()).await
            }
            "/agents" => {
                let agents = self.kernel.list_agents().await?;
                self.reply_to_channel(&msg, format_agent_list(&agents)).await
            }
            "/run" if parts.len() > 1 => {
                let prompt = parts[1..].join(" ");
                let task_id = self.kernel.create_task_from_channel(&msg, prompt).await?;
                self.reply_to_channel(&msg, format!("Task started: {task_id}")).await
            }
            "/help" => {
                self.reply_to_channel(&msg, HELP_TEXT.into()).await
            }
            _ => {
                self.reply_to_channel(&msg, "Unknown command. Send /help for available commands.".into()).await
            }
        }
    }

    async fn handle_natural_language(&self, msg: InboundMessage) -> Result<(), AgentOSError> {
        // Route to the user's default agent (or the last active task's agent)
        // as a new task with the message text as the prompt.
        // This makes the channel feel like a persistent conversation.
        let default_agent = self.kernel.get_default_agent().await?;
        let task_id = self.kernel.create_task_from_channel(&msg, msg.text.clone()).await?;
        self.reply_to_channel(&msg, format!("Got it. Working on it... (task {})", &task_id.to_string()[..8])).await
    }

    async fn handle_question_reply(&self, msg: InboundMessage, notif_id: NotificationID) -> Result<(), AgentOSError> {
        let response = UserResponse {
            text: msg.text.clone(),
            responded_at: msg.received_at,
            channel: msg.channel.clone(),
        };
        self.kernel.notification_router.route_response(&notif_id, response).await?;
        self.reply_to_channel(&msg, "Response sent to agent.".into()).await
    }

    async fn reply_to_channel(&self, original: &InboundMessage, text: String) -> Result<(), AgentOSError> {
        // Build a UserMessage and deliver back via the same channel
        // Use thread_id = original.raw's message_id for Telegram threading
        let msg = UserMessage {
            kind: UserMessageKind::Notification,
            subject: text.chars().take(80).collect(),
            body: text,
            priority: NotificationPriority::Info,
            // ... other fields
        };
        self.kernel.notification_router.deliver_to_channel(msg, &original.channel).await
    }
}
```

---

### 6.5 — Telegram Bidirectional Adapter

**File**: `crates/agentos-kernel/src/adapters/telegram.rs` (new file)

Use raw `reqwest` calls to the Telegram Bot API (avoids `teloxide` as a heavy dependency for now; can upgrade later).

```rust
pub struct TelegramAdapter {
    bot_token: String,
    chat_id: String,         // from UserChannelRegistry.external_id
    client: reqwest::Client,
}

// OUTBOUND: same as before — sendMessage + InlineKeyboard
#[async_trait]
impl DeliveryAdapter for TelegramAdapter {
    fn channel_id(&self) -> DeliveryChannel { DeliveryChannel::Telegram }
    fn supports_inbound(&self) -> bool { true }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        let text = format_telegram_message(msg);
        let reply_markup = build_inline_keyboard(msg);  // for Question kind

        let payload = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "Markdown",
            "reply_markup": reply_markup,
        });

        self.client
            .post(format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token))
            .json(&payload)
            .send()
            .await
            .map_err(|e| DeliveryError::Transient(e.to_string()))?
            .error_for_status()
            .map_err(|e| DeliveryError::Transient(e.to_string()))?;
        Ok(())
    }

    // INBOUND: long-polling getUpdates
    async fn listen(&self) -> Result<Pin<Box<dyn Stream<Item = InboundMessage> + Send>>, DeliveryError> {
        let token = self.bot_token.clone();
        let chat_id = self.chat_id.clone();
        let client = self.client.clone();

        let stream = async_stream::stream! {
            let mut offset: i64 = 0;
            loop {
                let url = format!(
                    "https://api.telegram.org/bot{}/getUpdates?offset={}&timeout=30",
                    token, offset
                );
                match client.get(&url).send().await {
                    Ok(resp) => {
                        if let Ok(updates) = resp.json::<TelegramUpdates>().await {
                            for update in updates.result {
                                offset = update.update_id + 1;
                                // Only process messages from our registered chat
                                if let Some(msg) = update.message {
                                    if msg.chat.id.to_string() == chat_id {
                                        yield InboundMessage {
                                            channel: DeliveryChannel::Telegram,
                                            external_sender_id: chat_id.clone(),
                                            text: msg.text.unwrap_or_default(),
                                            reply_to_notification_id: None, // matched later by InboundRouter
                                            received_at: Utc::now(),
                                            raw: serde_json::to_value(&msg).unwrap_or_default(),
                                        };
                                    }
                                }
                                // Handle callback_query (inline button taps)
                                if let Some(cq) = update.callback_query {
                                    if cq.message.chat.id.to_string() == chat_id {
                                        yield InboundMessage {
                                            channel: DeliveryChannel::Telegram,
                                            external_sender_id: chat_id.clone(),
                                            text: cq.data.unwrap_or_default(),
                                            reply_to_notification_id: None,
                                            received_at: Utc::now(),
                                            raw: serde_json::to_value(&cq).unwrap_or_default(),
                                        };
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Telegram long-poll error: {e}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn is_available(&self) -> bool { true }
}
```

**Inline keyboard for Questions** (enables button-tap replies in Telegram):
```rust
fn build_inline_keyboard(msg: &UserMessage) -> serde_json::Value {
    if let UserMessageKind::Question { options: Some(opts), .. } = &msg.kind {
        let buttons: Vec<Vec<serde_json::Value>> = opts.chunks(2)
            .map(|row| row.iter().map(|opt| serde_json::json!({
                "text": opt,
                "callback_data": opt,
            })).collect())
            .collect();
        serde_json::json!({ "inline_keyboard": buttons })
    } else {
        serde_json::Value::Null
    }
}
```

---

### 6.6 — ntfy Bidirectional Adapter

**File**: `crates/agentos-kernel/src/adapters/ntfy.rs`

ntfy action buttons + webhook callback for structured replies. Free-text replies via topic subscription.

```rust
pub struct NtfyAdapter {
    server_url: String,      // https://ntfy.sh or self-hosted
    topic: String,           // outbound topic: agentos-{user_id}
    reply_topic: String,     // inbound topic: agentos-{user_id}-reply
    access_token: Option<String>,
    webhook_base_url: Option<String>,  // AgentOS webhook for action button callbacks
    client: reqwest::Client,
}

impl DeliveryAdapter for NtfyAdapter {
    fn supports_inbound(&self) -> bool { true }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        let mut req = self.client.put(format!("{}/{}", self.server_url, self.topic))
            .header("Title", &msg.subject)
            .header("Priority", priority_to_ntfy(&msg.priority))
            .body(msg.body.clone());

        // Action buttons for Questions
        if let UserMessageKind::Question { options: Some(opts), .. } = &msg.kind {
            let actions: Vec<String> = opts.iter().enumerate().map(|(i, opt)| {
                if let Some(base) = &self.webhook_base_url {
                    // HTTP action: tap button → POST to AgentOS webhook
                    format!(
                        "http, {opt}, {base}/channel/ntfy/respond?notif={}&choice={opt}, method=POST",
                        msg.id
                    )
                } else {
                    // Fallback: view action → opens reply topic instructions
                    format!("view, {opt}, {}/{}", self.server_url, self.reply_topic)
                }
            }).collect();
            req = req.header("Actions", actions.join("; "));
        }

        req.send().await.map_err(|e| DeliveryError::Transient(e.to_string()))?
           .error_for_status().map_err(|e| DeliveryError::Transient(e.to_string()))?;
        Ok(())
    }

    // INBOUND: subscribe to reply topic via SSE
    async fn listen(&self) -> Result<Pin<Box<dyn Stream<Item = InboundMessage> + Send>>, DeliveryError> {
        let url = format!("{}/{}/sse", self.server_url, self.reply_topic);
        let client = self.client.clone();
        let stream = async_stream::stream! {
            loop {
                match client.get(&url).send().await {
                    Ok(resp) => {
                        let mut body = resp.bytes_stream();
                        while let Some(chunk) = body.next().await {
                            if let Ok(bytes) = chunk {
                                // Parse SSE events: "data: {...}" lines
                                if let Ok(text) = std::str::from_utf8(&bytes) {
                                    for line in text.lines() {
                                        if let Some(data) = line.strip_prefix("data: ") {
                                            if let Ok(event) = serde_json::from_str::<NtfyEvent>(data) {
                                                yield InboundMessage {
                                                    channel: DeliveryChannel::Ntfy,
                                                    external_sender_id: event.topic.clone(),
                                                    text: event.message,
                                                    reply_to_notification_id: None,
                                                    received_at: Utc::now(),
                                                    raw: serde_json::Value::Null,
                                                };
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("ntfy SSE error: {e}");
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }
}
```

---

### 6.7 — Email Bidirectional Adapter

**File**: `crates/agentos-kernel/src/adapters/email.rs`

Uses `lettre` for SMTP sending and `async-imap` for IMAP IDLE reply detection.

```rust
pub struct EmailAdapter {
    smtp_host: String,
    smtp_user: String,
    smtp_password: String,  // fetched from vault at startup
    from_address: String,
    to_address: String,
    imap_host: String,
    imap_port: u16,
}

impl DeliveryAdapter for EmailAdapter {
    fn supports_inbound(&self) -> bool { true }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        use lettre::{Message, SmtpTransport, Transport};
        let email = Message::builder()
            .from(self.from_address.parse()?)
            .to(self.to_address.parse()?)
            .subject(&msg.subject)
            // Set X-AgentOS-NotifID header for reply threading
            .header(("X-AgentOS-NotifID", msg.id.to_string()))
            .body(msg.body.clone())?;
        // ... send via SMTP
        Ok(())
    }

    // INBOUND: IMAP IDLE — notified when new mail arrives
    async fn listen(&self) -> Result<Pin<Box<dyn Stream<Item = InboundMessage> + Send>>, DeliveryError> {
        // Use async-imap with IDLE extension
        // When a new email arrives in INBOX from the user:
        //   - parse In-Reply-To header to find notification_id
        //   - yield InboundMessage with the email body as text
        // ...
    }
}
```

---

### 6.8 — `agentctl channel` CLI subcommands

**File**: `crates/agentos-cli/src/commands/channel.rs` (new file)

```
agentctl channel connect telegram
  → Prints: "Open Telegram, find @AgentOSBot, send /start"
  → Starts awaiting /start message (long-poll with timeout)
  → On receipt: registers channel, stores chat_id, confirms to user

agentctl channel connect ntfy [--server https://ntfy.sh] [--topic myagentostopic]
  → Registers ntfy channel
  → Prints: "Subscribe to <server>/<topic> in the ntfy app"

agentctl channel connect email --smtp-host ... --imap-host ... --to user@example.com
  → Registers email channel
  → Sends test email to verify connectivity

agentctl channel list
  → Table: ID, Kind, DisplayName, ConnectedAt, LastActive, Status

agentctl channel disconnect <channel-id>
  → Deregisters, stops listener, removes from registry

agentctl channel test <channel-id>
  → Sends a test notification to verify the channel works
```

---

### 6.9 — Telegram bot setup command

The Telegram setup flow requires a bot token. Rather than hard-coding one, each AgentOS installation creates its own bot via `@BotFather`. The setup command guides the user through this:

```
agentctl channel connect telegram

Step 1: Create your Telegram bot
  1. Open Telegram and find @BotFather
  2. Send: /newbot
  3. Choose a name (e.g. "My AgentOS")
  4. Choose a username (e.g. myagentosbot)
  5. Copy the bot token BotFather gives you

Enter bot token: [user pastes token]

Step 2: Connect your account
  1. Open Telegram
  2. Find your bot: @myagentosbot
  3. Send: /start

Waiting for /start... (timeout 5 minutes)
[user sends /start in Telegram]

✓ Connected! Your Telegram channel is now active.
  Bot: @myagentosbot
  Chat ID: 123456789
  Test: agentctl channel test telegram
```

The bot token is stored in `agentos-vault` (encrypted). The `chat_id` is stored in `UserChannelRegistry`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/user_channel_registry.rs` | NEW — UserChannelRegistry, RegisteredChannel, ChannelListenerRegistry |
| `crates/agentos-kernel/src/inbound_router.rs` | NEW — InboundRouter with slash commands + NLP fallback |
| `crates/agentos-kernel/src/adapters/telegram.rs` | NEW — TelegramAdapter with long-poll listen() |
| `crates/agentos-kernel/src/adapters/ntfy.rs` | NEW — NtfyAdapter with SSE listen() + action button callbacks |
| `crates/agentos-kernel/src/adapters/email.rs` | NEW — EmailAdapter with IMAP IDLE listen() |
| `crates/agentos-kernel/src/notification_router.rs` | Add `listen()` to DeliveryAdapter trait; add `deliver_to_channel()` |
| `crates/agentos-kernel/src/kernel.rs` | Add `channel_registry`, `listener_registry`, `inbound_router` fields; start listeners on boot |
| `crates/agentos-bus/src/message.rs` | Add `KernelCommand::ConnectChannel`, `DisconnectChannel`, `ListChannels`, `TestChannel` |
| `crates/agentos-cli/src/commands/channel.rs` | NEW — channel subcommands |
| `crates/agentos-cli/src/main.rs` | Register channel subcommand group |
| `crates/agentos-kernel/Cargo.toml` | Add `async-imap`, `lettre` dependencies |
| `config/default.toml` | Add `[channels]` section |

---

## Test Plan

```rust
#[tokio::test]
async fn test_channel_registry_persists_across_restart() {
    let registry = UserChannelRegistry::new(temp_db()).await.unwrap();
    registry.register(make_telegram_channel("123456")).await.unwrap();
    drop(registry);
    // re-open
    let registry2 = UserChannelRegistry::new(same_db()).await.unwrap();
    let channels = registry2.list_active().await.unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].external_id, "123456");
}

#[tokio::test]
async fn test_inbound_router_routes_slash_command() {
    let mut router = setup_inbound_router().await;
    let msg = make_inbound("/tasks", DeliveryChannel::Telegram);
    // handle the message
    router.route(msg).await.unwrap();
    // a reply should have been sent back via the channel
    // assert the reply contains task list text
}

#[tokio::test]
async fn test_inbound_router_routes_question_reply() {
    // pre-existing pending Question notification
    // inbound message text = "yes"
    // assert: UserResponse sent to waiting task
}

#[tokio::test]
async fn test_telegram_listen_yields_inbound_message() {
    // mock Telegram getUpdates endpoint
    // adapter.listen() → stream should yield one InboundMessage
    // text matches the mock message text
}

#[tokio::test]
async fn test_ntfy_listen_yields_inbound_on_sse_event() {
    // mock ntfy SSE endpoint
    // adapter.listen() → stream yields one InboundMessage
}

#[tokio::test]
async fn test_channel_connect_flow() {
    // test the connect + /start handshake flow
    // mock Telegram: first getUpdates returns /start message
    // assert: channel registered with correct chat_id
}
```

---

## Verification

```bash
# Build
cargo build -p agentos-kernel -p agentos-cli

# Connect Telegram (interactive)
agentctl channel connect telegram
# → follow prompts, send /start from Telegram
# → "✓ Connected"

agentctl channel list
# telegram   @mybot / 123456789   connected 30sec ago

# Send a test notification from CLI
agentctl channel test telegram
# → receive "Test notification from AgentOS" in Telegram

# Run a task and check it notifies on completion
agentctl task run --agent my-agent "say hello"
# → receive "✓ Task completed: say hello" in Telegram

# Send /tasks from Telegram
# → receive task list in reply

# Run a task that uses ask-user
agentctl task run --agent my-agent "ask me something"
# → receive question in Telegram with buttons
# → tap a button
# → receive "Response sent to agent" confirmation
```
