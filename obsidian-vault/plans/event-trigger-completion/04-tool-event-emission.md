---
title: "Phase 04 — Tool Event Emission"
tags:
  - kernel
  - event-system
  - tools
  - plan
  - v3
date: 2026-03-13
status: complete
effort: 2h
priority: medium
---
# Phase 04 — Tool Event Emission

> Wire ToolInstalled, ToolRemoved, and ToolExecutionFailed events into the tool registry and task executor.

---

## Why This Phase

Tool events enable tool-manager agents to react when tools are added, removed, or fail during execution. `ToolExecutionFailed` is particularly valuable — it lets agents retry with different parameters, switch to alternative tools, or escalate if a critical tool is broken.

---

## Current State

| What | Status |
|------|--------|
| `EventType::ToolInstalled` / `ToolRemoved` / `ToolExecutionFailed` | Defined in `agentos-types/src/event.rs` |
| `ToolRegistry::register()` | Working — returns `Result<ToolID, AgentOSError>` |
| `ToolRegistry::remove()` | Working — removes tool by ID |
| Tool execution in task_executor.rs | Working — runs tool, handles errors |
| **Event emission in any of these** | **None** |
| `ToolRegistry` has no access to `event_sender` | Needs injection |

---

## Target State

- `ToolInstalled` emitted after successful `ToolRegistry::register()`
- `ToolRemoved` emitted after successful `ToolRegistry::remove()`
- `ToolExecutionFailed` emitted in `task_executor.rs` when a tool call returns an error

---

## Subtasks

### 1. Add `event_sender` to `ToolRegistry`

**File:** `crates/agentos-kernel/src/tool_registry.rs`

`ToolRegistry` currently has no way to emit events. Add an optional sender:

```rust
pub struct ToolRegistry {
    tools: HashMap<ToolID, ToolManifestWrapper>,
    // ... existing fields ...
    event_sender: Option<mpsc::UnboundedSender<EventMessage>>,
}
```

Add a setter method:

```rust
pub fn set_event_sender(&mut self, sender: mpsc::UnboundedSender<EventMessage>) {
    self.event_sender = Some(sender);
}
```

**File:** `crates/agentos-kernel/src/kernel.rs`

In `Kernel::new()`, after creating the tool_registry and event channel, call:

```rust
tool_registry.write().await.set_event_sender(event_sender.clone());
```

### 2. Emit `ToolInstalled` in `ToolRegistry::register()`

**File:** `crates/agentos-kernel/src/tool_registry.rs`

**Where:** After the tool is successfully inserted into the registry map, before returning the `ToolID`.

```rust
if let Some(ref sender) = self.event_sender {
    use agentos_types::event::*;
    let event = EventMessage {
        id: EventID::new(),
        event_type: EventType::ToolInstalled,
        source: EventSource::ToolRunner,
        payload: serde_json::json!({
            "tool_id": tool_id.to_string(),
            "tool_name": manifest.manifest.name,
            "trust_tier": format!("{:?}", manifest.manifest.trust_tier),
            "description": manifest.manifest.description,
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

### 3. Emit `ToolRemoved` in `ToolRegistry::remove()`

**File:** `crates/agentos-kernel/src/tool_registry.rs`

**Where:** After successful removal from the map.

```rust
if let Some(ref sender) = self.event_sender {
    let event = EventMessage {
        id: EventID::new(),
        event_type: EventType::ToolRemoved,
        source: EventSource::ToolRunner,
        payload: serde_json::json!({
            "tool_id": tool_id.to_string(),
            "tool_name": removed_tool_name,
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

### 4. Emit `ToolExecutionFailed` in `task_executor.rs`

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** In the tool execution path, when the tool runner returns an error. This is inside the kernel context so `self.emit_event()` works directly.

```rust
// After tool execution fails:
self.emit_event(
    EventType::ToolExecutionFailed,
    EventSource::ToolRunner,
    EventSeverity::Warning,
    serde_json::json!({
        "task_id": task.id.to_string(),
        "agent_id": task.agent_id.to_string(),
        "tool_name": tool_name,
        "error": error.to_string(),
    }),
    0,
).await;
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/tool_registry.rs` | Add `event_sender` field, `set_event_sender()`, emit ToolInstalled/ToolRemoved |
| `crates/agentos-kernel/src/task_executor.rs` | Add ToolExecutionFailed emission in tool error path |
| `crates/agentos-kernel/src/kernel.rs` | Call `set_event_sender()` on tool_registry during init |

---

## Dependencies

None — can be done in parallel with Phases 01, 02, 05, 06.

---

## Test Plan

1. **ToolInstalled test:** Register a tool, verify event_sender receives `ToolInstalled` event with correct tool name and trust tier.

2. **ToolRemoved test:** Register then remove a tool, verify `ToolRemoved` event.

3. **ToolExecutionFailed test:** Mock a tool that returns an error, execute it through the task executor, verify `ToolExecutionFailed` event with error message.

4. **No sender test:** Verify `ToolRegistry` works correctly when `event_sender` is `None` (backward compatibility).

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel

grep -n "ToolInstalled" crates/agentos-kernel/src/tool_registry.rs
grep -n "ToolRemoved" crates/agentos-kernel/src/tool_registry.rs
grep -n "ToolExecutionFailed" crates/agentos-kernel/src/task_executor.rs
```

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[agentos-event-trigger-system]] — Original spec §3 (ToolEvents category)
