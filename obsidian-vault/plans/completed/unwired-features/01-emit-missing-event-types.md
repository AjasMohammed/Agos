---
title: "Phase 01 — Emit Missing Event Types"
tags:
  - kernel
  - event-system
  - next-steps
  - v3
date: 2026-03-17
status: complete
effort: 3d
priority: critical
---

# Phase 01 — Emit Missing Event Types

> Wire `emit_event()` calls for all 27 non-external EventType variants that are defined but never emitted from any code path, grouped by subsystem with exact file paths and code insertion points.

---

## Why This Phase

AgentOS defines 47 EventType variants and has a fully functional event bus (subscription registry, throttling, loop detection, dispatch loop, CLI commands, audit logging). However, 31 of those types are never emitted. Four of the 31 are ExternalEvents that require new subsystems (deferred). The remaining 27 can be wired into existing code paths today. Without these emissions, agents cannot react to deadlocks, security breaches, memory failures, hardware changes, or tool sandbox violations -- defeating the core event-driven architecture.

---

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| EventTypes emitted | 27 of 47 | 43 of 47 (4 external deferred) |
| TaskRetrying | Never emitted | Emitted on task retry in `task_executor.rs` |
| TaskPreempted | Never emitted | Emitted from `resource_arbiter.rs` preemption |
| TaskDeadlockDetected | Never emitted | Emitted from `resource_arbiter.rs` deadlock detection |
| SecretsAccessAttempt | Never emitted | Emitted from `commands/secret.rs` |
| SandboxEscapeAttempt | Never emitted | Emitted from `task_executor.rs` sandbox path |
| AgentImpersonationAttempt | Never emitted | Emitted from `run_loop.rs` agent mismatch |
| UnverifiedToolInstalled | Never emitted | Emitted from `tool_registry.rs` |
| ContextWindowExhausted | Never emitted | Emitted from `context.rs` at 100% budget |
| MemorySearchFailed | Never emitted | Emitted from `retrieval_gate.rs` |
| WorkingMemoryEviction | Never emitted | Emitted from `context.rs` eviction |
| ProcessCrashed | Never emitted | Emitted from `run_loop.rs` panic handler |
| NetworkInterfaceDown | Never emitted | Emitted from `health_monitor.rs` |
| ContainerResourceQuotaExceeded | Never emitted | Emitted from `health_monitor.rs` |
| KernelSubsystemError | Never emitted | Emitted from `run_loop.rs` restart budget exceeded |
| GPUAvailable | Never emitted | Emitted from `health_monitor.rs` |
| SensorReadingThresholdExceeded | Never emitted | Emitted from `health_monitor.rs` |
| DeviceConnected | Never emitted | Emitted from `commands/hal.rs` |
| DeviceDisconnected | Never emitted | Emitted from `commands/hal.rs` |
| HardwareAccessGranted | Never emitted | Emitted from `commands/hal.rs` |
| ToolSandboxViolation | Never emitted | Emitted from `task_executor.rs` |
| ToolResourceQuotaExceeded | Never emitted | Emitted from `task_executor.rs` |
| ToolChecksumMismatch | Never emitted | Emitted from `tool_registry.rs` |
| ToolRegistryUpdated | Never emitted | Emitted from `tool_registry.rs` |
| DelegationResponseReceived | Never emitted | Emitted from `task_completion.rs` |
| AgentUnreachable | Never emitted | Emitted from `agent_message_bus.rs` |
| ScheduledTaskCompleted | Never emitted | Emitted from `run_loop.rs` agentd loop |
| AuditLogTamperAttempt | Never emitted | Deferred (no tamper detection infra) |

---

## What to Do

### Group A: Task Lifecycle (3 events, 2 files)

#### A1. TaskRetrying -- `crates/agentos-kernel/src/task_executor.rs`

The task executor currently has no retry logic -- tasks either succeed or fail. To emit `TaskRetrying`, add a retry counter to the execution loop. When a task fails with a retryable error (LLM transient error, timeout on tool), retry up to `max_retries` (configurable, default 2) and emit before each retry attempt.

**Insertion point:** In `execute_task_sync()`, around the LLM inference call (currently line ~560). When the `llm.infer()` call returns a transient error:

```rust
// After catching a retryable LLM error:
if retry_count < max_retries {
    retry_count += 1;
    self.emit_event_with_trace(
        EventType::TaskRetrying,
        EventSource::TaskScheduler,
        EventSeverity::Warning,
        serde_json::json!({
            "task_id": task.id.to_string(),
            "agent_id": task.agent_id.to_string(),
            "retry_attempt": retry_count,
            "max_retries": max_retries,
            "reason": error_message,
        }),
        chain_depth,
        Some(iteration_trace_id),
    )
    .await;
    continue; // retry the inference loop
}
```

If adding full retry logic is too invasive for this phase, emit `TaskRetrying` as an informational event in `complete_task_failure()` when the error is classified as retryable (LLM transient), even if no actual retry happens yet. This ensures the event type is wired and subscribers can react.

#### A2. TaskPreempted -- `crates/agentos-kernel/src/resource_arbiter.rs`

The `acquire_with_priority()` method already has a preemption path at line ~278-297 where a higher-priority requester forcibly releases a lower-priority holder's lock. This is where `TaskPreempted` should be emitted.

**Problem:** `ResourceArbiter` does not hold an `event_sender`. Two options:
1. Add an `Option<mpsc::Sender<(EventType, serde_json::Value)>>` notification channel (like `ToolRegistry` does with `ToolLifecycleEvent`)
2. Return a `PreemptionOccurred { holder: AgentID, resource: String }` variant from `acquire_with_priority()` and let the kernel emit

**Recommended approach:** Option 1 -- add a notification channel.

```rust
// In ResourceArbiter struct, add:
pub(crate) preemption_sender: Option<tokio::sync::mpsc::Sender<PreemptionNotification>>,

#[derive(Debug, Clone)]
pub struct PreemptionNotification {
    pub preempted_agent: AgentID,
    pub preempting_agent: AgentID,
    pub resource_id: String,
}
```

In the preemption path (line ~280 of `resource_arbiter.rs`), after `state.release_all()`:

```rust
if let Some(ref sender) = self.preemption_sender {
    let _ = sender.try_send(PreemptionNotification {
        preempted_agent: holder,
        preempting_agent: agent_id,
        resource_id: resource_id.to_string(),
    });
}
```

In the kernel, add a listener loop (similar to `tool_lifecycle_listener` in `run_loop.rs`) that converts `PreemptionNotification` into `EventType::TaskPreempted` via `emit_event()`.

#### A3. TaskDeadlockDetected -- `crates/agentos-kernel/src/resource_arbiter.rs`

Same file as A2. The deadlock detection path is at line ~276 where `would_deadlock()` returns true. Add a notification in the same channel:

```rust
// Add to the notification enum or use a separate one:
pub struct DeadlockNotification {
    pub blocked_agent: AgentID,
    pub holder_agent: AgentID,
    pub resource_id: String,
}
```

Emit in the deadlock branch (line ~299 of `resource_arbiter.rs`), just before returning `Err`:

```rust
if let Some(ref sender) = self.deadlock_sender {
    let _ = sender.try_send(DeadlockNotification {
        blocked_agent: agent_id,
        holder_agent: holder,
        resource_id: resource_id.to_string(),
    });
}
```

The kernel listener converts this to `EventType::TaskDeadlockDetected`.

**Files changed:**
| File | Change |
|------|--------|
| `crates/agentos-kernel/src/resource_arbiter.rs` | Add `PreemptionNotification`, `DeadlockNotification`, sender fields, emit in preemption + deadlock paths |
| `crates/agentos-kernel/src/task_executor.rs` | Add `TaskRetrying` emission on retryable failures |
| `crates/agentos-kernel/src/run_loop.rs` | Add listener loop for arbiter notifications, convert to events |
| `crates/agentos-kernel/src/kernel.rs` | Add receiver field for arbiter notifications |

---

### Group B: Security Events (4 events, 4 files)

#### B1. SecretsAccessAttempt -- `crates/agentos-kernel/src/commands/secret.rs`

Every vault access command (`cmd_set_secret`, `cmd_get_secret`, `cmd_delete_secret`, `cmd_list_secrets`) should emit `SecretsAccessAttempt` with details about what was accessed and by whom.

```rust
self.emit_event(
    EventType::SecretsAccessAttempt,
    EventSource::SecretsVault,
    EventSeverity::Info, // Warning if access denied
    serde_json::json!({
        "action": "get", // or "set", "delete", "list"
        "key": key_name,
        "allowed": true, // false if permission denied
    }),
    0,
)
.await;
```

#### B2. SandboxEscapeAttempt -- `crates/agentos-kernel/src/task_executor.rs`

The task executor runs tools through `SandboxExecutor`. When the sandbox reports a violation (seccomp filter triggered, forbidden syscall), emit `SandboxEscapeAttempt`. Look for the sandbox execution path in `execute_task_sync()` where tool results are handled -- if the sandbox returns an error indicating a policy violation:

```rust
// In the tool execution error handling path:
if is_sandbox_violation(&error) {
    self.emit_event_with_trace(
        EventType::SandboxEscapeAttempt,
        EventSource::SecurityEngine,
        EventSeverity::Critical,
        serde_json::json!({
            "task_id": task.id.to_string(),
            "agent_id": task.agent_id.to_string(),
            "tool_name": tool_name,
            "violation": error.to_string(),
        }),
        chain_depth,
        Some(iteration_trace_id),
    )
    .await;
}
```

Also emit `ToolSandboxViolation` (Group D) from the same path.

#### B3. AgentImpersonationAttempt -- `crates/agentos-kernel/src/run_loop.rs`

In the command dispatch loop, when a bus message arrives with an `agent_id` that does not match any registered agent, or when an agent attempts to act as another agent:

```rust
// In the command dispatch section, after extracting agent_id from the message:
if !self.agent_registry.read().await.get_by_id(&claimed_agent_id).is_some() {
    self.emit_event(
        EventType::AgentImpersonationAttempt,
        EventSource::SecurityEngine,
        EventSeverity::Critical,
        serde_json::json!({
            "claimed_agent_id": claimed_agent_id.to_string(),
            "source": "bus_message",
        }),
        0,
    )
    .await;
}
```

#### B4. UnverifiedToolInstalled -- `crates/agentos-kernel/src/tool_registry.rs`

When `verify_manifest_with_crl()` in `register()` encounters a tool with `TrustTier::Community` or `TrustTier::Verified` that has a missing or invalid signature but is still registered (e.g., in a permissive mode), emit this event. Currently, invalid signatures cause `register()` to return `Err`, so the event should be emitted just before the error return, or in the lifecycle listener when a tool is installed with a non-Core trust tier:

In `event_dispatch.rs`, in the `tool_lifecycle_listener` handler for `ToolLifecycleEvent::Installed`, check the trust tier:

```rust
ToolLifecycleEvent::Installed { tool_id, tool_name, trust_tier, description } => {
    // Existing ToolInstalled emission...

    if trust_tier != "Core" {
        crate::event_dispatch::emit_signed_event(
            &capability_engine,
            &audit,
            &event_sender,
            EventType::UnverifiedToolInstalled,
            EventSource::ToolRunner,
            EventSeverity::Warning,
            serde_json::json!({
                "tool_id": tool_id.to_string(),
                "tool_name": tool_name,
                "trust_tier": trust_tier,
            }),
            0, TraceID::new(), None, None,
        );
    }
}
```

**Note:** `AuditLogTamperAttempt` is deferred because no tamper detection mechanism exists yet. The audit log uses SQLite but has no integrity verification (Merkle tree, checksums). This should be a separate future work item.

**Files changed:**
| File | Change |
|------|--------|
| `crates/agentos-kernel/src/commands/secret.rs` | Add `SecretsAccessAttempt` emission on every vault operation |
| `crates/agentos-kernel/src/task_executor.rs` | Add `SandboxEscapeAttempt` emission on sandbox violations |
| `crates/agentos-kernel/src/run_loop.rs` | Add `AgentImpersonationAttempt` emission on agent ID mismatch |
| `crates/agentos-kernel/src/event_dispatch.rs` | Add `UnverifiedToolInstalled` emission in tool lifecycle listener |

---

### Group C: Memory Events (3 events, 2 files)

#### C1. ContextWindowExhausted -- `crates/agentos-kernel/src/context.rs`

The `push_entry()` method in `ContextManager` already checks token budget at 80% and 95%. Add a check at 100%:

```rust
// In push_entry(), after the existing 80%/95% checks:
if self.token_budget > 0 {
    let utilization = estimated_tokens as f32 / self.token_budget as f32;
    if utilization >= 1.0 {
        // Context is fully exhausted
        // Return a flag or error that the caller can use to emit ContextWindowExhausted
    }
}
```

Since `ContextManager` does not hold an event sender, the emission should happen in `task_executor.rs` after the `push_entry()` call. The `drain_checkpoint_flag()` pattern already exists for budget enforcement. Add a similar `is_exhausted()` check:

In `task_executor.rs`, after pushing a context entry, check if the window is at 100%:

```rust
// After pushing context entry, if budget is exhausted:
if self.context_manager.is_budget_exhausted(&task.id).await {
    self.emit_event_with_trace(
        EventType::ContextWindowExhausted,
        EventSource::ContextManager,
        EventSeverity::Critical,
        serde_json::json!({
            "task_id": task.id.to_string(),
            "agent_id": task.agent_id.to_string(),
            "action": "context_window_full",
        }),
        chain_depth,
        Some(iteration_trace_id),
    )
    .await;
}
```

Add `is_budget_exhausted()` to `ContextManager`:

```rust
pub async fn is_budget_exhausted(&self, task_id: &TaskID) -> bool {
    if self.token_budget == 0 { return false; }
    let windows = self.windows.read().await;
    if let Some(window) = windows.get(task_id) {
        let estimated = window.entries.iter().map(|e| e.content.len() / 4).sum::<usize>();
        estimated >= self.token_budget
    } else {
        false
    }
}
```

#### C2. MemorySearchFailed -- `crates/agentos-kernel/src/retrieval_gate.rs`

In `RetrievalExecutor`, when a memory search (semantic, episodic, or procedural) fails with an error, emit this event. Look for the `retrieve()` or `search()` method and add emission in the error branch:

```rust
Err(e) => {
    // Emit MemorySearchFailed via the event_sender
    crate::event_dispatch::emit_signed_event(
        &self.capability_engine,
        &self.audit,
        &self.event_sender,
        EventType::MemorySearchFailed,
        EventSource::MemoryArbiter,
        EventSeverity::Warning,
        serde_json::json!({
            "agent_id": agent_id.to_string(),
            "search_type": "semantic", // or "episodic", "procedural"
            "error": e.to_string(),
            "query_preview": query.chars().take(100).collect::<String>(),
        }),
        0, TraceID::new(),
        Some(agent_id), task_id,
    );
    Err(e)
}
```

If `RetrievalExecutor` does not hold event infrastructure, add `event_sender`, `capability_engine`, and `audit` fields, or use a notification channel pattern like `ToolRegistry`.

#### C3. WorkingMemoryEviction -- `crates/agentos-kernel/src/context.rs` or `crates/agentos-kernel/src/task_executor.rs`

When `SemanticEviction` runs (the overflow strategy), entries are removed from the context window. Emit `WorkingMemoryEviction` each time entries are evicted.

Since `ContextManager` has no event sender, emit from `task_executor.rs` by checking the window size before and after pushing:

```rust
let pre_count = self.context_manager.entry_count(&task.id).await;
self.context_manager.push_entry(&task.id, entry).await.ok();
let post_count = self.context_manager.entry_count(&task.id).await;
if post_count < pre_count {
    // Eviction occurred
    self.emit_event_with_trace(
        EventType::WorkingMemoryEviction,
        EventSource::ContextManager,
        EventSeverity::Info,
        serde_json::json!({
            "task_id": task.id.to_string(),
            "agent_id": task.agent_id.to_string(),
            "entries_evicted": pre_count - post_count,
        }),
        chain_depth,
        Some(iteration_trace_id),
    )
    .await;
}
```

Alternatively, modify `push_entry()` to return the number of evicted entries.

**Files changed:**
| File | Change |
|------|--------|
| `crates/agentos-kernel/src/context.rs` | Add `is_budget_exhausted()` method; optionally return eviction count from `push_entry()` |
| `crates/agentos-kernel/src/task_executor.rs` | Add `ContextWindowExhausted` and `WorkingMemoryEviction` emissions |
| `crates/agentos-kernel/src/retrieval_gate.rs` | Add `MemorySearchFailed` emission on search errors |

---

### Group D: System Health & Hardware (9 events, 2 files)

#### D1. ProcessCrashed -- `crates/agentos-kernel/src/run_loop.rs`

In the supervisor loop, lines ~537-554, when a `JoinError` is a panic:

```rust
Some(Err(join_error)) => {
    // ... existing code to identify crashed task ...
    if join_error.is_panic() {
        self.emit_event(
            EventType::ProcessCrashed,
            EventSource::InferenceKernel,
            EventSeverity::Critical,
            serde_json::json!({
                "task_kind": task_name,
                "panic": true,
                "error": format!("{:?}", join_error),
            }),
            0,
        )
        .await;
    }
}
```

**Insert after** the existing `tracing::error!` call at line ~551 and the existing `audit_log` call.

#### D2. KernelSubsystemError -- `crates/agentos-kernel/src/run_loop.rs`

When a task exceeds its restart budget (line ~533), emit before breaking:

```rust
if !self.check_restart_budget(&mut restart_counts, &kind.to_string()) {
    self.emit_event(
        EventType::KernelSubsystemError,
        EventSource::InferenceKernel,
        EventSeverity::Critical,
        serde_json::json!({
            "task_kind": kind.to_string(),
            "reason": "restart_budget_exceeded",
            "max_restarts": MAX_RESTARTS,
        }),
        0,
    )
    .await;
    tracing::error!(task = %kind, "Task exceeded restart budget, kernel degraded");
    break;
}
```

#### D3-D4. NetworkInterfaceDown, ContainerResourceQuotaExceeded -- `crates/agentos-kernel/src/health_monitor.rs`

Add new checks in `check_system_health()`:

```rust
// Network interface check (after system snapshot section):
if let Some(interfaces) = snapshot.get("network_interfaces").and_then(|n| n.as_array()) {
    for iface in interfaces {
        let name = iface.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
        let is_up = iface.get("is_up").and_then(|v| v.as_bool()).unwrap_or(true);
        if !is_up && should_emit(last_emitted, EventType::NetworkInterfaceDown) {
            kernel.emit_event(
                EventType::NetworkInterfaceDown,
                EventSource::HardwareAbstractionLayer,
                EventSeverity::Warning,
                serde_json::json!({
                    "interface": name,
                }),
                0,
            ).await;
        }
    }
}
```

For `ContainerResourceQuotaExceeded`, check cgroup limits if available in the HAL system snapshot:

```rust
if let Some(cgroup) = snapshot.get("cgroup") {
    let mem_limit = cgroup.get("memory_limit_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
    let mem_usage = cgroup.get("memory_usage_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
    if mem_limit > 0 {
        let usage_pct = (mem_usage as f32 / mem_limit as f32) * 100.0;
        if usage_pct > 95.0 && should_emit(last_emitted, EventType::ContainerResourceQuotaExceeded) {
            kernel.emit_event(
                EventType::ContainerResourceQuotaExceeded,
                EventSource::HardwareAbstractionLayer,
                EventSeverity::Critical,
                serde_json::json!({
                    "resource": "memory",
                    "usage_percent": usage_pct,
                    "limit_bytes": mem_limit,
                    "usage_bytes": mem_usage,
                }),
                0,
            ).await;
        }
    }
}
```

#### D5. GPUAvailable -- `crates/agentos-kernel/src/health_monitor.rs`

In the GPU VRAM check section, when GPUs are detected and available, emit once:

```rust
// Track previous GPU availability to detect new GPUs
if !devices.is_empty() && should_emit(last_emitted, EventType::GPUAvailable) {
    for device in devices {
        let name = device.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
        kernel.emit_event(
            EventType::GPUAvailable,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Info,
            serde_json::json!({
                "gpu_name": name,
                "vram_total_mb": device.get("vram_total_mb").and_then(|v| v.as_u64()).unwrap_or(0),
            }),
            0,
        ).await;
    }
}
```

#### D6. SensorReadingThresholdExceeded -- `crates/agentos-kernel/src/health_monitor.rs`

Add a new section for sensor queries:

```rust
// Sensor check
if let Ok(sensor_json) = kernel.hal.query("sensor", serde_json::json!({"action": "list"}), permissions).await {
    if let Some(readings) = sensor_json.get("readings").and_then(|r| r.as_array()) {
        for reading in readings {
            let name = reading.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
            let value = reading.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let threshold = reading.get("threshold").and_then(|v| v.as_f64());
            if let Some(thresh) = threshold {
                if value > thresh && should_emit(last_emitted, EventType::SensorReadingThresholdExceeded) {
                    kernel.emit_event(
                        EventType::SensorReadingThresholdExceeded,
                        EventSource::HardwareAbstractionLayer,
                        EventSeverity::Warning,
                        serde_json::json!({
                            "sensor_name": name,
                            "value": value,
                            "threshold": thresh,
                        }),
                        0,
                    ).await;
                }
            }
        }
    }
}
```

#### D7-D9. DeviceConnected, DeviceDisconnected, HardwareAccessGranted -- `crates/agentos-kernel/src/commands/hal.rs`

In `cmd_hal_register_device()`, after successful registration:

```rust
self.emit_event(
    EventType::DeviceConnected,
    EventSource::HardwareAbstractionLayer,
    EventSeverity::Info,
    serde_json::json!({
        "device_id": device_id,
        "device_type": device_type,
        "device_name": device_name,
    }),
    0,
).await;
```

In `cmd_hal_approve_device()`, after approval:

```rust
self.emit_event(
    EventType::HardwareAccessGranted,
    EventSource::HardwareAbstractionLayer,
    EventSeverity::Info,
    serde_json::json!({
        "device_id": device_id,
        "approved_by": "operator", // or agent_id if available
    }),
    0,
).await;
```

In `cmd_hal_revoke_device()` (repurpose for disconnect semantics), or add a `cmd_hal_remove_device()` if it does not exist:

```rust
self.emit_event(
    EventType::DeviceDisconnected,
    EventSource::HardwareAbstractionLayer,
    EventSeverity::Info,
    serde_json::json!({
        "device_id": device_id,
        "reason": "revoked",
    }),
    0,
).await;
```

**Files changed:**
| File | Change |
|------|--------|
| `crates/agentos-kernel/src/run_loop.rs` | Add `ProcessCrashed` and `KernelSubsystemError` emissions |
| `crates/agentos-kernel/src/health_monitor.rs` | Add `NetworkInterfaceDown`, `ContainerResourceQuotaExceeded`, `GPUAvailable`, `SensorReadingThresholdExceeded` |
| `crates/agentos-kernel/src/commands/hal.rs` | Add `DeviceConnected`, `DeviceDisconnected`, `HardwareAccessGranted` |

---

### Group E: Tool Events (4 events, 2 files)

#### E1. ToolSandboxViolation -- `crates/agentos-kernel/src/task_executor.rs`

In the tool execution error handling path, when the error indicates a sandbox policy violation:

```rust
// After tool execution fails:
if error_msg.contains("sandbox") || error_msg.contains("seccomp") || error_msg.contains("syscall denied") {
    self.emit_event_with_trace(
        EventType::ToolSandboxViolation,
        EventSource::ToolRunner,
        EventSeverity::Critical,
        serde_json::json!({
            "task_id": task.id.to_string(),
            "agent_id": task.agent_id.to_string(),
            "tool_name": tool_name,
            "violation": error_msg,
        }),
        chain_depth,
        Some(iteration_trace_id),
    )
    .await;
}
```

#### E2. ToolResourceQuotaExceeded -- `crates/agentos-kernel/src/task_executor.rs`

When a tool exceeds its `max_memory_mb` or `max_cpu_ms` limits:

```rust
if error_msg.contains("resource") || error_msg.contains("quota") || error_msg.contains("memory limit") || error_msg.contains("cpu limit") {
    self.emit_event_with_trace(
        EventType::ToolResourceQuotaExceeded,
        EventSource::ToolRunner,
        EventSeverity::Warning,
        serde_json::json!({
            "task_id": task.id.to_string(),
            "tool_name": tool_name,
            "error": error_msg,
        }),
        chain_depth,
        Some(iteration_trace_id),
    )
    .await;
}
```

#### E3. ToolChecksumMismatch -- `crates/agentos-kernel/src/tool_registry.rs`

In `register()`, when checksum validation fails. Currently `verify_manifest_with_crl()` handles this. Add a notification variant:

```rust
// In ToolLifecycleEvent:
ChecksumMismatch {
    tool_name: String,
    expected: String,
    actual: String,
}
```

Emit before returning `Err` from `register()` when the checksum does not match:

```rust
if let Some(ref sender) = self.lifecycle_sender {
    let _ = sender.try_send(ToolLifecycleEvent::ChecksumMismatch {
        tool_name: name.clone(),
        expected: expected_checksum,
        actual: actual_checksum,
    });
}
```

In the kernel's tool lifecycle listener, convert to `EventType::ToolChecksumMismatch`.

#### E4. ToolRegistryUpdated -- `crates/agentos-kernel/src/tool_registry.rs` / `crates/agentos-kernel/src/event_dispatch.rs`

Emit after any successful `register()` or `remove()` call. Since both operations already send `ToolLifecycleEvent::Installed` / `ToolLifecycleEvent::Removed`, add `ToolRegistryUpdated` emission in the tool lifecycle listener alongside `ToolInstalled`/`ToolRemoved`:

```rust
// In the tool lifecycle listener, after emitting ToolInstalled or ToolRemoved:
crate::event_dispatch::emit_signed_event(
    &capability_engine,
    &audit,
    &event_sender,
    EventType::ToolRegistryUpdated,
    EventSource::ToolRunner,
    EventSeverity::Info,
    serde_json::json!({
        "action": "installed", // or "removed"
        "tool_name": tool_name,
    }),
    0, TraceID::new(), None, None,
);
```

**Files changed:**
| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Add `ToolSandboxViolation`, `ToolResourceQuotaExceeded` emissions |
| `crates/agentos-kernel/src/tool_registry.rs` | Add `ChecksumMismatch` lifecycle event variant |
| `crates/agentos-kernel/src/event_dispatch.rs` | Add `ToolChecksumMismatch`, `ToolRegistryUpdated`, `UnverifiedToolInstalled` in lifecycle listener |

---

### Group F: Agent Communication & Schedule (3 events, 3 files)

#### F1. DelegationResponseReceived -- `crates/agentos-kernel/src/task_completion.rs`

When a delegated child task completes (success or failure), the parent should receive a `DelegationResponseReceived` event. In `complete_task_success()`, after waking dependency waiters:

```rust
let waiters = self.scheduler.complete_dependency(task.id).await;
for waiter_id in waiters {
    self.emit_event_with_trace(
        EventType::DelegationResponseReceived,
        EventSource::TaskScheduler,
        EventSeverity::Info,
        serde_json::json!({
            "parent_task_id": waiter_id.to_string(),
            "child_task_id": task.id.to_string(),
            "child_agent_id": task.agent_id.to_string(),
            "outcome": "success",
        }),
        0,
        Some(task_trace_id),
    )
    .await;
    self.scheduler.requeue(&waiter_id).await.ok();
}
```

Similarly in `complete_task_failure()`.

#### F2. AgentUnreachable -- `crates/agentos-kernel/src/agent_message_bus.rs`

When `MessageDeliveryFailed` is emitted due to the target agent not being found (as opposed to channel full), also emit `AgentUnreachable`. In `send_direct()`, when the target agent is not registered:

```rust
// Add AgentUnreachable notification variant to CommNotification:
CommNotification {
    event_type: EventType::AgentUnreachable,
    payload: serde_json::json!({
        "unreachable_agent": to_name,
        "from_agent": from_name,
        "reason": "not_registered",
    }),
}
```

#### F3. ScheduledTaskCompleted -- `crates/agentos-kernel/src/run_loop.rs`

In the agentd scheduler loop, after a scheduled task finishes executing successfully. The `schedule_notification_listener` in `run_loop.rs` handles schedule notifications. Add emission in the task completion callback for scheduled tasks.

Alternatively, add a `ScheduledTaskCompleted` notification variant to `ScheduleNotification` and emit from `schedule_manager.rs`:

```rust
pub fn mark_completed(&self, job_id: &str) -> ScheduleNotification {
    ScheduleNotification {
        event_type: EventType::ScheduledTaskCompleted,
        payload: serde_json::json!({
            "job_id": job_id,
            "completed_at": chrono::Utc::now().to_rfc3339(),
        }),
    }
}
```

**Files changed:**
| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_completion.rs` | Add `DelegationResponseReceived` emission when child task completes |
| `crates/agentos-kernel/src/agent_message_bus.rs` | Add `AgentUnreachable` notification variant |
| `crates/agentos-kernel/src/schedule_manager.rs` | Add `ScheduledTaskCompleted` notification |
| `crates/agentos-kernel/src/run_loop.rs` | Convert new notifications to events in listener loops |

---

### Group G: External Events (4 events -- DEFERRED)

`WebhookReceived`, `ExternalFileChanged`, `ExternalAPIEvent`, `ExternalAlertReceived` require new subsystems:

1. **Webhook server:** An HTTP endpoint that receives external webhooks and converts them to events
2. **File watcher:** A filesystem notify listener (e.g., `notify` crate) that watches configured paths
3. **API bridge:** A polling/streaming adapter for external APIs
4. **Alert receiver:** A receiver for monitoring alerts (PagerDuty, Prometheus AlertManager, etc.)

These are **out of scope** for this phase. They should be planned as a separate "External Events Bridge" plan.

---

## Prerequisites

None -- this is the first phase. It can be worked on independently of all other phases.

---

## Test Plan

1. **Task lifecycle events:**
   - Add test in `task_executor.rs` tests: verify `TaskRetrying` is emitted when a retryable error occurs
   - Add test in `resource_arbiter.rs` tests: verify `PreemptionNotification` is sent on priority preemption
   - Add test in `resource_arbiter.rs` tests: verify `DeadlockNotification` is sent when `would_deadlock()` detects a cycle

2. **Security events:**
   - Add test in `commands/secret.rs`: verify `SecretsAccessAttempt` is emitted on vault access
   - Verify `SandboxEscapeAttempt` emission by mocking a sandbox violation error string

3. **Memory events:**
   - Add test in `context.rs`: verify `is_budget_exhausted()` returns true at 100% budget
   - Verify `WorkingMemoryEviction` emission by pushing entries past `max_entries`

4. **Health/HAL events:**
   - Existing `health_monitor.rs` test pattern: verify new event types are emitted by mocking HAL responses
   - Add test in `commands/hal.rs`: verify `DeviceConnected` / `HardwareAccessGranted` events

5. **Tool events:**
   - Add test in `tool_registry.rs`: verify `ChecksumMismatch` lifecycle event
   - Verify `ToolRegistryUpdated` emission in tool lifecycle listener test

6. **Communication events:**
   - Add test in `agent_message_bus.rs`: verify `AgentUnreachable` notification on failed delivery to missing agent

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- --nocapture
cargo test -p agentos-kernel -- event --nocapture
cargo test -p agentos-kernel -- health_monitor --nocapture
cargo test -p agentos-kernel -- resource_arbiter --nocapture
cargo test -p agentos-kernel -- tool_registry --nocapture
cargo test -p agentos-kernel -- agent_message_bus --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```

After implementation, verify no EventType variant is unused by grepping:
```bash
# Should find emit_event calls for all non-external EventType variants:
grep -r "EventType::" crates/agentos-kernel/src/ | grep -v "event_bus.rs" | grep -v "test" | sort -u
```

---

## Related

- [[Unwired Features Plan]] -- Parent plan
- [[Event Trigger Completion Plan]] -- Original event system plan
- [[22-Unwired Features]] -- Next-steps parent index
