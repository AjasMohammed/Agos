---
title: "Phase 06 — Communication & Schedule Event Emission"
tags:
  - kernel
  - event-system
  - communication
  - scheduling
  - plan
  - v3
date: 2026-03-13
status: complete
effort: 3h
priority: medium
---
# Phase 06 — Communication & Schedule Event Emission

> Wire DirectMessageReceived, BroadcastReceived, MessageDeliveryFailed into AgentMessageBus and CronJobFired, ScheduledTaskMissed, ScheduledTaskFailed into ScheduleManager.

---

## Why This Phase

Agent communication events are the backbone of multi-agent coordination. Without `DirectMessageReceived`, agents cannot be triggered when another agent sends them a message — the entire delegation and collaboration model depends on this. Schedule events enable agents to respond to cron job lifecycle, detecting missed or failed scheduled tasks.

---

## Current State

| What | Status |
|------|--------|
| `EventType::DirectMessageReceived` / `BroadcastReceived` / `MessageDeliveryFailed` | Defined in `agentos-types/src/event.rs` |
| `EventType::CronJobFired` / `ScheduledTaskMissed` / `ScheduledTaskFailed` | Defined in `agentos-types/src/event.rs` |
| `AgentMessageBus` — `send_direct()`, `broadcast()` methods | Working |
| `ScheduleManager` — `check_due_jobs()` method | Working — identifies due cron jobs |
| **Event emission in either subsystem** | **None** |
| Neither subsystem has access to `event_sender` | Needs injection |

---

## Target State

- `DirectMessageReceived` emitted after successful `send_direct()` delivery
- `BroadcastReceived` emitted for each recipient after `broadcast()` delivery
- `MessageDeliveryFailed` emitted when `send_direct()` or `broadcast()` fails
- `CronJobFired` emitted when `check_due_jobs()` identifies a due job
- `ScheduledTaskMissed` emitted when a due job's target agent is unavailable
- `ScheduledTaskFailed` emitted when a scheduled task completes with error

---

## Subtasks

### 1. Add `event_sender` to `AgentMessageBus`

**File:** `crates/agentos-kernel/src/agent_message_bus.rs`

Add an optional event sender field and setter, same pattern as Phase 04 for ToolRegistry:

```rust
pub struct AgentMessageBus {
    // ... existing fields ...
    event_sender: Option<mpsc::UnboundedSender<EventMessage>>,
}

impl AgentMessageBus {
    pub fn set_event_sender(&mut self, sender: mpsc::UnboundedSender<EventMessage>) {
        self.event_sender = Some(sender);
    }
}
```

**File:** `crates/agentos-kernel/src/kernel.rs`

Wire the sender during kernel init:

```rust
agent_message_bus.write().await.set_event_sender(event_sender.clone());
```

### 2. Emit `DirectMessageReceived` in `send_direct()`

**File:** `crates/agentos-kernel/src/agent_message_bus.rs`

**Where:** After the message is successfully delivered to the recipient's inbox/queue.

```rust
if let Some(ref sender) = self.event_sender {
    let event = EventMessage {
        id: EventID::new(),
        event_type: EventType::DirectMessageReceived,
        source: EventSource::AgentMessageBus,
        payload: serde_json::json!({
            "from_agent": message.from.to_string(),
            "to_agent": message.to.to_string(),
            "message_id": message.id.to_string(),
        }),
        severity: EventSeverity::Info,
        timestamp: chrono::Utc::now(),
        signature: vec![],
        trace_id: uuid::Uuid::new_v4().to_string(),
        chain_depth: 0,
    };
    let _ = sender.send(event);
}
```

### 3. Emit `BroadcastReceived` in `broadcast()`

**File:** `crates/agentos-kernel/src/agent_message_bus.rs`

**Where:** After the broadcast is delivered. Emit one event per recipient or a single event with the recipient list:

```rust
if let Some(ref sender) = self.event_sender {
    let event = EventMessage {
        id: EventID::new(),
        event_type: EventType::BroadcastReceived,
        source: EventSource::AgentMessageBus,
        payload: serde_json::json!({
            "from_agent": message.from.to_string(),
            "recipient_count": recipient_count,
            "message_id": message.id.to_string(),
        }),
        severity: EventSeverity::Info,
        timestamp: chrono::Utc::now(),
        signature: vec![],
        trace_id: uuid::Uuid::new_v4().to_string(),
        chain_depth: 0,
    };
    let _ = sender.send(event);
}
```

### 4. Emit `MessageDeliveryFailed` on errors

**File:** `crates/agentos-kernel/src/agent_message_bus.rs`

**Where:** In error paths of `send_direct()` and `broadcast()` (e.g., recipient not found, queue full).

```rust
if let Some(ref sender) = self.event_sender {
    let event = EventMessage {
        id: EventID::new(),
        event_type: EventType::MessageDeliveryFailed,
        source: EventSource::AgentMessageBus,
        payload: serde_json::json!({
            "from_agent": from_id.to_string(),
            "to_agent": to_id.to_string(),
            "error": error.to_string(),
        }),
        severity: EventSeverity::Warning,
        timestamp: chrono::Utc::now(),
        signature: vec![],
        trace_id: uuid::Uuid::new_v4().to_string(),
        chain_depth: 0,
    };
    let _ = sender.send(event);
}
```

### 5. Add `event_sender` to `ScheduleManager`

**File:** `crates/agentos-kernel/src/schedule_manager.rs`

Same pattern as AgentMessageBus — add optional sender field and setter.

### 6. Emit `CronJobFired` in `check_due_jobs()`

**File:** `crates/agentos-kernel/src/schedule_manager.rs`

**Where:** When a job's `next_run_at <= now`, after the job is identified as due and before/after the task is created.

```rust
if let Some(ref sender) = self.event_sender {
    let event = EventMessage {
        id: EventID::new(),
        event_type: EventType::CronJobFired,
        source: EventSource::Scheduler,
        payload: serde_json::json!({
            "schedule_id": job.id.to_string(),
            "schedule_name": job.name,
            "cron_expression": job.cron_expression,
            "run_count": job.run_count,
        }),
        severity: EventSeverity::Info,
        timestamp: chrono::Utc::now(),
        signature: vec![],
        trace_id: uuid::Uuid::new_v4().to_string(),
        chain_depth: 0,
    };
    let _ = sender.send(event);
}
```

### 7. Emit `ScheduledTaskMissed` when target agent unavailable

**File:** `crates/agentos-kernel/src/schedule_manager.rs`

**Where:** If a due job fires but the target agent is not connected or available, emit with `Warning` severity.

### 8. Emit `ScheduledTaskFailed` on scheduled task error

**File:** `crates/agentos-kernel/src/schedule_manager.rs`

**Where:** After a task created by a cron job completes with an error status.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/agent_message_bus.rs` | Add `event_sender`, emit 3 event types |
| `crates/agentos-kernel/src/schedule_manager.rs` | Add `event_sender`, emit 3 event types |
| `crates/agentos-kernel/src/kernel.rs` | Wire `event_sender` to both subsystems |

---

## Dependencies

None — can be done in parallel with Phases 01, 02, 04, 05.

---

## Test Plan

1. **DirectMessage test:** Send a direct message between two mock agents, verify `DirectMessageReceived` event.

2. **Broadcast test:** Send a broadcast, verify `BroadcastReceived` event with correct recipient count.

3. **Delivery failure test:** Send to a non-existent agent, verify `MessageDeliveryFailed` event.

4. **CronJob test:** Create a due cron job, run `check_due_jobs()`, verify `CronJobFired` event.

5. **No sender test:** Verify both subsystems work correctly when `event_sender` is `None`.

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel

grep -n "DirectMessageReceived" crates/agentos-kernel/src/agent_message_bus.rs
grep -n "BroadcastReceived" crates/agentos-kernel/src/agent_message_bus.rs
grep -n "CronJobFired" crates/agentos-kernel/src/schedule_manager.rs
```

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[agentos-event-trigger-system]] — Original spec §3 (AgentCommunication, ScheduleEvents categories)
