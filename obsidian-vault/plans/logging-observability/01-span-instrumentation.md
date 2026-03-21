---
title: Phase 1 — Span Instrumentation
tags:
  - observability
  - logging
  - kernel
  - phase-1
  - next-steps
date: 2026-03-21
status: planned
effort: 1d
priority: high
---

# Phase 1 — Span Instrumentation

> Add `#[instrument]` to all critical kernel hot paths so every log line is stamped with `task_id`, `agent_id`, and `tool_id`, making call chains traceable end-to-end.

---

## Why This Phase

Without tracing spans, every `tracing::warn!` or `tracing::error!` is an isolated line with no context. You cannot tell which task caused a scheduler warning, which agent triggered a tool error, or which tool call chain led to a failure. `#[instrument]` solves this with zero manual log field repetition — the span fields propagate to all child events automatically.

---

## Current → Target State

**Current:** No `#[instrument]` attributes anywhere in the codebase. Log lines look like:
```
2026-03-21T10:00:00Z  WARN agentos_kernel::task_executor: Requeue failed
```
(no task context — impossible to debug in production)

**Target:** Key functions carry spans. Log lines look like:
```
2026-03-21T10:00:00Z  WARN task_executor{task_id=task-abc agent_id=agent-xyz}: Requeue failed
```

---

## Detailed Subtasks

### 1. Add `tracing` as dev-dependency if needed

Check `crates/agentos-kernel/Cargo.toml` — `tracing` should already be a workspace dependency.

File: `crates/agentos-kernel/Cargo.toml`
Verify line: `tracing.workspace = true` (already present; no change needed)

---

### 2. Instrument `run_loop.rs` dispatch entry point

File: `crates/agentos-kernel/src/run_loop.rs`

Find the main dispatch/handle function that processes `KernelCommand` variants. Add `#[instrument]` with `skip_all` (to avoid printing the full `Arc<Kernel>`) and relevant fields:

```rust
use tracing::instrument;

// On the function that receives a KernelCommand and dispatches it:
#[instrument(skip_all, fields(command = %command_name))]
async fn handle_command(kernel: Arc<Kernel>, command: KernelCommand) -> Result<(), AgentOSError> {
    // ...
}
```

Also add a restart/recovery log line in the subsystem restart loop (currently silent):
```rust
tracing::warn!(subsystem = %name, restart_count = count, "Subsystem restarting after failure");
```

---

### 3. Instrument `task_executor.rs` execute function

File: `crates/agentos-kernel/src/task_executor.rs`

Find the main async function that executes a task (receives `AgentTask`). Add:

```rust
#[instrument(skip_all, fields(
    task_id = %task.id,
    agent_id = %task.agent_id,
    task_type = ?task.task_type,
))]
pub async fn execute_task(kernel: Arc<Kernel>, task: AgentTask) -> Result<TaskResult, AgentOSError> {
    // ...
}
```

For the inference loop inside (if any), add a child span at the iteration level:
```rust
let _span = tracing::debug_span!("inference_iteration", iteration = i).entered();
```

---

### 4. Instrument `tool_call.rs`

File: `crates/agentos-kernel/src/tool_call.rs`

Find the function that validates capability and dispatches a tool. Add:

```rust
#[instrument(skip_all, fields(
    tool_id = %tool_id,
    tool_name = %tool_name,
    task_id = %task_id,
))]
pub async fn invoke_tool(
    kernel: Arc<Kernel>,
    tool_id: &ToolID,
    tool_name: &str,
    task_id: &TaskID,
    input: serde_json::Value,
) -> Result<ToolOutput, AgentOSError> {
    // ...
}
```

---

### 5. Instrument `scheduler.rs` key operations

File: `crates/agentos-kernel/src/scheduler.rs`

Add `#[instrument]` to `schedule()`, `requeue()`, and `complete()` (the three most-called methods):

```rust
#[instrument(skip(self), fields(task_id = %task_id))]
pub async fn schedule(&self, task_id: &TaskID, ...) -> Result<(), AgentOSError>

#[instrument(skip(self), fields(task_id = %task_id))]
pub async fn requeue(&self, task_id: &TaskID) -> Result<(), AgentOSError>

#[instrument(skip(self), fields(task_id = %task_id))]
pub async fn complete(&self, task_id: &TaskID, result: TaskResult) -> Result<(), AgentOSError>
```

---

### 6. Instrument `intent_validator.rs`

File: `crates/agentos-kernel/src/intent_validator.rs`

```rust
#[instrument(skip_all, fields(intent_type = ?intent.kind))]
pub async fn validate(&self, intent: &IntentMessage) -> Result<ValidationResult, AgentOSError>
```

---

### 7. Add startup log lines for boot visibility

File: `crates/agentos-kernel/src/kernel.rs`

After each major subsystem initialization, add a structured info line:
```rust
tracing::info!(subsystem = "scheduler", "Subsystem started");
tracing::info!(subsystem = "event_dispatch", "Subsystem started");
tracing::info!(subsystem = "health_monitor", "Subsystem started");
```

Also add a single kernel boot banner:
```rust
tracing::info!(
    version = env!("CARGO_PKG_VERSION"),
    log_level = %config.logging.log_level,
    "AgentOS kernel starting"
);
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/run_loop.rs` | `#[instrument]` on dispatch fn; warn on restart |
| `crates/agentos-kernel/src/task_executor.rs` | `#[instrument]` on execute_task |
| `crates/agentos-kernel/src/tool_call.rs` | `#[instrument]` on invoke_tool |
| `crates/agentos-kernel/src/scheduler.rs` | `#[instrument]` on schedule/requeue/complete |
| `crates/agentos-kernel/src/intent_validator.rs` | `#[instrument]` on validate |
| `crates/agentos-kernel/src/kernel.rs` | boot info lines per subsystem |

---

## Dependencies

None — this is the first phase and unblocks all others.

---

## Test Plan

1. `cargo build -p agentos-kernel` — must compile clean
2. Run kernel with `RUST_LOG=debug` and submit a task — log output should show span hierarchy with `task_id` and `agent_id`
3. Confirm `cargo test -p agentos-kernel` still passes — no behavioural changes, only logging additions
4. Check that `cargo clippy -p agentos-kernel -- -D warnings` passes (unused import warnings are the most common issue with `#[instrument]`)

---

## Verification

```bash
# Build check
cargo build -p agentos-kernel

# Clippy
cargo clippy -p agentos-kernel -- -D warnings

# Tests
cargo test -p agentos-kernel

# Runtime check (requires kernel running)
RUST_LOG=debug agentctl task run --agent mock --goal "test" 2>&1 | grep "task_id="
# Expected: lines containing task_id=... agent_id=...
```

---

## Related

- [[Logging Observability Plan]] — master plan and design decisions
- [[Logging Observability Data Flow]] — how spans propagate
- [[02-tools-logging]] — Phase 2 (depends on this phase)
- [[03-silent-failure-elimination]] — Phase 3 (depends on this phase)
