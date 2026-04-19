---
title: "Phase 02 — Security Event Emission"
tags:
  - kernel
  - event-system
  - security
  - plan
  - v3
date: 2026-03-13
status: complete
effort: 3h
priority: critical
---
# Phase 02 — Security Event Emission

> Wire PromptInjectionAttempt, CapabilityViolation, and UnauthorizedToolAccess events into the kernel security paths.

---

## Why This Phase

Security events are the highest-priority event category — every occurrence must be seen (`ThrottlePolicy::None` per spec). Without emission, security-monitor agents cannot detect injection attempts, capability violations, or unauthorized tool access. These are the events that enable autonomous security response.

---

## Current State

| What | Status |
|------|--------|
| `EventType::PromptInjectionAttempt` / `CapabilityViolation` / `UnauthorizedToolAccess` | Defined in `agentos-types/src/event.rs` |
| `injection_scanner.rs` scans prompts and tool outputs | Working — returns `ScanResult` with `is_suspicious`, `matches`, `max_threat` |
| `task_executor.rs` checks scan results | Working — audits and can reject tasks |
| Capability validation in `task_executor.rs` | Working — checks `PermissionSet` before tool execution |
| `intent_validator.rs` validates intent coherence | Working — detects looping, scope escalation |
| **Event emission at any of these points** | **None** |

---

## Target State

- `PromptInjectionAttempt` emitted when `injection_scanner.scan()` returns `is_suspicious == true` (both initial prompt scan and tool output scan)
- `CapabilityViolation` emitted when a tool call fails permission validation
- `UnauthorizedToolAccess` emitted when an agent requests a tool not in its allowed set

---

## Subtasks

### 1. Emit `PromptInjectionAttempt` on initial prompt scan

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** Inside `execute_task()`, after `self.injection_scanner.scan(&task.original_prompt)` is called and `prompt_scan.is_suspicious` is `true`. The existing code already audits this — add the event emission right after the audit call.

**Code to add:**

```rust
if prompt_scan.is_suspicious {
    let threat_level = prompt_scan.max_threat
        .as_ref()
        .map(|t| format!("{:?}", t))
        .unwrap_or_else(|| "unknown".to_string());

    let severity = match prompt_scan.max_threat {
        Some(ThreatLevel::High) => EventSeverity::Critical,
        _ => EventSeverity::Warning,
    };

    self.emit_event(
        EventType::PromptInjectionAttempt,
        EventSource::SecurityEngine,
        severity,
        serde_json::json!({
            "task_id": task.id.to_string(),
            "agent_id": task.agent_id.to_string(),
            "source": "user_prompt",
            "threat_level": threat_level,
            "pattern_count": prompt_scan.matches.len(),
            "patterns": prompt_scan.matches.iter()
                .map(|m| &m.pattern_name)
                .collect::<Vec<_>>(),
        }),
        0,
    ).await;
}
```

### 2. Emit `PromptInjectionAttempt` on tool output scan

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** After tool execution, the code scans tool output for injection. Find where `injection_scanner.scan(&tool_output)` is called in the tool result handling path. Add the same pattern as subtask 1, but with `"source": "tool_output"` and include the tool name:

```rust
serde_json::json!({
    "task_id": task.id.to_string(),
    "agent_id": task.agent_id.to_string(),
    "source": "tool_output",
    "tool_name": tool_name,
    "threat_level": threat_level,
    "pattern_count": output_scan.matches.len(),
    "patterns": output_scan.matches.iter()
        .map(|m| &m.pattern_name)
        .collect::<Vec<_>>(),
})
```

### 3. Emit `CapabilityViolation` on permission check failure

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** In the tool execution path, after the capability/permission check fails. Look for where `PermissionSet::check()` returns a denial or where the capability token validation rejects an intent. The code likely returns an error or skips the tool call.

**Code to add at the rejection point:**

```rust
self.emit_event(
    EventType::CapabilityViolation,
    EventSource::SecurityEngine,
    EventSeverity::Critical,
    serde_json::json!({
        "task_id": task.id.to_string(),
        "agent_id": task.agent_id.to_string(),
        "tool_name": tool_name,
        "required_permission": required_permission,
        "violation_reason": "permission_denied",
        "action_taken": "blocked",
    }),
    0,
).await;
```

### 4. Emit `UnauthorizedToolAccess` when tool not in allowed set

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** Before tool execution, the code checks whether the requested tool exists in the agent's allowed tool set. If the tool is not found or not permitted, emit:

```rust
self.emit_event(
    EventType::UnauthorizedToolAccess,
    EventSource::SecurityEngine,
    EventSeverity::Critical,
    serde_json::json!({
        "task_id": task.id.to_string(),
        "agent_id": task.agent_id.to_string(),
        "requested_tool": requested_tool_name,
        "agent_allowed_tools": agent_tools.iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>(),
        "action_taken": "blocked",
    }),
    0,
).await;
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Add 4 `emit_event` calls at injection scan and permission check points |
| `crates/agentos-types/src/event.rs` | No changes — types already defined |

---

## Dependencies

None — `emit_event()` infrastructure exists. Can be done in parallel with Phase 01.

---

## Test Plan

1. **Injection detection test:** Submit a task with a known injection pattern (e.g., "ignore previous instructions"), verify `PromptInjectionAttempt` event is emitted with `source: "user_prompt"` and correct threat level.

2. **Capability violation test:** Have a mock agent attempt a tool call without the required permission, verify `CapabilityViolation` event is emitted with `action_taken: "blocked"`.

3. **Unauthorized tool test:** Have a mock agent request a tool not in its allowed set, verify `UnauthorizedToolAccess` event is emitted.

4. **Severity check:** Verify `PromptInjectionAttempt` with `High` threat level has `EventSeverity::Critical`.

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel

grep -n "PromptInjectionAttempt" crates/agentos-kernel/src/task_executor.rs
grep -n "CapabilityViolation" crates/agentos-kernel/src/task_executor.rs
grep -n "UnauthorizedToolAccess" crates/agentos-kernel/src/task_executor.rs
```

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[03-security-trigger-prompts]] — Phase 03 builds custom prompts for these events
- [[agentos-event-trigger-system]] — Original spec §3 (SecurityEvents category)
