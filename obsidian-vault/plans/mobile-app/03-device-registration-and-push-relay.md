---
title: Phase 3 — Device Registration & Push Relay
tags:
  - mobile
  - push
  - channels
  - phase-3
date: 2026-04-19
status: planned
effort: 3d
priority: high
---

# Phase 3 — Device Registration & Push Relay

> Add a `/v1/devices/*` API for mobile clients to register push tokens, and implement a `MobilePushAdapter` in `agentos-channels` that turns kernel events into APNs/FCM notifications. Default transport is Expo Push Service (no Apple/Google credentials needed in dev).

---

## Why this phase

The approval workflow in [[09-approval-workflow-ux]] and the glanceable-task-status UX in [[07-task-management-screens]] both depend on being able to push to a phone out-of-band. That requires two things this phase delivers: a server-side record of each mobile device's push token, and a transport that can deliver notifications through APNs (iOS) and FCM (Android).

## Current → Target state

**Current:**
- No device registry.
- No push delivery — all channels are server-initiated outbound (Discord, Slack webhooks).
- Escalations surface only in web UI / CLI.

**Target:**
- SQLite table `devices` in `auth.db` (keeps device rows next to user rows):
  - `id` UUID, `user_id`, `platform` (`ios`|`android`), `push_token`, `name`, `model`, `created_at`, `last_seen_at`, `revoked_at`.
- REST endpoints:
  - `POST /v1/devices` — register (auth required)
  - `PATCH /v1/devices/:id` — update token / name
  - `DELETE /v1/devices/:id` — revoke (logout)
  - `GET /v1/devices` — list user's devices
- New `MobilePushAdapter` in `agentos-channels/src/mobile_push.rs` implementing `ChannelAdapter`.
- `PushTransport` trait with two impls: `ExpoPushTransport` and `ApnsFcmTransport`.
- `MobilePushHook` in `agentos-kernel`: subscribes to `HookEvent::TaskEnd`, `EscalationCreated`, `CheckpointWritten` events and funnels them to the adapter when users opt in.
- User preferences table `notification_preferences`: per-user opt-in per event type (default: escalations on, task-end off).

## Detailed subtasks

### 3.1 Device registry

File: `crates/agentos-api/src/auth/store.rs` — extend with device methods.

```rust
pub struct Device {
    pub id: Uuid,
    pub user_id: Uuid,
    pub platform: Platform,     // Ios | Android
    pub push_token: ZeroizingString,
    pub name: String,
    pub model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}
```

Schema migration:

```sql
CREATE TABLE devices (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    platform TEXT NOT NULL CHECK (platform IN ('ios','android')),
    push_token TEXT NOT NULL,
    name TEXT NOT NULL,
    model TEXT,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    revoked_at INTEGER
);
CREATE INDEX idx_devices_user ON devices(user_id) WHERE revoked_at IS NULL;
CREATE UNIQUE INDEX idx_devices_token ON devices(push_token) WHERE revoked_at IS NULL;
```

`push_token` uses `ZeroizingString` in memory; stored as text in SQLite (unavoidable — it's what APNs/FCM need). Database file is SQLCipher-encrypted via the vault key.

### 3.2 Device handlers

File: `crates/agentos-api/src/handlers/devices.rs` (new).

```rust
pub async fn register(
    Extension(principal): Extension<AuthPrincipal>,
    State(s): State<AppState>,
    Json(req): Json<RegisterDevice>,
) -> Result<Json<DeviceResponse>, ApiError> {
    let user_id = principal.user_id()?;  // fails for ApiKey principals
    // Enforce per-user device cap (default 10).
    let existing = s.auth_store.list_user_devices(user_id).await?;
    if existing.iter().filter(|d| d.revoked_at.is_none()).count() >= 10 {
        return Err(ApiError::DeviceCapReached);
    }
    // Upsert by push_token — re-registering the same token just refreshes name/model.
    let device = s.auth_store.upsert_device(user_id, req).await?;
    audit!(AuditEventType::DeviceRegistered { user_id, device_id: device.id });
    Ok(Json(device.into()))
}
```

**Security:** API key principals are REJECTED for `/v1/devices/*` — only user-auth (JWT) can register devices.

### 3.3 `PushTransport` trait + Expo impl

File: `crates/agentos-channels/src/mobile_push.rs` (new).

```rust
#[async_trait]
pub trait PushTransport: Send + Sync {
    async fn send(&self, dev: &Device, payload: &PushPayload) -> Result<PushReceipt, PushError>;
}

pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub category_id: Option<String>,  // maps to iOS category for actionable notifications
    pub data: serde_json::Value,      // e.g. { "kind": "escalation", "id": "..." }
    pub priority: PushPriority,       // Normal | High
}

pub struct ExpoPushTransport {
    http: reqwest::Client,
    access_token: Option<ZeroizingString>,  // optional, for rate-limit benefits
}

#[async_trait]
impl PushTransport for ExpoPushTransport {
    async fn send(&self, dev: &Device, p: &PushPayload) -> Result<PushReceipt, PushError> {
        let msg = json!({
            "to": &*dev.push_token,
            "title": p.title,
            "body": p.body,
            "data": p.data,
            "priority": match p.priority { PushPriority::High => "high", _ => "default" },
            "channelId": p.category_id,
        });
        let resp = self.http.post("https://exp.host/--/api/v2/push/send")
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .bearer_auth(self.access_token.as_deref().unwrap_or(""))
            .json(&msg).send().await?;
        // Parse ticket, return receipt id for follow-up.
        ...
    }
}
```

Production impl `ApnsFcmTransport` (stretch for this phase; ship behind a config flag):
- APNs: `a2` crate (HTTP/2, p8 key auth).
- FCM: HTTP v1 API via `reqwest` + OAuth2 service-account token (refresh every hour).

### 3.4 `MobilePushAdapter`

```rust
pub struct MobilePushAdapter {
    transport: Arc<dyn PushTransport>,
    store: Arc<AuthStore>,
}

#[async_trait]
impl ChannelAdapter for MobilePushAdapter {
    fn id(&self) -> &str { "mobile_push" }

    async fn send(&self, msg: ChannelMessage) -> Result<(), ChannelError> {
        // msg.recipient.user_id → all active devices for that user
        let user_id = msg.recipient.user_id().ok_or(ChannelError::MissingRecipient)?;
        let devices = self.store.list_user_devices(user_id).await?;
        let payload = PushPayload {
            title: msg.subject.unwrap_or_default(),
            body: msg.body,
            category_id: msg.metadata.get("category_id").and_then(|v| v.as_str()).map(String::from),
            data: msg.metadata,
            priority: if msg.high_priority { PushPriority::High } else { PushPriority::Normal },
        };
        for d in devices.into_iter().filter(|d| d.revoked_at.is_none()) {
            match self.transport.send(&d, &payload).await {
                Ok(_) => {}
                Err(PushError::InvalidToken) => {
                    self.store.revoke_device(d.id, "transport_invalid_token").await.ok();
                }
                Err(e) => tracing::warn!(?e, device_id = %d.id, "push send failed"),
            }
        }
        Ok(())
    }
}
```

Integrate `ApnsFcmTransport` invalid-token signals (APNs `BadDeviceToken`, FCM `UNREGISTERED`) so we auto-revoke stale rows.

### 3.5 Notification preferences

Table:

```sql
CREATE TABLE notification_preferences (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    escalations_enabled INTEGER NOT NULL DEFAULT 1,
    task_completion_enabled INTEGER NOT NULL DEFAULT 0,
    task_failure_enabled INTEGER NOT NULL DEFAULT 1,
    pipeline_run_complete_enabled INTEGER NOT NULL DEFAULT 0,
    quiet_hours_start TEXT,  -- "22:00"
    quiet_hours_end TEXT,    -- "08:00"
    updated_at INTEGER NOT NULL
);
```

Expose via `GET/PATCH /v1/notifications/preferences`.

### 3.6 `MobilePushHook`

File: `crates/agentos-kernel/src/hooks/mobile_push_hook.rs` (new).

```rust
pub struct MobilePushHook {
    adapter: Arc<MobilePushAdapter>,
    store: Arc<AuthStore>,
}

#[async_trait]
impl Hook for MobilePushHook {
    async fn on_event(&self, event: &HookEvent) -> HookResult {
        match event {
            HookEvent::TaskEnd { task, .. } if !task.success => {
                let pref = self.store.get_preferences(task.owner).await.ok()?;
                if !pref.task_failure_enabled || self.in_quiet_hours(&pref) { return HookResult::Continue; }
                self.adapter.send(ChannelMessage { ... /* compose */ }).await.ok();
            }
            HookEvent::EscalationCreated { escalation, user_id } => {
                let pref = self.store.get_preferences(*user_id).await.ok()?;
                if !pref.escalations_enabled { return HookResult::Continue; }  // escalations IGNORE quiet hours
                self.adapter.send(ChannelMessage {
                    subject: Some(format!("Approve: {}", escalation.tool_name)),
                    body: escalation.input_preview.clone(),
                    high_priority: true,
                    metadata: json!({"kind":"escalation","id": escalation.id, "category_id":"ESCALATION"}),
                    ..default()
                }).await.ok();
            }
            _ => {}
        }
        HookResult::Continue
    }
}
```

Register in kernel bootstrap, AFTER `AuditHook` and `ApprovalHook` so audit happens first.

### 3.7 Unregister on logout

`POST /v1/auth/logout` from Phase 2 is extended: if the caller provides `device_id`, we also `DELETE` it. Mobile app calls this on logout and on token-refresh reuse detection.

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-api/src/auth/store.rs` | add device methods + migrations |
| `crates/agentos-api/src/handlers/devices.rs` | new |
| `crates/agentos-api/src/handlers/notifications.rs` | extend with preferences endpoints |
| `crates/agentos-api/src/service.rs` | mount routes |
| `crates/agentos-channels/src/mobile_push.rs` | new |
| `crates/agentos-channels/src/lib.rs` | export `MobilePushAdapter` |
| `crates/agentos-channels/Cargo.toml` | add `reqwest`, optional `a2` behind `apns` feature |
| `crates/agentos-kernel/src/hooks/mobile_push_hook.rs` | new |
| `crates/agentos-kernel/src/kernel.rs` | register `MobilePushHook` on boot |
| `crates/agentos-audit/src/events.rs` | `DeviceRegistered`, `DeviceRevoked`, `PushSent`, `PushFailed` |
| `config/default.toml` | `[push]` section (`transport = "expo" | "apns_fcm"`, `apns.p8_path`, `fcm.service_account_path`) |

## Dependencies

- [[02-mobile-oauth2-auth-layer]] — JWT principal → user_id needed for device ownership.

## Test plan

- Unit: Device registration enforces cap of 10; 11th request returns `DeviceCapReached`.
- Unit: Device upsert by `push_token` — re-registering updates `name`/`model`, does not create duplicate.
- Unit: `MobilePushAdapter.send` fans out to all non-revoked devices for a user.
- Unit: Transport returning `InvalidToken` auto-revokes device row.
- Unit: Quiet-hours suppresses task-completion but never escalations.
- Integration: Mock Expo server (wiremock) — send escalation push, assert POST to `/push/send` with expected body.
- Integration: Revoked device is NOT included in next send.
- Security: API-key principal cannot register a device (`/v1/devices` returns 403).

## Verification

```bash
cargo test -p agentos-channels -p agentos-api
cargo clippy --workspace -- -D warnings
# Smoke with Expo test token:
curl -X POST https://agentos.example.com/v1/devices \
  -H "Authorization: Bearer $JWT" \
  -d '{"platform":"ios","push_token":"ExponentPushToken[test]","name":"Dev iPhone"}'
```

## Related

- [[Mobile App Plan]]
- [[02-mobile-oauth2-auth-layer]]
- [[09-approval-workflow-ux]] — consumes this transport
