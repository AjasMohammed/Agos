---
title: Event HMAC Signing and Audit Log Fix
tags:
  - kernel
  - security
  - v3
  - bugfix
date: 2026-03-13
status: complete
effort: 4h
priority: high
---

# Event HMAC Signing and Audit Log Fix

> Refactor `AgentMessageBus` and `ScheduleManager` to use the lifecycle event pattern (like `ToolRegistry`) so their events are HMAC-signed and written to the audit log.

---

## Why This Phase

This is Issue #9 from the Issues and Fixes audit -- the only medium-severity architectural issue that remains unfixed. `AgentMessageBus` and `ScheduleManager` emit `EventMessage` values directly into the kernel's event channel with `signature: vec![]`, bypassing HMAC-SHA256 signing and audit log writes. This creates two gaps:

1. **Unsigned events** -- if signature verification is added to the dispatch path, all communication and schedule events will be silently dropped.
2. **Invisible to audit** -- message delivery failures and missed cron jobs do not appear in the audit log, violating the project invariant that all security-relevant operations are logged.

The correct pattern already exists: `ToolRegistry` sends lightweight `ToolLifecycleEvent` notifications through a separate channel, and the kernel run loop (`run_loop.rs`) processes them via the properly signing/auditing `emit_event` path.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `AgentMessageBus` event emission | Constructs `EventMessage` with `signature: vec![]`, sends via raw `event_sender` | Sends `CommunicationLifecycleEvent` enum variant via typed channel; kernel run loop calls `emit_event` |
| `ScheduleManager` event emission | Same as above | Sends `ScheduleLifecycleEvent` enum variant; kernel run loop calls `emit_event` |
| Audit log coverage | Communication/schedule events not audited | All events flow through `emit_event` which writes to `AuditLog` |
| HMAC signatures | `signature: vec![]` on communication/schedule events | Events signed by `CapabilityEngine` in `emit_event` |

---

## What to Do

### 1. Define `CommunicationLifecycleEvent` enum

Open `crates/agentos-kernel/src/agent_message_bus.rs`.

Add a new enum at the top of the file (before the `AgentMessageBus` struct):

```rust
/// Lightweight notification for communication events.
/// The kernel run loop converts these into properly signed EventMessages.
#[derive(Debug, Clone)]
pub enum CommunicationLifecycleEvent {
    DirectMessageDelivered {
        from_agent: AgentID,
        to_agent: AgentID,
        message_id: agentos_types::MessageID,
    },
    BroadcastDelivered {
        from_agent: AgentID,
        recipient_count: u32,
        message_id: agentos_types::MessageID,
    },
    DeliveryFailed {
        from_agent: AgentID,
        to_agent: Option<AgentID>,
        error: String,
    },
}
```

### 2. Replace `event_sender` with typed lifecycle sender in `AgentMessageBus`

In the `AgentMessageBus` struct:

- Change the `event_sender` field type from `RwLock<Option<mpsc::UnboundedSender<EventMessage>>>` to `RwLock<Option<mpsc::UnboundedSender<CommunicationLifecycleEvent>>>`
- Rename `set_event_sender` to `set_lifecycle_sender` (or keep the name but change the type)
- Replace the `emit_event` method body: instead of constructing an `EventMessage`, construct the appropriate `CommunicationLifecycleEvent` variant and send it

### 3. Define `ScheduleLifecycleEvent` enum

Open `crates/agentos-kernel/src/schedule_manager.rs`.

Add a new enum:

```rust
/// Lightweight notification for schedule events.
#[derive(Debug, Clone)]
pub enum ScheduleLifecycleEvent {
    CronJobFired {
        schedule_id: ScheduleID,
        schedule_name: String,
        cron_expression: String,
        run_count: u64,
    },
    TaskMissed {
        schedule_id: ScheduleID,
        schedule_name: String,
        agent_name: String,
        reason: String,
    },
    TaskFailed {
        schedule_id: ScheduleID,
        schedule_name: String,
        agent_name: String,
        error: String,
    },
}
```

### 4. Replace `event_sender` in `ScheduleManager` with typed lifecycle sender

Same refactor as step 2: change the sender type, update `emit_event` to construct `ScheduleLifecycleEvent` variants.

### 5. Add lifecycle receiver fields to `Kernel`

Open `crates/agentos-kernel/src/kernel.rs`.

Add two new fields to the `Kernel` struct:

```rust
pub(crate) comm_lifecycle_receiver: Arc<
    tokio::sync::Mutex<
        tokio::sync::mpsc::UnboundedReceiver<crate::agent_message_bus::CommunicationLifecycleEvent>,
    >,
>,
pub(crate) schedule_lifecycle_receiver: Arc<
    tokio::sync::Mutex<
        tokio::sync::mpsc::UnboundedReceiver<crate::schedule_manager::ScheduleLifecycleEvent>,
    >,
>,
```

In `Kernel::boot()`, create the channels and inject senders:

```rust
let (comm_lifecycle_sender, comm_lifecycle_receiver) = tokio::sync::mpsc::unbounded_channel();
message_bus.set_lifecycle_sender(comm_lifecycle_sender).await;

let (schedule_lifecycle_sender, schedule_lifecycle_receiver) = tokio::sync::mpsc::unbounded_channel();
schedule_manager.set_lifecycle_sender(schedule_lifecycle_sender).await;
```

### 6. Add processing functions in `run_loop.rs`

Open `crates/agentos-kernel/src/run_loop.rs`.

Add a new `TaskKind` variant: `CommunicationLifecycleListener` and `ScheduleLifecycleListener`.

Follow the exact pattern used for `ToolLifecycleListener` (search for `process_tool_lifecycle_event` in the codebase). The kernel receives each lifecycle event and calls `self.emit_event(...)` which handles HMAC signing and audit log writing.

Add processing functions:

```rust
async fn process_comm_lifecycle_event(
    &self,
    event: CommunicationLifecycleEvent,
) {
    match event {
        CommunicationLifecycleEvent::DirectMessageDelivered { from_agent, to_agent, message_id } => {
            self.emit_event(
                EventType::DirectMessageReceived,
                EventSource::AgentMessageBus,
                EventSeverity::Info,
                serde_json::json!({
                    "from_agent": from_agent.to_string(),
                    "to_agent": to_agent.to_string(),
                    "message_id": message_id.to_string(),
                }),
                0,
            ).await;
        }
        // ... other variants
    }
}
```

### 7. Update tests

Update tests in `agent_message_bus.rs` and `schedule_manager.rs`:
- Tests that check for `EventMessage` on the event channel should now check for `CommunicationLifecycleEvent` / `ScheduleLifecycleEvent` on the lifecycle channel
- Existing tests that verify "bus works without event sender" should verify "bus works without lifecycle sender"

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/agent_message_bus.rs` | Add `CommunicationLifecycleEvent` enum; replace `event_sender` with typed lifecycle sender; update `emit_event` and all tests |
| `crates/agentos-kernel/src/schedule_manager.rs` | Add `ScheduleLifecycleEvent` enum; replace `event_sender` with typed lifecycle sender; update `emit_event` and all tests |
| `crates/agentos-kernel/src/kernel.rs` | Add `comm_lifecycle_receiver` and `schedule_lifecycle_receiver` fields; create channels in `boot()` |
| `crates/agentos-kernel/src/run_loop.rs` | Add `CommunicationLifecycleListener` and `ScheduleLifecycleListener` task kinds; add `process_comm_lifecycle_event` and `process_schedule_lifecycle_event` functions |

---

## Prerequisites

None -- this can be done in parallel with Phase 01, but should be completed before Phase 05 (Issues audit update).

---

## Test Plan

- `cargo test -p agentos-kernel` must pass -- all existing event emission tests updated
- Add test: `test_direct_message_emits_lifecycle_event` -- send a direct message, verify `CommunicationLifecycleEvent::DirectMessageDelivered` received on the lifecycle channel
- Add test: `test_broadcast_emits_lifecycle_event` -- broadcast, verify `CommunicationLifecycleEvent::BroadcastDelivered` with correct `recipient_count`
- Add test: `test_delivery_failure_emits_lifecycle_event` -- send to nonexistent agent, verify `CommunicationLifecycleEvent::DeliveryFailed`
- Add test: `test_cron_job_fired_emits_lifecycle_event` -- create and fire a cron job, verify `ScheduleLifecycleEvent::CronJobFired`
- Verify: no more `signature: vec![]` in production code (only test helpers may use it)

---

## Verification

```bash
# No signature: vec![] in production code (agent_message_bus and schedule_manager)
grep -n "signature: vec!\[\]" crates/agentos-kernel/src/agent_message_bus.rs crates/agentos-kernel/src/schedule_manager.rs
# Should return no results

cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```
