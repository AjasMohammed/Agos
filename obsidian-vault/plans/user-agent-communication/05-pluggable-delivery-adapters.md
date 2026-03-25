---
title: "Phase 5: Pluggable External Delivery Adapters"
tags:
  - kernel
  - integrations
  - plan
  - phase-5
date: 2026-03-24
status: complete
effort: 2.5d
priority: medium
---

# Phase 5: Pluggable External Delivery Adapters

> Extend the `DeliveryAdapter` trait defined in Phase 1 with production-ready external adapters: outbound webhook (generic HTTPS POST), desktop notification (notify-rust, Linux), and Slack webhook. All adapters are configured in `config/default.toml` and registered at kernel startup without code changes.

**Depends on**: [[01-user-message-type-and-router]] (Phase 1)
**Blocks**: Nothing (leaf node)

---

## Why This Phase

The CLI and SSE adapters from Phases 1–2 serve users who are actively watching a terminal or browser. External adapters serve users who are away — they may be in a meeting, on mobile, or using a notification manager. Webhook + desktop + Slack covers the full notification surface of a typical developer:
- **Desktop**: instant, no configuration, works even if browser/terminal is closed
- **Webhook**: integrates with any external system (PagerDuty, Discord, ntfy.sh, custom dashboard)
- **Slack**: most engineering teams already use it; webhook URL is the only config needed

The `DeliveryAdapter` trait from Phase 1 means these are pure additions — no kernel code changes beyond registration at startup.

---

## Current State vs. Target

| Item | Current | Target |
|------|---------|--------|
| `DeliveryAdapter` trait | Defined in Phase 1 | Extended with 3 new impls |
| Webhook adapter | Partial (escalation-only) | General-purpose, all `UserMessage` types |
| Desktop adapter | Does not exist | `notify-rust` crate, Linux-only, priority-filtered |
| Slack adapter | Does not exist | Slack webhook URL, formatted message payload |
| Adapter config | Does not exist | `[notifications.adapters]` in `default.toml` |
| SSRF protection | In escalation webhook | Reused for all webhook-based adapters |

---

## Design: Adapter Configuration

```toml
# config/default.toml

[notifications.adapters.webhook]
enabled = false
url = "https://your-endpoint.example.com/notify"
# Secret for HMAC-SHA256 signature in X-AgentOS-Signature header
secret = ""
# Minimum priority to deliver (info/warning/urgent/critical)
min_priority = "warning"
# Retry policy
max_retries = 3
retry_delay_secs = 5
timeout_secs = 10

[notifications.adapters.desktop]
enabled = true
# Minimum priority to show as desktop notification
min_priority = "warning"
# Show notifications even for task completion (info priority)
notify_on_task_complete = true

[notifications.adapters.slack]
enabled = false
# Slack webhook URL (from Slack App configuration)
webhook_url = ""
# Minimum priority to send to Slack
min_priority = "warning"
# Channel override (overrides webhook URL's default channel)
channel = ""
# Include full body or subject only
include_body = true
```

---

## Detailed Subtasks

### 5.1 — Outbound Webhook Adapter

**File**: `crates/agentos-kernel/src/notification_router.rs` (add to existing adapter impls)

```rust
pub struct WebhookDeliveryAdapter {
    url: String,
    secret: Option<String>,       // for HMAC-SHA256 X-AgentOS-Signature header
    min_priority: NotificationPriority,
    max_retries: u32,
    retry_delay: Duration,
    timeout: Duration,
    client: reqwest::Client,
}

impl WebhookDeliveryAdapter {
    pub fn from_config(cfg: &WebhookAdapterConfig) -> Result<Self, AgentOSError> {
        // SSRF protection: validate URL is not loopback/private/metadata
        validate_webhook_url(&cfg.url)?;  // reuse from escalation.rs
        Ok(Self {
            url: cfg.url.clone(),
            secret: cfg.secret.clone().filter(|s| !s.is_empty()),
            min_priority: cfg.min_priority,
            max_retries: cfg.max_retries,
            retry_delay: Duration::from_secs(cfg.retry_delay_secs),
            timeout: Duration::from_secs(cfg.timeout_secs),
            client: reqwest::Client::builder().timeout(self.timeout).build()?,
        })
    }
}

#[derive(Serialize)]
struct WebhookPayload<'a> {
    notification_id: &'a str,
    subject: &'a str,
    body: &'a str,
    priority: &'a str,
    kind: &'a str,
    from: &'a str,
    task_id: Option<&'a str>,
    requires_response: bool,
    created_at: &'a str,
    agentos_version: &'static str,
}

#[async_trait]
impl DeliveryAdapter for WebhookDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel { DeliveryChannel::Webhook }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        if msg.priority < self.min_priority {
            return Ok(()); // below threshold — skip
        }

        let payload = WebhookPayload {
            notification_id: &msg.id.to_string(),
            subject: &msg.subject,
            body: &msg.body,
            priority: &format!("{:?}", msg.priority).to_lowercase(),
            kind: kind_to_tag(&msg.kind),
            from: &format!("{:?}", msg.from),
            task_id: msg.task_id.as_ref().map(|id| id.as_str()),
            requires_response: msg.interaction.is_some(),
            created_at: &msg.created_at.to_rfc3339(),
            agentos_version: env!("CARGO_PKG_VERSION"),
        };

        let body = serde_json::to_string(&payload)?;
        let mut req = self.client.post(&self.url)
            .header("Content-Type", "application/json")
            .header("X-AgentOS-Version", env!("CARGO_PKG_VERSION"));

        // HMAC-SHA256 signature if secret is configured
        if let Some(secret) = &self.secret {
            let sig = hmac_sha256(secret.as_bytes(), body.as_bytes());
            req = req.header("X-AgentOS-Signature", format!("sha256={}", hex::encode(sig)));
        }

        let mut last_err = None;
        for attempt in 0..=self.max_retries {
            match req.try_clone().unwrap().body(body.clone()).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    last_err = Some(format!("HTTP {}", resp.status()));
                }
                Err(e) => {
                    last_err = Some(e.to_string());
                }
            }
            if attempt < self.max_retries {
                tokio::time::sleep(self.retry_delay).await;
            }
        }
        Err(DeliveryError::Transient(last_err.unwrap_or_default()))
    }

    async fn is_available(&self) -> bool { true }
}
```

---

### 5.2 — Desktop Notification Adapter (Linux)

**File**: `crates/agentos-kernel/src/notification_router.rs`

Add `notify-rust` to `Cargo.toml` (Linux-only, optional feature):
```toml
[dependencies]
notify-rust = { version = "4", optional = true }

[features]
desktop-notifications = ["notify-rust"]
```

```rust
#[cfg(feature = "desktop-notifications")]
pub struct DesktopDeliveryAdapter {
    min_priority: NotificationPriority,
    notify_on_task_complete: bool,
}

#[cfg(feature = "desktop-notifications")]
#[async_trait]
impl DeliveryAdapter for DesktopDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel { DeliveryChannel::Desktop }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        use notify_rust::{Notification, Urgency};

        if msg.priority < self.min_priority {
            if let UserMessageKind::TaskComplete { .. } = &msg.kind {
                if !self.notify_on_task_complete { return Ok(()); }
            } else {
                return Ok(());
            }
        }

        let urgency = match msg.priority {
            NotificationPriority::Critical => Urgency::Critical,
            NotificationPriority::Urgent   => Urgency::Normal,
            _                              => Urgency::Low,
        };

        let icon = match &msg.kind {
            UserMessageKind::TaskComplete { outcome: TaskOutcome::Success, .. } => "dialog-information",
            UserMessageKind::TaskComplete { .. }                                 => "dialog-warning",
            UserMessageKind::Question { .. }                                     => "dialog-question",
            _                                                                    => "agentos",
        };

        // Desktop notifications cannot block — spawn as best-effort
        let subject = msg.subject.clone();
        let body_preview = msg.body.chars().take(150).collect::<String>();
        tokio::task::spawn_blocking(move || {
            let _ = Notification::new()
                .summary(&subject)
                .body(&body_preview)
                .icon(icon)
                .urgency(urgency)
                .timeout(notify_rust::Timeout::Milliseconds(8000))
                .show();
        });

        Ok(())
    }

    async fn is_available(&self) -> bool {
        // Check if a notification daemon is running (D-Bus available)
        // Simplified: try sending a test notification and see if it errors
        true
    }
}

// Stub for non-Linux or when feature is disabled
#[cfg(not(feature = "desktop-notifications"))]
pub struct DesktopDeliveryAdapter;

#[cfg(not(feature = "desktop-notifications"))]
#[async_trait]
impl DeliveryAdapter for DesktopDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel { DeliveryChannel::Desktop }
    async fn deliver(&self, _msg: &UserMessage) -> Result<(), DeliveryError> { Ok(()) }
    async fn is_available(&self) -> bool { false }
}
```

---

### 5.3 — Slack Webhook Adapter

**File**: `crates/agentos-kernel/src/notification_router.rs`

```rust
pub struct SlackDeliveryAdapter {
    webhook_url: String,
    min_priority: NotificationPriority,
    include_body: bool,
    client: reqwest::Client,
}

impl SlackDeliveryAdapter {
    pub fn from_config(cfg: &SlackAdapterConfig) -> Result<Self, AgentOSError> {
        validate_webhook_url(&cfg.webhook_url)?;  // SSRF protection
        Ok(Self {
            webhook_url: cfg.webhook_url.clone(),
            min_priority: cfg.min_priority,
            include_body: cfg.include_body,
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Serialize)]
struct SlackBlock<'a> {
    r#type: &'static str,
    text: SlackText<'a>,
}

#[derive(Serialize)]
struct SlackText<'a> {
    r#type: &'static str,
    text: &'a str,
}

#[async_trait]
impl DeliveryAdapter for SlackDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel { DeliveryChannel::Slack }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        if msg.priority < self.min_priority {
            return Ok(());
        }

        // Slack Block Kit message
        let emoji = match msg.priority {
            NotificationPriority::Critical => ":rotating_light:",
            NotificationPriority::Urgent   => ":warning:",
            NotificationPriority::Warning  => ":large_yellow_circle:",
            NotificationPriority::Info     => ":information_source:",
        };

        let header_text = format!("{emoji} *AgentOS* — {}", msg.subject);
        let body_text = if self.include_body {
            msg.body.chars().take(500).collect::<String>()
        } else {
            String::new()
        };

        let mut blocks = vec![
            serde_json::json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": header_text }
            }),
        ];

        if !body_text.is_empty() {
            blocks.push(serde_json::json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": body_text }
            }));
        }

        if msg.interaction.is_some() {
            blocks.push(serde_json::json!({
                "type": "context",
                "elements": [{ "type": "mrkdwn", "text": "_Reply via `agentctl notifications respond` or the web UI._" }]
            }));
        }

        let payload = serde_json::json!({ "blocks": blocks });

        self.client.post(&self.webhook_url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| DeliveryError::Transient(e.to_string()))?
            .error_for_status()
            .map_err(|e| DeliveryError::Transient(e.to_string()))?;

        Ok(())
    }

    async fn is_available(&self) -> bool { true }
}
```

---

### 5.4 — Register adapters at kernel startup

**File**: `crates/agentos-kernel/src/kernel.rs` (startup/initialization code)

```rust
pub async fn build_notification_router(
    config: &KernelConfig,
    inbox: Arc<UserInbox>,
) -> Result<NotificationRouter, AgentOSError> {
    let mut adapters: Vec<Box<dyn DeliveryAdapter>> = vec![
        Box::new(CliDeliveryAdapter::new()),
    ];

    if config.notifications.adapters.desktop.enabled {
        adapters.push(Box::new(DesktopDeliveryAdapter::from_config(
            &config.notifications.adapters.desktop
        )));
    }

    if config.notifications.adapters.webhook.enabled {
        adapters.push(Box::new(WebhookDeliveryAdapter::from_config(
            &config.notifications.adapters.webhook
        )?));
    }

    if config.notifications.adapters.slack.enabled {
        adapters.push(Box::new(SlackDeliveryAdapter::from_config(
            &config.notifications.adapters.slack
        )?));
    }

    // SseDeliveryAdapter is added by the web server after it creates AppState
    // (see Phase 2 design)

    Ok(NotificationRouter::new(inbox, adapters))
}
```

---

### 5.5 — SSRF protection reuse

**File**: `crates/agentos-kernel/src/escalation.rs` has `validate_webhook_url()`.

Extract to a shared location:

**File**: `crates/agentos-kernel/src/network_safety.rs` (new file, or add to a shared utils module)

```rust
/// Reject webhook URLs that target loopback, private networks,
/// or cloud metadata services (SSRF protection).
pub fn validate_webhook_url(url: &str) -> Result<(), AgentOSError> {
    // This logic already exists in escalation.rs — move it here
    // and update escalation.rs to call this function
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/notification_router.rs` | Add WebhookDeliveryAdapter, DesktopDeliveryAdapter, SlackDeliveryAdapter |
| `crates/agentos-kernel/src/kernel.rs` | `build_notification_router()` wires adapters from config |
| `crates/agentos-kernel/src/network_safety.rs` | NEW — shared `validate_webhook_url()` |
| `crates/agentos-kernel/src/escalation.rs` | Use `network_safety::validate_webhook_url()` |
| `crates/agentos-kernel/Cargo.toml` | Add `notify-rust` optional dependency |
| `config/default.toml` | Add `[notifications.adapters.*]` sections |
| `crates/agentos-kernel/src/config.rs` | Add `WebhookAdapterConfig`, `DesktopAdapterConfig`, `SlackAdapterConfig` |

---

## Test Plan

```rust
#[tokio::test]
async fn test_webhook_adapter_posts_on_deliver() {
    // spin up a local HTTP server (wiremock or axum test server)
    let server = MockServer::start().await;
    server.mock(|when, then| {
        when.method(POST).path("/notify");
        then.status(200);
    });
    let adapter = WebhookDeliveryAdapter::from_config(&WebhookAdapterConfig {
        url: server.uri() + "/notify",
        enabled: true, ..Default::default()
    }).unwrap();
    let msg = make_test_notification_with_priority(NotificationPriority::Urgent);
    adapter.deliver(&msg).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn test_webhook_adapter_skips_below_min_priority() {
    let adapter = /* min_priority: Urgent */;
    let msg = make_test_notification_with_priority(NotificationPriority::Info);
    adapter.deliver(&msg).await.unwrap();
    // no HTTP request should have been made
}

#[tokio::test]
async fn test_webhook_adapter_retries_on_server_error() {
    // server returns 500 twice, then 200
    // adapter retries up to max_retries
    // expect success on 3rd attempt
}

#[tokio::test]
async fn test_ssrf_protection_blocks_loopback_webhook() {
    let result = WebhookDeliveryAdapter::from_config(&WebhookAdapterConfig {
        url: "http://127.0.0.1:8080/evil".to_string(),
        enabled: true, ..Default::default()
    });
    assert!(result.is_err());
}

#[tokio::test]
async fn test_slack_adapter_sends_block_kit_message() {
    let server = MockServer::start().await;
    server.mock(|when, then| {
        when.method(POST).path("/slack-webhook")
             .body_contains("blocks");
        then.status(200).body("ok");
    });
    let adapter = SlackDeliveryAdapter::from_config(&SlackAdapterConfig {
        webhook_url: server.uri() + "/slack-webhook",
        enabled: true, ..Default::default()
    }).unwrap();
    adapter.deliver(&make_test_notification_with_priority(NotificationPriority::Urgent)).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn test_webhook_adapter_includes_hmac_signature() {
    let secret = "mysecret";
    let adapter = WebhookDeliveryAdapter::from_config(&WebhookAdapterConfig {
        url: "https://example.com/notify".to_string(),
        secret: secret.to_string(),
        enabled: true, ..Default::default()
    }).unwrap();
    // inspect the X-AgentOS-Signature header in the request
    // verify it is HMAC-SHA256 of the payload body with the secret
}
```

---

## Verification

```bash
# Build with desktop notifications feature
cargo build -p agentos-kernel --features desktop-notifications

# Standard build (no desktop)
cargo build -p agentos-kernel

# Tests
cargo test -p agentos-kernel -- webhook_adapter
cargo test -p agentos-kernel -- slack_adapter
cargo test -p agentos-kernel -- ssrf

# Manual test — webhook
# Configure config/default.toml:
# [notifications.adapters.webhook]
# enabled = true
# url = "https://webhook.site/your-unique-id"
# min_priority = "info"

agentctl kernel start &
agentctl task run --agent my-agent "say hello"
# → check webhook.site for the completion notification payload

# Manual test — Slack
# [notifications.adapters.slack]
# enabled = true
# webhook_url = "https://hooks.slack.com/services/..."
# → run a task → check Slack channel for Block Kit message
```
