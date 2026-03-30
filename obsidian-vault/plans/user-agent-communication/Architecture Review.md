---
title: User-Agent Communication — Architecture Review
tags:
  - kernel
  - review
  - plan
  - architecture
date: 2026-03-24
status: complete
effort: 1d
priority: critical
---

# User-Agent Communication — Architecture Review

> Critical review of the UNIS plan (Phases 1–5) for production-grade quality and long-term viability. Identifies structural gaps, proposes fixes, and scopes Phase 6.

---

## Verdict Summary

| Layer | Rating | Notes |
|-------|--------|-------|
| **Data model** (`UserMessage`) | ✅ Strong | Well-typed, expressive, covers all cases |
| **Outbound dispatch** (`NotificationRouter`) | ✅ Strong | Inbox-first is correct; adapter pattern is clean |
| **Blocking ask_user** (`oneshot`) | ⚠️ Fragile | Non-durable: restart loses the wait |
| **Inbound routing** | ❌ Missing | No `ChannelListener`, no `InboundRouter` |
| **Channel registration** | ❌ Missing | No `UserChannelRegistry` — channels are static config, not dynamic |
| **DeliveryAdapter extensibility** | ⚠️ Closed | Enum-based `DeliveryChannel` breaks on new channels |
| **Async correctness** | ⚠️ Risk | `Mutex<rusqlite::Connection>` is blocking in async context |
| **WaitingTaskMap lifecycle** | ⚠️ Risk | Entries can leak if task crashes mid-wait |
| **Message continuity** | ❌ Missing | No threading across messages in same conversation |
| **User-initiated communication** | ❌ Missing | User cannot message agents; entire inbound half absent |

---

## Critical Gaps (must fix before production)

### GAP-1: The plan is outbound-only — no inbound channel

**Impact**: Critical. Without this, users cannot initiate communication with the ecosystem. The plan covers agents → user but not user → agents.

The `DeliveryAdapter` trait has only:
```rust
async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError>;
async fn is_available(&self) -> bool;
```

There is no `receive()` method, no `ChannelListener`, no background task that polls Telegram for new messages, no `InboundRouter` that maps "user sent /tasks" to a `KernelCommand`.

**Fix**: Phase 6 — Bidirectional Channel Protocol. See [[06-bidirectional-channel-protocol]].

---

### GAP-2: No `UserChannelRegistry`

**Impact**: Critical. The ecosystem has no awareness of which channels are connected. Agents cannot query "can I reach this user?". Connection is static `config/default.toml` keys, not a dynamic "user connected Telegram" registration.

Currently: Telegram `chat_id` does not exist as a concept in the plan at all. Phase 5 adds webhook/Slack adapters as static config. There is no:
- `agentctl channel connect telegram` command
- First-use `/start` handshake with chat_id capture
- Registry queried by agents before deciding to notify

**Fix**: `UserChannelRegistry` in kernel, `agentctl channel connect/list/disconnect` CLI, channel state persisted in vault. See Phase 6.

---

### GAP-3: `oneshot::channel` for ask_user is non-durable

**Impact**: High. If the kernel restarts, crashes, or is OOM-killed while a task is parked in `TaskState::Waiting`, the `oneshot::Sender` is dropped. The user may have already been notified and is waiting to respond, but the task is gone with no indication.

The plan acknowledges this as a known limitation ("task fails with KernelRestarted — agent can retry"), but the mitigation is insufficient for production. A task that took 20 minutes before asking the user is not something users want silently reset.

**Fix**: When a task enters `TaskState::Waiting`, serialize the `(TaskID, NotificationID)` pair to `KernelStateStore`. On restart, the kernel sweeps `user_messages` for `status=Pending` Questions with matching task IDs and reconstructs the wait map — or fails the tasks explicitly with a clear error.

```rust
// On entering TaskState::Waiting:
kernel.state_store.record_waiting_task(task_id, notification_id).await?;

// On kernel restart:
let pending = inbox.list_pending_questions().await?;
for q in pending {
    if let Some(task_id) = q.task_id {
        // Fail the task with a clear message
        kernel.fail_task(task_id, "Kernel restarted while waiting for user response. Please retry.").await;
    }
}
```

---

### GAP-4: `DeliveryChannel` enum is closed

**Impact**: High. Adding a new channel (Telegram, ntfy, Matrix) requires:
1. Adding a variant to `pub enum DeliveryChannel`
2. Updating all `match` arms across the codebase
3. Recompiling

For a long-term extensible system, channels should be string-identified.

**Fix**: Change to:
```rust
pub enum DeliveryChannel {
    Cli,
    Web,
    Webhook,
    Desktop,
    Slack,
    Telegram,
    Ntfy,
    Email,
    Custom(String),  // for future / user-defined adapters
}
```

Or go fully string-based with constants:
```rust
pub type ChannelId = String;
pub mod channels {
    pub const CLI: &str = "cli";
    pub const WEB: &str = "web";
    pub const TELEGRAM: &str = "telegram";
    pub const NTFY: &str = "ntfy";
}
```

---

### GAP-5: `UserInbox` uses blocking `Mutex` in async context

**Impact**: High. `Arc<Mutex<rusqlite::Connection>>` with `std::sync::Mutex` will block the Tokio thread pool when SQLite operations take time. Under load (many concurrent agents), this causes thread starvation.

Current plan:
```rust
pub struct UserInbox {
    db: Arc<Mutex<Connection>>,  // ← std::sync::Mutex — WRONG in async
}
```

**Fix**: Either:
- Use `tokio::sync::Mutex` (async mutex — does not block the thread)
- Use `tokio::task::spawn_blocking` around every DB call
- Switch to `sqlx` with its built-in async SQLite connection pool (preferred long-term)

Recommendation: Use `sqlx` with `SqlitePool`. It's already used in other AgentOS crates (audit log).

---

## High-Severity Issues

### ISSUE-6: `WaitingTaskMap` can leak entries

If an agent creates a blocking `ask_user`, registers the `oneshot::Sender` in `WaitingTaskMap`, but then the task panics or is cancelled before `tokio::time::timeout` fires, the `Sender` stays in the map until kernel restart.

**Fix**: The existing `TimeoutChecker` (sweeps resource locks and escalations every 10min) should also sweep `WaitingTaskMap`:

```rust
// In TimeoutChecker loop:
router.sweep_expired_waiters().await;

// In NotificationRouter:
pub async fn sweep_expired_waiters(&self) {
    let now = Utc::now();
    let inbox = self.inbox.list_expired_questions(now).await;
    let mut map = self.waiting_tasks.write().await;
    for q in inbox {
        if let Some(tx) = map.remove(&q.id) {
            // auto-action the question
            let _ = tx.send(UserResponse {
                text: q.interaction.as_ref()
                    .map(|i| i.auto_action.clone())
                    .unwrap_or("<auto-denied>".into()),
                responded_at: now,
                channel: DeliveryChannel::Cli, // kernel-generated
            });
        }
    }
}
```

---

### ISSUE-7: `BusMessage::NotificationPush` belongs in Phase 1, not Phase 2

Phase 2 introduces `BusMessage::NotificationPush(UserMessage)` for the web server to subscribe to notifications via the bus. But the bus message enum is defined in `agentos-bus` which Phase 1 modifies. Phase 2 should not introduce bus message variants — that creates a confusing dependency where Phase 2 requires bus changes that Phase 1 should own.

**Fix**: Move `BusMessage::NotificationPush(UserMessage)` into Phase 1's subtask 1.3 bus message changes.

---

### ISSUE-8: Slack is structurally outbound-only

The Phase 5 Slack adapter sends Block Kit messages, which can include buttons. But Slack interactive buttons require:
1. A Slack app with Interactivity enabled
2. A public HTTPS endpoint that Slack POSTs to when buttons are clicked
3. Signature verification of Slack payloads

The Phase 5 plan says users should "reply via `agentctl notifications respond` or the web UI" — but this defeats the purpose of a Slack channel. Users in Slack want to reply IN Slack.

**Fix**: Phase 5 Slack adapter is correctly scoped as **outbound-only notification**. Mark this clearly. True bidirectional Slack (with interactive buttons) requires a separate `SlackInteractivityAdapter` using Slack Events API — deferred to Phase 6 or later.

---

### ISSUE-9: No message threading / conversation context

Each `UserMessage` is a standalone notification. In a real communication channel (Telegram, email), messages from the same task or agent should appear as a thread. Without threading:
- User gets 5 unrelated notifications about the same task
- Responses are not linked to the original question in the Telegram UI
- Email chains are broken

**Fix**: Add optional threading fields to `UserMessage`:

```rust
pub struct UserMessage {
    // ... existing fields ...
    pub thread_id: Option<String>,          // groups related messages in inbox
    pub reply_to_external_id: Option<String>, // for Telegram: reply to a specific message_id
}
```

The `UserInbox` should support fetching by `thread_id`. Adapters that support threading (Telegram, email `In-Reply-To`) should use `reply_to_external_id` to chain replies.

---

## Medium-Severity Issues

### ISSUE-10: No channel health monitoring

`is_available()` returns `true` unconditionally for webhook/Slack/Telegram adapters. In production, if a webhook endpoint is down for 2 hours, the `NotificationRouter` will keep trying to deliver to it (3 retries per message) for every notification, wasting time.

**Fix**: Add circuit breaker state to each adapter:
```rust
struct CircuitBreaker {
    consecutive_failures: u32,
    open_until: Option<DateTime<Utc>>,  // don't try again until
}
```
After 5 consecutive failures: open circuit for 5 minutes. `is_available()` returns false when circuit is open.

---

### ISSUE-11: `ask_user` timeout and `expires_at` can drift

In Phase 3, the timeout is calculated twice:
```rust
expires_at: Some(Utc::now() + Duration::from_secs(input.timeout_secs.unwrap_or(600))),
// ... later ...
let timeout = Duration::from_secs(input.timeout_secs.unwrap_or(600));
let response = tokio::time::timeout(timeout, rx).await;
```

Between these two calls, several milliseconds may pass, causing the tokio timeout to fire slightly before `expires_at`. The sweep timer then finds a non-expired question with no sender, causing a "no waiting task" error in `route_response`.

**Fix**: Calculate `expires_at` once, derive the timeout duration from it:
```rust
let expires_at = Utc::now() + Duration::from_secs(input.timeout_secs.unwrap_or(600));
let msg = UserMessage { expires_at: Some(expires_at), ... };
// ...
let timeout = (expires_at - Utc::now()).to_std().unwrap_or(Duration::from_secs(600));
let response = tokio::time::timeout(timeout, rx).await;
```

---

### ISSUE-12: No user notification preferences / filtering

The plan has `min_priority` per adapter but no:
- Per-kind filtering ("only TaskComplete on Telegram, no status updates")
- Quiet hours ("don't push between 11pm and 8am")
- Per-agent overrides ("never notify me about agent X")
- Digest mode ("batch notifications into a 5-minute summary")

**Fix**: Add `NotificationFilter` to config:
```toml
[notifications.filter]
min_priority = "info"
allowed_kinds = ["task_complete", "question"]  # empty = all
quiet_hours_start = "23:00"
quiet_hours_end = "08:00"
quiet_hours_tz = "UTC"
```

This is a non-critical enhancement but important for long-term usability.

---

## What's Missing Entirely: Phase 6

The current 5-phase plan covers only **outbound notifications + ask_user pausing**. The full bidirectional communication vision requires:

1. **`UserChannelRegistry`** — ecosystem-wide awareness of connected channels, persisted in vault
2. **`ChannelListener`** — per-channel background task that receives inbound messages (Telegram long-poll, ntfy SSE, IMAP IDLE)
3. **`InboundRouter`** — maps user messages to `KernelCommand` or new task creation
4. **`agentctl channel` commands** — `connect`, `list`, `disconnect`, `status`
5. **Channel setup flow** — guided setup: register bot, capture user ID, test connection
6. **Bidirectional `DeliveryAdapter`** trait with `listen()` method

Without Phase 6, the plan is a notification system, not a communication channel. See [[06-bidirectional-channel-protocol]] for full specification.

---

## Required Plan Updates

| Update | Phase | Action |
|--------|-------|--------|
| Move `BusMessage::NotificationPush` to Phase 1 | Phase 1 | Update subtask 1.3 |
| Change `DeliveryChannel` to include `Custom(String)` | Phase 1 | Update data model |
| Change `UserInbox` to use `sqlx` / tokio Mutex | Phase 1 | Update subtask 1.5 |
| Add `thread_id` and `reply_to_external_id` to `UserMessage` | Phase 1 | Update subtask 1.1 |
| Add `WaitingTaskMap` sweep to `TimeoutChecker` | Phase 3 | Update subtask 3.1 |
| Fix `expires_at` drift in `ask_user` | Phase 3 | Update subtask 3.2 |
| Add durable task-wait state to `KernelStateStore` | Phase 3 | New subtask 3.7 |
| Mark Slack as outbound-only, note interactive limitations | Phase 5 | Update description |
| Add circuit breaker to adapter health | Phase 5 | New subtask 5.6 |
| **Add `UserChannelRegistry` + `ChannelListener` + `InboundRouter`** | **Phase 6 (new)** | **New phase doc** |

---

## Revised Phase Table

| Phase | Name | Effort | Status |
|-------|------|--------|--------|
| 1 | UserMessage types + NotificationRouter + CLI | 2.5d | planned (updated) |
| 2 | SSE delivery + Web notification center | 2d | planned |
| 3 | ask_user tool + durable task blocking | 3d | planned (updated) |
| 4 | Task completion auto-notifications | 1d | planned |
| 5 | Pluggable external adapters (outbound) | 2.5d | planned |
| **6** | **Bidirectional Channel Protocol** | **4d** | **new** |

**Total: ~15 days** (was 10d). The additional 5 days is Phase 6 (the true bidirectional channel) and improved durability in Phase 3.

---

## Related

- [[User-Agent Communication Plan]] — master plan (needs updating)
- [[06-bidirectional-channel-protocol]] — Phase 6 spec
