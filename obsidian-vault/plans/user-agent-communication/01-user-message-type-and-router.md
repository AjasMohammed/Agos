---
title: "Phase 1: UserMessage Type + NotificationRouter + CLI Adapter"
tags:
  - kernel
  - types
  - cli
  - plan
  - phase-1
date: 2026-03-24
status: planned
effort: 2d
priority: high
---

# Phase 1: UserMessage Type + NotificationRouter + CLI Adapter

> The unblocking foundation. Introduce the `UserMessage` type, a `NotificationRouter` kernel subsystem, a `UserInbox` backed by SQLite, a CLI delivery adapter, and wire the existing-but-unconnected `StatusUpdate` bus message. All subsequent phases build on this.

**Depends on**: Nothing (standalone foundation)
**Blocks**: Phases 2, 3, 4, 5

---

## Why This Phase

Before agents can notify users or ask questions, three things must exist:
1. A **data model** (`UserMessage`) expressive enough to represent a notification, a question, a status update, or a task-complete signal — and usable by all delivery channels without modification.
2. A **kernel subsystem** (`NotificationRouter`) that receives messages from any source and dispatches to delivery adapters.
3. A **delivery adapter** that works in every AgentOS environment — the CLI — so even users with no web UI can receive notifications.

Without this phase, nothing in Phases 2–5 has anywhere to send messages.

---

## Current State vs. Target

| Item | Current | Target |
|------|---------|--------|
| `UserMessage` type | Does not exist | Defined in `agentos-types/src/notification.rs` |
| `NotificationID` newtype | Does not exist | Defined in `agentos-types/src/ids.rs` via `define_id!` |
| `NotificationRouter` | Does not exist | Kernel subsystem in `agentos-kernel/src/notification_router.rs` |
| `UserInbox` | Does not exist | SQLite-backed, in `KernelStateStore` |
| `CliDeliveryAdapter` | Does not exist | Writes to a UNIX socket queue + agentctl commands |
| `StatusUpdate` bus wiring | Type exists, never sent | Kernel sends on every `TaskState` change |
| `agentctl notifications` | Does not exist | `list`, `read`, `watch` subcommands |
| `user.notify` permission | Does not exist | Added to `PermissionSet` + `PermissionOp` |
| Audit events | Partial | `NotificationSent`, `NotificationDelivered`, `NotificationRead` added |

---

## Detailed Subtasks

### 1.1 — Add types to `agentos-types`

**File**: `crates/agentos-types/src/notification.rs` (new file)

Create the following types. These must be `#[derive(Debug, Clone, Serialize, Deserialize)]`. All optional fields use `#[serde(skip_serializing_if = "Option::is_none")]`.

```rust
use crate::{AgentID, TaskID, TraceID, TaskState};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

define_id!(NotificationID);

pub struct UserMessage {
    pub id: NotificationID,
    pub from: NotificationSource,
    pub task_id: Option<TaskID>,
    pub trace_id: TraceID,
    pub kind: UserMessageKind,
    pub priority: NotificationPriority,
    pub subject: String,              // ≤80 chars; used for CLI one-liner + email subject
    pub body: String,                 // Full markdown
    pub interaction: Option<InteractionRequest>,
    pub delivery_status: HashMap<DeliveryChannel, DeliveryStatus>,
    pub response: Option<UserResponse>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub read: bool,
}

pub enum NotificationSource {
    Agent(AgentID),
    Kernel,
    System,
}

pub enum UserMessageKind {
    Notification,
    Question {
        question: String,
        options: Option<Vec<String>>,  // None = free text
        free_text_allowed: bool,
    },
    TaskComplete {
        task_id: TaskID,
        outcome: TaskOutcome,
        summary: String,
        duration_ms: u64,
        iterations: u32,
        cost_usd: Option<f64>,
        tool_calls: u32,
    },
    StatusUpdate {
        task_id: TaskID,
        old_state: TaskState,
        new_state: TaskState,
        detail: Option<String>,
    },
}

pub enum NotificationPriority { Info, Warning, Urgent, Critical }

pub struct InteractionRequest {
    pub timeout_secs: u64,
    pub auto_action: String,     // text returned if timeout; e.g. "<auto-denied>"
    pub blocking: bool,
    pub max_concurrent: u8,      // default 3 — max blocking questions from one agent
}

pub struct UserResponse {
    pub text: String,
    pub responded_at: DateTime<Utc>,
    pub channel: DeliveryChannel,
}

pub enum DeliveryChannel { Cli, Web, Webhook, Desktop, Slack }

pub enum DeliveryStatus {
    Pending,
    Delivered { at: DateTime<Utc> },
    Failed { reason: String },
    Skipped,
}

pub enum TaskOutcome { Success, Failed, Cancelled, TimedOut }
```

Add `NotificationID` to `crates/agentos-types/src/ids.rs`:
```rust
define_id!(NotificationID);
```

Re-export from `crates/agentos-types/src/lib.rs`:
```rust
pub mod notification;
pub use notification::{
    UserMessage, UserMessageKind, NotificationPriority, NotificationSource,
    InteractionRequest, UserResponse, DeliveryChannel, DeliveryStatus,
    TaskOutcome, NotificationID,
};
```

---

### 1.2 — Add `user.notify` and `user.interact` permissions

**File**: `crates/agentos-types/src/capability.rs`

In `PermissionOp` enum, add (if not present):
```rust
pub enum PermissionOp {
    // ... existing ...
    Notify,     // user.notify — send fire-and-forget notification
    Interact,   // user.interact — send blocking question
}
```

In `PermissionEntry`, the existing `read/write/execute/query/observe` fields are already generic enough — use `write` for `user.notify` and `execute` for `user.interact`, matching the resource string `"user.notify"` and `"user.interact"`. No struct change needed; just document the new resource strings.

**File**: `crates/agentos-capability/src/lib.rs` (or wherever default permission sets are defined)

Add constants:
```rust
pub const PERM_USER_NOTIFY: &str = "user.notify";
pub const PERM_USER_INTERACT: &str = "user.interact";
pub const PERM_USER_STATUS: &str = "user.status";
```

---

### 1.3 — Add `KernelCommand` variants for notifications

**File**: `crates/agentos-bus/src/message.rs`

Add to `KernelCommand`:
```rust
KernelCommand::SendUserNotification {
    subject: String,
    body: String,
    priority: NotificationPriority,
    kind: Option<UserMessageKind>,   // defaults to Notification if None
    trace_id: TraceID,
},
KernelCommand::AskUser {
    question: String,
    context_summary: String,
    decision_point: String,
    options: Option<Vec<String>>,
    timeout_secs: u64,
    blocking: bool,
    trace_id: TraceID,
},
KernelCommand::RespondToNotification {
    notification_id: NotificationID,
    response_text: String,
    channel: DeliveryChannel,
},
KernelCommand::ListNotifications {
    unread_only: bool,
    limit: u32,
},
KernelCommand::MarkNotificationRead {
    notification_id: NotificationID,
},
```

Add to `KernelResponse`:
```rust
KernelResponse::NotificationList(Vec<UserMessage>),
KernelResponse::NotificationSent { id: NotificationID },
```

Also wire `StatusUpdate` — it already exists in `BusMessage`:
```rust
pub enum BusMessage {
    // ... existing ...
    StatusUpdate(StatusUpdate),  // already defined, now actually sent
}
```

The kernel must send `StatusUpdate` on every `TaskState` change. Find where task state transitions happen in `crates/agentos-kernel/src/task_executor.rs` and emit via the bus connection.

---

### 1.4 — Create `NotificationRouter` in kernel

**File**: `crates/agentos-kernel/src/notification_router.rs` (new file)

```rust
pub struct NotificationRouter {
    inbox: Arc<UserInbox>,
    adapters: Vec<Box<dyn DeliveryAdapter>>,
    waiting_tasks: Arc<RwLock<HashMap<NotificationID, oneshot::Sender<UserResponse>>>>,
    rate_limiter: Arc<RwLock<HashMap<AgentID, RateLimiterState>>>,
}

impl NotificationRouter {
    pub fn new(inbox: Arc<UserInbox>, adapters: Vec<Box<dyn DeliveryAdapter>>) -> Self { ... }

    /// Called by kernel command handler for SendUserNotification / AskUser
    pub async fn deliver(
        &self,
        msg: UserMessage,
        // If blocking: Some(rx) is stored; caller awaits rx
    ) -> Result<Option<oneshot::Receiver<UserResponse>>, AgentOSError> { ... }

    /// Called when user responds (via CLI or web)
    pub async fn route_response(
        &self,
        notification_id: NotificationID,
        response: UserResponse,
    ) -> Result<(), AgentOSError> { ... }

    /// Rate limit: max 10 notifications/min per agent; max 3 concurrent blocking per agent
    fn check_rate_limit(&self, from: &NotificationSource) -> Result<(), AgentOSError> { ... }
}

#[async_trait]
pub trait DeliveryAdapter: Send + Sync {
    fn channel_id(&self) -> DeliveryChannel;
    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError>;
    async fn is_available(&self) -> bool;
}
```

---

### 1.5 — Create `UserInbox`

**File**: `crates/agentos-kernel/src/user_inbox.rs` (new file)

```rust
pub struct UserInbox {
    db: Arc<Mutex<Connection>>,  // SQLite via rusqlite
}

impl UserInbox {
    pub fn new(db_path: &Path) -> Result<Self, AgentOSError> { ... }
    pub async fn write(&self, msg: &UserMessage) -> Result<(), AgentOSError> { ... }
    pub async fn update_delivery_status(&self, id: &NotificationID, channel: DeliveryChannel, status: DeliveryStatus) -> Result<(), AgentOSError> { ... }
    pub async fn mark_read(&self, id: &NotificationID) -> Result<(), AgentOSError> { ... }
    pub async fn set_response(&self, id: &NotificationID, response: &UserResponse) -> Result<(), AgentOSError> { ... }
    pub async fn list(&self, unread_only: bool, limit: usize) -> Result<Vec<UserMessage>, AgentOSError> { ... }
    pub async fn get(&self, id: &NotificationID) -> Result<Option<UserMessage>, AgentOSError> { ... }
}
```

Schema (use parameterized queries only — no string interpolation):
```sql
CREATE TABLE IF NOT EXISTS user_messages (
    id          TEXT PRIMARY KEY,
    from_source TEXT NOT NULL,   -- JSON: NotificationSource
    task_id     TEXT,
    trace_id    TEXT NOT NULL,
    kind        TEXT NOT NULL,   -- JSON: UserMessageKind
    priority    TEXT NOT NULL,
    subject     TEXT NOT NULL,
    body        TEXT NOT NULL,
    interaction TEXT,            -- JSON: InteractionRequest or NULL
    delivery_status TEXT NOT NULL, -- JSON: HashMap<DeliveryChannel, DeliveryStatus>
    response    TEXT,            -- JSON: UserResponse or NULL
    created_at  TEXT NOT NULL,
    expires_at  TEXT,
    read        INTEGER NOT NULL DEFAULT 0
);
```

Purge policy: when inbox exceeds 1000 messages, delete the 100 oldest read messages on each write.

---

### 1.6 — Create `CliDeliveryAdapter`

**File**: `crates/agentos-kernel/src/notification_router.rs` (inner module or same file)

```rust
pub struct CliDeliveryAdapter {
    // Writes notification summary to a well-known path
    // ($XDG_RUNTIME_DIR/agentos/notifications.jsonl or similar)
    // CLI reads this file when user runs `agentctl notifications list`
    // Alternative: keep in-memory + SQLite UserInbox is the source (CLI queries kernel)
}

impl DeliveryAdapter for CliDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel { DeliveryChannel::Cli }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        // For now: write to UserInbox (already done by router before this call)
        // The CLI queries kernel directly — this adapter is a no-op stub
        // Future: print a badge/indicator to an active CLI session if one is connected
        Ok(())
    }

    async fn is_available(&self) -> bool { true }
}
```

The CLI delivery model for Phase 1 is simple: the `UserInbox` SQLite DB is the notification store; the CLI reads from it via `agentctl notifications list`. No persistent connection required.

---

### 1.7 — Add command dispatch in kernel

**File**: `crates/agentos-kernel/src/commands/` (new file: `notification.rs`)

```rust
pub async fn handle_send_notification(
    kernel: &Kernel,
    subject: String,
    body: String,
    priority: NotificationPriority,
    kind: Option<UserMessageKind>,
    trace_id: TraceID,
    from_agent: Option<AgentID>,
) -> Result<KernelResponse, AgentOSError> {
    // 1. validate user.notify permission for from_agent
    // 2. build UserMessage
    // 3. call kernel.notification_router.deliver(msg, None).await
    // 4. write audit: NotificationSent
    // 5. return KernelResponse::NotificationSent { id }
}

pub async fn handle_list_notifications(
    kernel: &Kernel,
    unread_only: bool,
    limit: u32,
) -> Result<KernelResponse, AgentOSError> {
    let msgs = kernel.user_inbox.list(unread_only, limit as usize).await?;
    Ok(KernelResponse::NotificationList(msgs))
}

pub async fn handle_mark_read(kernel: &Kernel, id: NotificationID) -> Result<KernelResponse, AgentOSError> {
    kernel.user_inbox.mark_read(&id).await?;
    Ok(KernelResponse::Success { data: None })
}
```

Add dispatch arms in `crates/agentos-kernel/src/run_loop.rs` (or wherever commands are dispatched).

---

### 1.8 — Add `agentctl notifications` CLI subcommand

**File**: `crates/agentos-cli/src/commands/notifications.rs` (new file)

```
agentctl notifications list [--unread] [--limit N]
agentctl notifications read <id>
agentctl notifications watch   (polls every 5s, prints new messages)
```

- `list`: print table of subject, priority, from, time, read status
- `read <id>`: print full body + mark as read
- `watch`: poll loop (for Phase 1; upgraded to SSE push in Phase 2)

Add to `crates/agentos-cli/src/main.rs` under a `notifications` subcommand group.

---

### 1.9 — Wire `StatusUpdate` to bus

**File**: `crates/agentos-kernel/src/task_executor.rs`

Find every location where `task.state` changes. After each state change, send a `BusMessage::StatusUpdate(StatusUpdate { task_id, state, message })` via the kernel's bus connection. The CLI currently ignores these — that's fine for Phase 1. Phase 2 will display them in the web UI via SSE.

---

### 1.10 — Add audit events

**File**: `crates/agentos-audit/src/lib.rs` (or wherever `AuditEventType` is defined)

Add:
```rust
AuditEventType::NotificationSent,
AuditEventType::NotificationDelivered,
AuditEventType::NotificationRead,
AuditEventType::UserResponseReceived,
AuditEventType::NotificationAutoActioned,
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/notification.rs` | NEW — UserMessage, all related types |
| `crates/agentos-types/src/ids.rs` | Add `define_id!(NotificationID)` |
| `crates/agentos-types/src/lib.rs` | Re-export notification module |
| `crates/agentos-bus/src/message.rs` | Add KernelCommand variants, KernelResponse variants |
| `crates/agentos-kernel/src/notification_router.rs` | NEW — NotificationRouter, DeliveryAdapter trait, CliDeliveryAdapter |
| `crates/agentos-kernel/src/user_inbox.rs` | NEW — UserInbox with SQLite backend |
| `crates/agentos-kernel/src/commands/notification.rs` | NEW — command handlers |
| `crates/agentos-kernel/src/run_loop.rs` | Add dispatch arms for new commands |
| `crates/agentos-kernel/src/kernel.rs` | Add `notification_router` and `user_inbox` fields |
| `crates/agentos-kernel/src/task_executor.rs` | Emit StatusUpdate on state changes |
| `crates/agentos-cli/src/commands/notifications.rs` | NEW — notifications subcommand |
| `crates/agentos-cli/src/main.rs` | Register notifications subcommand |
| `crates/agentos-audit/src/lib.rs` | Add notification audit event types |
| `config/default.toml` | Add `[notifications]` section (inbox_path, max_inbox_size) |

---

## Dependencies

**Requires**: Nothing upstream — this is the foundation.
**Blocks**: [[02-sse-delivery-and-web-inbox]], [[03-ask-user-tool]], [[04-task-completion-notifications]], [[05-pluggable-delivery-adapters]]

---

## Test Plan

```rust
// crates/agentos-kernel/tests/notification_router_test.rs

#[tokio::test]
async fn test_notification_written_to_inbox() {
    // setup kernel with mock adapters
    // send SendUserNotification command
    // assert UserInbox.list() returns the message
}

#[tokio::test]
async fn test_notification_requires_permission() {
    // agent without user.notify permission
    // assert command returns PermissionDenied error
}

#[tokio::test]
async fn test_list_notifications_unread_only() {
    // insert 3 messages, mark 1 as read
    // list with unread_only=true → expect 2
}

#[tokio::test]
async fn test_status_update_sent_on_task_state_change() {
    // run a task to completion
    // capture BusMessage::StatusUpdate events
    // assert at least Running + Complete states were emitted
}

#[tokio::test]
async fn test_rate_limit_notifications() {
    // send 11 notifications from same agent rapidly
    // assert 11th returns RateLimitExceeded
}
```

---

## Verification

After implementation:
```bash
# Build must pass
cargo build -p agentos-types -p agentos-kernel -p agentos-cli

# All tests pass
cargo test -p agentos-kernel -- notification

# Run kernel + send a notification via CLI
agentctl kernel start &
agentctl notifications list
# expect: (empty)

# Simulate: connect as agent and send notification
# (via a test agent or direct bus command)

# CLI should show the notification
agentctl notifications list
# expect: row with subject, priority, from, time, unread

agentctl notifications read <id>
# expect: full body printed; marked as read

agentctl notifications list
# expect: same row but marked as read

# Clippy + fmt
cargo clippy -p agentos-kernel -p agentos-types -p agentos-cli -- -D warnings
cargo fmt --all -- --check
```
