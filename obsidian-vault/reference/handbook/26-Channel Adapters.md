---
title: Channel Adapters
tags:
  - channels
  - integrations
  - reference
  - handbook
  - v3
date: 2026-04-02
status: complete
effort: 2h
priority: high
---

# Channel Adapters

> The `agentos-channels` crate provides bidirectional messaging adapters for external platforms. Agents can receive inbound messages from users (via Discord, Telegram, Slack, or Webhooks) and send outbound responses back through the same platform. This is distinct from the notification system in [[21-User Notifications and Channels]], which is for kernel-to-operator alerting.

---

## Overview

Channel adapters connect AgentOS to external messaging platforms. Each adapter handles:

- **Inbound** — receiving messages from users on the platform and routing them to agents
- **Outbound** — sending agent replies back to the platform
- **Health** — reporting connection status

All adapters implement the `ChannelAdapter` trait and are managed by the `ChannelManager` in the kernel.

### How inbound routing works

```
Discord/Telegram/Slack/WhatsApp
        ↓ (inbound message)
   ChannelManager.inbound_tx  (mpsc channel)
        ↓
   Kernel router
        ↓
   Target agent's context window
        ↓
   Agent reply  →  ChannelManager.send()  →  Platform
```

---

## Adapter Comparison

| Adapter | Inbound method | Outbound API | Max message | Threads | Status |
|---------|---------------|-------------|-------------|---------|--------|
| **Discord** | WebSocket Gateway | REST v10 | 2,000 chars | ✗ | Stable |
| **Telegram** | Long-polling | Bot API | 4,096 chars | ✗ | Stable |
| **Slack** | REST polling (5s) | `chat.postMessage` | 40,000 chars | ✓ | Stable |
| **WhatsApp** | Webhook (inbound via REST API) | Cloud API v18 | 4,096 chars | ✗ | Stable |
| **Webhook** | Webhook (inbound via REST API) | HTTP POST | 100,000 chars | ✗ | Stable |
| **Email** | Stub (IMAP planned) | SMTP via `lettre` | Unlimited | ✓ | Partial |

---

## Discord

Discord uses the **Gateway WebSocket API** for inbound messages and the **REST API v10** for outbound.

### What you need

- A Discord application and bot created in the [Discord Developer Portal](https://discord.com/developers/applications)
- A **bot token** (from the Bot tab of your application)
- The **channel ID** of the Discord channel you want to bridge (right-click channel → Copy Channel ID — requires Developer Mode enabled in Discord settings)
- Bot must be added to your server with `MESSAGE_CONTENT` intent enabled

### Required bot intents

In the Discord Developer Portal, under your bot's settings:

- Enable **Message Content Intent** (Privileged Gateway Intents)
- The adapter requests intents `33280` — `GUILD_MESSAGES` (1 << 9 = 512) and `MESSAGE_CONTENT` (1 << 15 = 32768)

### Bot permissions

The bot needs at minimum: `Read Messages/View Channels`, `Send Messages`.

### Store the bot token

```bash
agentos secret set DISCORD_BOT_TOKEN
# Paste your bot token when prompted
```

### Register the adapter (programmatic)

Channel adapters are registered in code via `ChannelManager::register()`. The kernel configuration API for channels is in active development — the current integration point is the kernel boot sequence or a custom startup hook:

```rust
use agentos_channels::discord::DiscordAdapter;
use std::sync::Arc;

let adapter = DiscordAdapter::new(
    std::env::var("DISCORD_BOT_TOKEN").unwrap(),
    "YOUR_CHANNEL_ID".to_string(),
    "discord-main".to_string(),   // unique instance ID
);
channel_manager.register("discord-main", Arc::new(adapter)).await?;
```

### Health states

| State | Meaning |
|-------|---------|
| `Connected` | REST API reachable **and** Gateway WebSocket listener running |
| `Degraded("REST reachable but Gateway listener is not running")` | Bot token is valid but the WS listener has exited (restart needed) |
| `Disconnected(reason)` | REST API unreachable — invalid token, network error |

### Notes and limitations

- The Gateway listener does **not** automatically reconnect after a WebSocket drop or Discord-initiated reconnect. If the listener exits (visible via `Degraded` health), the kernel must be restarted or the adapter re-registered.
- Discord sends a HELLO opcode on first connect; the adapter responds with IDENTIFY and then maintains a heartbeat loop. The bot token is held in memory only until IDENTIFY is sent, then zeroed.
- Only `MESSAGE_CREATE` dispatch events in the configured channel are forwarded as inbound messages. Bot messages are not filtered out by the adapter itself — apply filtering at the agent routing layer if needed.

---

## Telegram

Telegram uses **long-polling** (`getUpdates` with `timeout=30`) for inbound messages.

### What you need

- A Telegram bot created via [@BotFather](https://t.me/BotFather) — `/newbot` gives you a **bot token**
- The **chat ID** of the chat or group you want to bridge
  - For a private chat: message your bot, then call `https://api.telegram.org/bot<TOKEN>/getUpdates` and note the `message.chat.id` value
  - For a group: add the bot to the group, send a message, then use `getUpdates`

### Store the bot token

```bash
agentos secret set TELEGRAM_BOT_TOKEN
# Paste your bot token (format: 123456789:ABCdef...)
```

### Register the adapter (programmatic)

```rust
use agentos_channels::telegram::TelegramAdapter;
use std::sync::Arc;

let adapter = TelegramAdapter::new(
    std::env::var("TELEGRAM_BOT_TOKEN").unwrap(),
    "123456789".to_string(),   // chat_id (numeric)
    "telegram-ops".to_string(), // unique instance ID
);
channel_manager.register("telegram-ops", Arc::new(adapter)).await?;
```

### Polling behaviour

- The adapter polls `/getUpdates?offset=<last+1>&timeout=30` in a loop
- The `offset` advances after each batch so messages are not re-delivered
- On network error, the adapter waits 5 seconds and retries (no exponential backoff)
- Outbound messages are sent via `/sendMessage` with `parse_mode: Markdown`

### Health states

| State | Meaning |
|-------|---------|
| `Connected` | `/getMe` returned 200 OK |
| `Degraded("status <N>")` | Bot API returned a non-success HTTP status |
| `Disconnected("timeout" / "connection refused" / "network error")` | Network-level failure (error details intentionally sanitised — the bot token appears in the API URL) |

### Notes and limitations

- Telegram does not deliver messages sent before the bot was started (offset is initialised to current time)
- There is no webhook mode in the current implementation — long-polling means one outbound HTTP request per 30-second poll window
- Outbound messages use `parse_mode: Markdown`. Telegram's legacy Markdown parser requires careful escaping for `*`, `_`, `` ` ``, and `[` characters

---

## Slack

Slack uses **REST polling** (`conversations.history` every 5 seconds) for inbound messages.

### What you need

- A Slack app at [api.slack.com/apps](https://api.slack.com/apps)
- A **Bot Token** (`xoxb-...`) from the **OAuth & Permissions** tab
- Required OAuth scopes: `channels:history`, `chat:write`
- The **channel ID** (not the display name — found via the Slack API or by right-clicking a channel)

> [!note] Socket Mode
> The polling approach is suitable for low-volume deployments. For production use, Slack recommends **Socket Mode** with an app-level token. Socket Mode is not yet implemented in this adapter; the comment in the source code notes it as a future improvement.

### Store the bot token

```bash
agentos secret set SLACK_BOT_TOKEN
# Paste your bot token (format: xoxb-...)
```

### Register the adapter (programmatic)

```rust
use agentos_channels::slack::SlackAdapter;
use std::sync::Arc;

let adapter = SlackAdapter::new(
    std::env::var("SLACK_BOT_TOKEN").unwrap(),
    "C0123456789".to_string(),   // channel_id
    "slack-ops".to_string(),      // unique instance ID
);
channel_manager.register("slack-ops", Arc::new(adapter)).await?;
```

### Polling behaviour

- Calls `conversations.history?channel=<id>&oldest=<last_ts>&limit=10` every 5 seconds
- Messages are processed in chronological order (reversed from the API response)
- `last_ts` is initialised to the current Unix timestamp so historical messages are not replayed
- Outbound messages use `chat.postMessage` — the `ok` field in the response is checked; `ok: false` returns an error with the Slack error string

### Health states

| State | Meaning |
|-------|---------|
| `Connected` | `auth.test` returned `ok: true` |
| `Degraded("<error>")` | Slack returned `ok: false` — invalid token, missing scope, etc. |
| `Disconnected(reason)` | Network-level failure |

---

## WhatsApp

WhatsApp uses the **Meta Cloud API** (Graph API v18.0). Inbound messages are **webhook-driven** — they arrive via a webhook posted to the REST API layer (`agentos-api`), which forwards them to the `ChannelManager` directly.

### What you need

- A Meta Business account and a verified **WhatsApp Business App** in the [Meta Developer Console](https://developers.facebook.com)
- A **phone number ID** (from the WhatsApp product in your app)
- A **recipient phone number** in E.164 format (e.g. `+15551234567`)
- A **Cloud API access token** (System User token with `whatsapp_business_messaging` permission)
- A configured webhook in the Meta Developer Console pointing at `https://<your-host>/api/v1/channels/whatsapp/inbound`

### Store the access token

```bash
agentos secret set WHATSAPP_ACCESS_TOKEN
```

### Register the adapter (programmatic)

```rust
use agentos_channels::whatsapp::WhatsAppAdapter;
use std::sync::Arc;

let adapter = WhatsAppAdapter::new(
    std::env::var("WHATSAPP_ACCESS_TOKEN").unwrap(),
    "123456789012345".to_string(),  // phone_number_id
    "+15551234567".to_string(),     // recipient_phone (E.164)
    "whatsapp-support".to_string(), // unique instance ID
);
channel_manager.register("whatsapp-support", Arc::new(adapter)).await?;
```

### Inbound message flow

WhatsApp inbound messages do not use a polling loop. The `start_listener` implementation parks until cancelled — inbound messages are received via the REST API webhook endpoint and injected into the `ChannelManager.inbound_tx` channel by the API layer.

### Message constraints

- Plain text only via `type: "text"` — no Markdown formatting
- Maximum 4,096 characters per message
- Delivery receipts are available via the Graph API (not yet implemented in the current adapter)

### Health states

| State | Meaning |
|-------|---------|
| `Connected` | `GET /{phone_number_id}` returned 200 OK |
| `Degraded("status <N>")` | Graph API returned a non-success status |
| `Disconnected(reason)` | Network-level failure |

---

## Webhook

The Webhook adapter is for **custom integrations**. Outbound messages are HTTP POSTed to a URL you control; inbound messages are received via the REST API webhook endpoint.

### What you need

- A **target URL** that accepts `POST` requests with JSON body
- A **shared secret** for HMAC-SHA256 request signing

### Request signing

Every outbound POST includes an `X-AgentOS-Signature` header — the hex-encoded HMAC-SHA256 of the request body using the shared secret. Verify it on your receiving server:

```python
import hmac, hashlib

def verify(body: bytes, signature: str, secret: str) -> bool:
    expected = hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, signature)
```

### Inbound signatures

The same signing mechanism can be applied to inbound webhooks. When the REST API layer receives a `POST /api/v1/channels/webhook/inbound/<instance_id>`, it can verify the `X-AgentOS-Signature` before forwarding to the channel manager.

### Register the adapter (programmatic)

```rust
use agentos_channels::webhook::WebhookAdapter;
use std::sync::Arc;

let adapter = WebhookAdapter::new(
    "https://your-server.example.com/agentos-hook".to_string(),
    "your-shared-secret".to_string(),
    "webhook-integration".to_string(),
);
channel_manager.register("webhook-integration", Arc::new(adapter)).await?;
```

### Outbound payload

```json
{
  "channel_instance_id": "webhook-integration",
  "content": { "type": "Text", "data": "Agent reply text" },
  "thread_id": null
}
```

### Health states

Health is checked via `HEAD <target_url>`. Both `2xx` and `405 Method Not Allowed` are treated as `Connected` — a `405` means the server is reachable but does not support HEAD, which is normal for POST-only endpoints.

---

## Email

The email adapter sends outbound messages via SMTP using the `lettre` crate (`AsyncSmtpTransport<Tokio1Executor>`). Inbound message reception via IMAP IDLE is planned but not yet implemented.

### What you need

- An SMTP server with credentials (host, port, username, password)
- A sender email address (the `From:` header)
- A recipient email address

### Store SMTP credentials

```bash
agentos secret set SMTP_PASSWORD
# Paste your SMTP password when prompted
```

### Register the adapter (programmatic)

```rust
use agentos_channels::email::EmailAdapter;
use std::sync::Arc;

let adapter = EmailAdapter::new(
    "smtp.example.com".to_string(),     // SMTP host
    587,                                  // SMTP port (STARTTLS)
    "bot@example.com".to_string(),       // sender address
    "operator@example.com".to_string(),  // recipient address
    "bot@example.com".to_string(),       // username
    std::env::var("SMTP_PASSWORD").unwrap(), // password
    "email-ops".to_string(),             // instance ID
);
channel_manager.register("email-ops", Arc::new(adapter)).await?;
```

### Health states

| State | Meaning |
|-------|---------|
| `Connected` | TCP connect to SMTP host:port succeeded |
| `Disconnected(reason)` | TCP connect failed — host unreachable, DNS failure |

### Notes and limitations

- Outbound only — IMAP IDLE inbound listening is not yet implemented
- Messages are sent as plain text (`text/plain`)
- The SMTP connection is established per-send (no persistent connection pool)
- STARTTLS is used when the port supports it (typically port 587)

---

## Retry Mechanism

All channel adapters benefit from a shared retry mechanism (`crates/agentos-channels/src/retry.rs`) that provides exponential backoff for transient failures.

### RetryPolicy

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_attempts` | u32 | 3 | Maximum attempts including the initial attempt |
| `base_delay` | Duration | 500ms | Base delay between retries (doubled each attempt) |
| `max_delay` | Duration | 30s | Maximum delay cap |

### Retryable Errors

Only transient errors trigger retries. The retry logic inspects error messages for:

- Network errors: `timeout`, `connection`, `reset by peer`, `broken pipe`, `timed out`
- Rate limits: `429`
- Server errors: `500`, `502`, `503`, `504`

Non-transient errors (authentication failures, bad requests) are returned immediately without retry.

### Usage

The `with_retry()` function wraps any async operation:

```rust
use agentos_channels::retry::{with_retry, RetryPolicy};

let result = with_retry(&RetryPolicy::default(), "discord", || async {
    // ... send message ...
}).await?;
```

Each retry logs a warning with the channel name, attempt number, delay, and error message.

---

## ChannelManager

All adapters are managed by `ChannelManager` in the kernel:

```
ChannelManager
  ├── register(instance_id, adapter)  — starts listener task, adds to map
  ├── send(instance_id, msg)          — routes outbound to the right adapter
  ├── health()                        — health check all registered adapters
  ├── deregister(instance_id)         — removes adapter (listener token cancelled)
  └── adapter_count()                 — number of currently registered adapters
```

Listeners run as independent Tokio tasks. If a listener exits (e.g., Discord WebSocket drop), the task terminates silently and the adapter's health degrades to `Degraded`. The `ChannelManager` does not restart listeners automatically.

The `inbound_tx` sender routes all inbound messages from all channels into a single `mpsc` channel consumed by the kernel router.

---

## Security Notes

| Property | Implementation |
|----------|---------------|
| Token storage | All credentials are `Zeroizing<String>` — zeroed from heap on drop |
| Discord IDENTIFY | Bot token is taken from `Option` and dropped immediately after the IDENTIFY payload is sent; it is not held for the duration of the Gateway listener loop |
| Telegram error logging | Error messages from `reqwest` are sanitised — the bot token (which appears in the URL) is never included in log output |
| Webhook signatures | HMAC-SHA256 with `subtle::ConstantTimeEq` for constant-time comparison — prevents timing attacks |

---

## Related

- [[21-User Notifications and Channels]] — Operator notification inbox and delivery channels (distinct from bidirectional agent channels)
- [[23-REST API Reference]] — REST API layer that receives inbound webhooks for WhatsApp and Webhook adapters
- [[08-Security Model]] — Credential handling and zeroization
- [[09-Secrets and Vault]] — Storing bot tokens and access tokens securely
