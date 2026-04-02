---
title: "Phase 1.2: Channel Adapter System"
tags:
  - kernel
  - channels
  - v3
  - plan
  - phase-1
date: 2026-03-30
status: planned
effort: 5d
priority: critical
---

# Phase 1.2: Channel Adapter System

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create `agentos-channels` crate with a `ChannelAdapter` trait and 6 bidirectional adapters: Discord, Slack, Telegram, WhatsApp, Email, Webhook.

**Architecture:** Each adapter normalizes platform-specific messages to a unified `ChannelMessage` type. Inbound messages are forwarded to the kernel via `mpsc::Sender<InboundMessage>`. Outbound messages go through the adapter's `send()` method. A `ChannelManager` in the kernel registers adapters and routes messages to/from agents.

**Tech Stack:** tokio, reqwest, tokio-tungstenite (Discord gateway), lettre (email), agentos-types

---

## Why This Phase

AgentOS has zero bidirectional messaging. OpenFang has 40 channels. OpenClaw works through WhatsApp, Telegram, and 30+ platforms. Without channels, no end user can interact with AgentOS agents through their existing messaging apps.

## Current → Target State

**Current:** Notification-only adapters (Telegram, ntfy, email) in `notification_router.rs`. The existing `DeliveryAdapter` trait supports outbound delivery + optional `start_listening` for inbound. `UserChannelRegistry` stores channel credentials in SQLite. `ChannelKind` enum has Telegram, Ntfy, Email, Custom.

**Target:** New `agentos-channels` crate with `ChannelAdapter` trait (different from `DeliveryAdapter` — bidirectional by design). 6 adapters. `ChannelManager` in kernel routes inbound messages to agent context windows and outbound messages from agents to channels.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `crates/agentos-channels/Cargo.toml` | Create | New crate manifest |
| `crates/agentos-channels/src/lib.rs` | Create | Trait + types + module index |
| `crates/agentos-channels/src/types.rs` | Create | ChannelMessage, MessageContent, ChannelCapabilities |
| `crates/agentos-channels/src/discord.rs` | Create | Discord adapter (Gateway WS + REST) |
| `crates/agentos-channels/src/slack.rs` | Create | Slack adapter (Socket Mode + Web API) |
| `crates/agentos-channels/src/telegram.rs` | Create | Telegram adapter (long-poll getUpdates) |
| `crates/agentos-channels/src/whatsapp.rs` | Create | WhatsApp Cloud API adapter |
| `crates/agentos-channels/src/email.rs` | Create | IMAP inbound + SMTP outbound |
| `crates/agentos-channels/src/webhook.rs` | Create | Generic webhook (HMAC-signed) |
| `crates/agentos-channels/src/manager.rs` | Create | ChannelManager: adapter registry + message routing |
| `crates/agentos-kernel/src/kernel.rs` | Modify | Add ChannelManager field |
| `crates/agentos-kernel/src/run_loop.rs` | Modify | Spawn channel listener tasks |
| `crates/agentos-types/src/channel.rs` | Modify | Extend ChannelKind with Discord, Slack, WhatsApp |
| `crates/agentos-bus/src/message.rs` | Modify | Add ChannelMessage KernelCommand variant |
| `Cargo.toml` (workspace) | Modify | Add agentos-channels member |

## Dependencies

- **Requires:** Phase 1.1 (REST API — channel webhook endpoints served by agentos-api)
- **Blocks:** Nothing directly (Phase 1.3 marketplace is independent)

---

## Detailed Tasks

### Task 1: Scaffold Crate and Define Trait

**Files:**
- Create: `crates/agentos-channels/Cargo.toml`
- Create: `crates/agentos-channels/src/lib.rs`
- Create: `crates/agentos-channels/src/types.rs`

- [ ] **Step 1: Create crate directory**

```bash
mkdir -p crates/agentos-channels/src
```

- [ ] **Step 2: Write Cargo.toml**

```toml
[package]
name = "agentos-channels"
version.workspace = true
edition.workspace = true

[dependencies]
agentos-types = { path = "../agentos-types" }
async-trait = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
tokio-tungstenite = { version = "0.24", features = ["native-tls"] }
reqwest = { version = "0.12", features = ["json"] }
lettre = { version = "0.11", features = ["tokio1-native-tls", "smtp-transport", "builder"] }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
chrono = { workspace = true }
hmac = "0.12"
sha2 = "0.10"
hex = { workspace = true }
uuid = { version = "1", features = ["v4"] }
```

- [ ] **Step 3: Write types.rs**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique message identifier.
pub type MessageID = String;

/// Unified channel message — all adapters normalize to/from this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: MessageID,
    pub channel_type: String,
    pub sender: ChannelIdentity,
    pub content: MessageContent,
    pub thread_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelIdentity {
    pub platform_id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum MessageContent {
    Text(String),
    Markdown(String),
    Image { url: String, alt: Option<String> },
    File { url: String, filename: String, mime: String },
    Mixed(Vec<MessageContent>),
}

impl MessageContent {
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) | MessageContent::Markdown(s) => s.clone(),
            MessageContent::Image { alt, .. } => alt.clone().unwrap_or_default(),
            MessageContent::File { filename, .. } => format!("[file: {}]", filename),
            MessageContent::Mixed(parts) => parts.iter().map(|p| p.as_text()).collect::<Vec<_>>().join("\n"),
        }
    }
}

/// What a channel adapter supports.
#[derive(Debug, Clone)]
pub struct ChannelCapabilities {
    pub threads: bool,
    pub reactions: bool,
    pub media: bool,
    pub rich_formatting: bool,
    pub max_message_length: usize,
}

/// Outbound message from kernel to external platform.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub channel_instance_id: String,
    pub content: MessageContent,
    pub thread_id: Option<String>,
}

/// Receipt confirming delivery.
#[derive(Debug, Clone)]
pub struct DeliveryReceipt {
    pub message_id: MessageID,
    pub delivered_at: DateTime<Utc>,
}

/// Inbound message from external platform to kernel.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub id: MessageID,
    pub channel_type: String,
    pub channel_instance_id: String,
    pub sender: ChannelIdentity,
    pub content: MessageContent,
    pub thread_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub raw: serde_json::Value,
}
```

- [ ] **Step 4: Write trait in lib.rs**

```rust
pub mod types;
pub mod discord;
pub mod email;
pub mod manager;
pub mod slack;
pub mod telegram;
pub mod webhook;
pub mod whatsapp;

use async_trait::async_trait;
use agentos_types::AgentOSError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use types::*;

/// Health status of a channel connection.
#[derive(Debug, Clone)]
pub enum ChannelHealth {
    Connected,
    Degraded(String),
    Disconnected(String),
}

/// Bidirectional channel adapter trait.
#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ChannelCapabilities;
    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError>;
    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError>;
    async fn health_check(&self) -> ChannelHealth;
}
```

- [ ] **Step 5: Add to workspace, verify compilation**

Run: `cargo build -p agentos-channels`

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-channels/ Cargo.toml
git commit -m "feat(channels): scaffold crate with ChannelAdapter trait and types"
```

### Task 2: Telegram Adapter

**Files:**
- Create: `crates/agentos-channels/src/telegram.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_telegram_update() {
        let json = serde_json::json!({
            "update_id": 123,
            "message": {
                "message_id": 456,
                "chat": {"id": 789, "type": "private"},
                "from": {"id": 111, "first_name": "Test", "is_bot": false},
                "text": "Hello agent",
                "date": 1234567890
            }
        });
        let update: TelegramUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(update.update_id, 123);
        let msg = update.message.unwrap();
        assert_eq!(msg.text.unwrap(), "Hello agent");
        assert_eq!(msg.chat.id, 789);
    }
}
```

- [ ] **Step 2: Implement Telegram adapter**

```rust
use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
    date: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
    first_name: String,
    #[serde(default)]
    username: Option<String>,
}

pub struct TelegramAdapter {
    bot_token: String,
    chat_id: String,
    instance_id: String,
    client: reqwest::Client,
}

impl TelegramAdapter {
    pub fn new(bot_token: String, chat_id: String, instance_id: String) -> Self {
        Self {
            bot_token,
            chat_id,
            instance_id,
            client: reqwest::Client::new(),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.bot_token, method)
    }
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    fn name(&self) -> &str { "telegram" }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: false,
            reactions: true,
            media: true,
            rich_formatting: true,
            max_message_length: 4096,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let text = msg.content.as_text();
        let resp = self.client
            .post(&self.api_url("sendMessage"))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": text,
                "parse_mode": "Markdown"
            }))
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(AgentOSError::ToolExecutionFailed(
                format!("Telegram API error: {}", resp.status()),
            ));
        }

        Ok(DeliveryReceipt {
            message_id: uuid::Uuid::new_v4().to_string(),
            delivered_at: chrono::Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        let mut offset: i64 = 0;
        let client = self.client.clone();
        let url = self.api_url("getUpdates");
        let instance_id = self.instance_id.clone();

        loop {
            if cancel.is_cancelled() { break; }

            let resp = client
                .get(&url)
                .query(&[("offset", offset.to_string()), ("timeout", "30".to_string())])
                .send()
                .await;

            match resp {
                Ok(r) => {
                    if let Ok(body) = r.json::<serde_json::Value>().await {
                        if let Some(updates) = body.get("result").and_then(|r| r.as_array()) {
                            for update in updates {
                                if let Ok(u) = serde_json::from_value::<TelegramUpdate>(update.clone()) {
                                    offset = u.update_id + 1;
                                    if let Some(msg) = u.message {
                                        if let Some(text) = msg.text {
                                            let inbound = InboundMessage {
                                                id: msg.message_id.to_string(),
                                                channel_type: "telegram".to_string(),
                                                channel_instance_id: instance_id.clone(),
                                                sender: ChannelIdentity {
                                                    platform_id: msg.from.as_ref().map(|f| f.id.to_string()).unwrap_or_default(),
                                                    display_name: msg.from.as_ref().map(|f| f.first_name.clone()),
                                                },
                                                content: MessageContent::Text(text),
                                                thread_id: None,
                                                timestamp: chrono::Utc::now(),
                                                raw: update.clone(),
                                            };
                                            let _ = tx.send(inbound).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Telegram poll error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        match self.client.get(&self.api_url("getMe")).send().await {
            Ok(r) if r.status().is_success() => ChannelHealth::Connected,
            Ok(r) => ChannelHealth::Degraded(format!("status {}", r.status())),
            Err(e) => ChannelHealth::Disconnected(e.to_string()),
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p agentos-channels -- test_parse_telegram`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/agentos-channels/src/telegram.rs
git commit -m "feat(channels): add Telegram adapter with long-poll listener"
```

### Task 3: Discord Adapter

**Files:** `crates/agentos-channels/src/discord.rs`

Follow the same pattern as Telegram but using:
- Gateway WebSocket connection (`wss://gateway.discord.gg`) for inbound
- REST API (`https://discord.com/api/v10/channels/{id}/messages`) for outbound
- Bot token auth via `Authorization: Bot <token>` header
- `GUILD_MESSAGES` and `MESSAGE_CONTENT` gateway intents

### Task 4: Slack Adapter

**Files:** `crates/agentos-channels/src/slack.rs`

Follow the same pattern using:
- Socket Mode WebSocket for inbound events
- Web API (`https://slack.com/api/chat.postMessage`) for outbound
- OAuth2 bot token auth
- Block Kit message formatting

### Task 5: WhatsApp, Email, Webhook Adapters

**Files:** `whatsapp.rs`, `email.rs`, `webhook.rs`

- WhatsApp: Meta Cloud API (`graph.facebook.com/v18.0/`) with webhook callbacks for inbound
- Email: `lettre` crate for SMTP outbound, IMAP IDLE or polling for inbound
- Webhook: Generic HTTP POST with HMAC-SHA256 signing; listens on configurable path via agentos-api

### Task 6: ChannelManager and Kernel Integration

**Files:**
- Create: `crates/agentos-channels/src/manager.rs`
- Modify: `crates/agentos-kernel/src/kernel.rs`
- Modify: `crates/agentos-kernel/src/run_loop.rs`
- Modify: `crates/agentos-types/src/channel.rs`
- Modify: `crates/agentos-bus/src/message.rs`

- [ ] **Step 1: Write ChannelManager**

```rust
use crate::{ChannelAdapter, ChannelHealth};
use crate::types::{InboundMessage, OutboundMessage};
use agentos_types::AgentOSError;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::info;

pub struct ChannelManager {
    adapters: RwLock<HashMap<String, Arc<dyn ChannelAdapter>>>,
    inbound_tx: mpsc::Sender<InboundMessage>,
    cancel: CancellationToken,
}

impl ChannelManager {
    pub fn new(inbound_tx: mpsc::Sender<InboundMessage>, cancel: CancellationToken) -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
            inbound_tx,
            cancel,
        }
    }

    pub async fn register(&self, instance_id: &str, adapter: Arc<dyn ChannelAdapter>) -> Result<(), AgentOSError> {
        let tx = self.inbound_tx.clone();
        let cancel = self.cancel.child_token();
        let adapter_clone = adapter.clone();

        // Start inbound listener in background
        tokio::spawn(async move {
            if let Err(e) = adapter_clone.start_listener(tx, cancel).await {
                tracing::error!("Channel listener failed: {}", e);
            }
        });

        self.adapters.write().await.insert(instance_id.to_string(), adapter);
        info!("Registered channel adapter: {}", instance_id);
        Ok(())
    }

    pub async fn send(&self, instance_id: &str, msg: OutboundMessage) -> Result<(), AgentOSError> {
        let adapters = self.adapters.read().await;
        let adapter = adapters.get(instance_id).ok_or_else(|| {
            AgentOSError::ToolExecutionFailed(format!("channel {} not found", instance_id))
        })?;
        adapter.send(msg).await?;
        Ok(())
    }

    pub async fn health(&self) -> HashMap<String, ChannelHealth> {
        let adapters = self.adapters.read().await;
        let mut results = HashMap::new();
        for (id, adapter) in adapters.iter() {
            results.insert(id.clone(), adapter.health_check().await);
        }
        results
    }

    pub async fn deregister(&self, instance_id: &str) {
        self.adapters.write().await.remove(instance_id);
    }
}
```

- [ ] **Step 2: Extend ChannelKind enum**

In `crates/agentos-types/src/channel.rs`, add Discord, Slack, WhatsApp variants to `ChannelKind`.

- [ ] **Step 3: Add ChannelManager to Kernel struct**

In `kernel.rs`, add `channel_manager: Arc<ChannelManager>` field and initialize in `Kernel::new()`.

- [ ] **Step 4: Wire inbound messages to agent routing**

In `run_loop.rs`, spawn a task that reads from the `inbound_rx` channel and dispatches to the appropriate agent's context via `KernelCommand::ChannelMessage`.

- [ ] **Step 5: Run build and tests**

Run: `cargo build --workspace && cargo test --workspace`

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-channels/ crates/agentos-kernel/ crates/agentos-types/ crates/agentos-bus/
git commit -m "feat(channels): add ChannelManager with kernel integration and 6 adapters"
```

---

## Test Plan

| Test | Assertion |
|------|-----------|
| Telegram update parsing | JSON → `TelegramUpdate` with correct fields |
| MessageContent::as_text | Text, Markdown, Mixed all produce plain text |
| ChannelManager register/deregister | Adapter count changes correctly |
| ChannelManager send to unknown | Returns error |
| Health check (mocked) | Returns Connected/Disconnected appropriately |

## Verification

```bash
cargo build --workspace
cargo test -p agentos-channels
cargo clippy -p agentos-channels -- -D warnings
cargo fmt -p agentos-channels -- --check
```
