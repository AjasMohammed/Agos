---
title: "Phase 4: Checkpoint Lifecycle"
tags:
  - kernel
  - durability
  - plan
date: 2026-04-03
status: complete
effort: 1d
priority: medium
---

# Phase 4: Checkpoint Lifecycle

> Add checkpoint pruning to the `TimeoutChecker` sweep cycle, a `--no-checkpoint` flag for ephemeral tasks, and `CheckpointWritten` / `CheckpointRestored` audit events for operational visibility.

---

## Why This Phase

Checkpoints accumulate on disk without bounds unless pruned. This phase adds:
1. Automatic pruning of checkpoints older than 72 hours (same pattern as `sweep_expired_snapshots`).
2. A `--no-checkpoint` flag for tasks that should not be checkpointed (e.g., quick one-shot queries).
3. Audit events so operators can track checkpoint activity in the audit log.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Checkpoint pruning | None; checkpoints grow unbounded | `TimeoutChecker` prunes checkpoints >72h every 10min |
| Ephemeral tasks | All tasks checkpointed (when enabled) | `--no-checkpoint` flag on `agentos task run` skips checkpointing |
| Audit trail | No checkpoint events | `CheckpointWritten` and `CheckpointRestored` audit event types |
| `AgentTask` | No checkpoint-skip flag | New `skip_checkpoint: bool` field |

## What to Do

### 1. Add checkpoint pruning to `TimeoutChecker`

Open `crates/agentos-kernel/src/timeout_checker.rs`. In the existing sweep loop (which already handles `sweep_expired_escalations`, `sweep_expired_snapshots`, etc.), add:

```rust
// Prune checkpoints older than 72 hours.
if let Some(ref cp_store) = self.checkpoint_store {
    match cp_store.prune_older_than(chrono::Duration::hours(72)).await {
        Ok(0) => {} // nothing to prune
        Ok(n) => {
            tracing::info!(pruned = n, "TimeoutChecker: pruned {} expired checkpoints", n);
        }
        Err(e) => {
            tracing::warn!(error = %e, "TimeoutChecker: checkpoint pruning failed");
        }
    }
}
```

### 2. Add `skip_checkpoint` field to `AgentTask`

Open `crates/agentos-types/src/task.rs`. Add to `AgentTask`:

```rust
/// When true, the task executor will not write checkpoints for this task.
/// Set by `--no-checkpoint` CLI flag for ephemeral one-shot tasks.
#[serde(default)]
pub skip_checkpoint: bool,
```

### 3. Add `--no-checkpoint` CLI flag

Open `crates/agentos-cli/src/commands/mod.rs`. Add to the `RunTask` subcommand:

```rust
/// Skip checkpointing for this task (ephemeral execution).
#[arg(long)]
no_checkpoint: bool,
```

Wire it into `KernelCommand::RunTask`:

```rust
KernelCommand::RunTask {
    agent_name,
    prompt,
    autonomous,
    no_checkpoint,  // new field
}
```

Open `crates/agentos-bus/src/message.rs`. Add to `KernelCommand::RunTask`:

```rust
/// When true, task executor skips checkpoint writes.
#[serde(default)]
no_checkpoint: bool,
```

### 4. Guard checkpoint writes in task executor

Open `crates/agentos-kernel/src/task_executor.rs`. Wrap the checkpoint write block (added in Phase 2) with:

```rust
if !task.skip_checkpoint {
    if let Some(ref cp_store) = self.checkpoint_store {
        // ... existing checkpoint write logic ...
    }
}
```

### 5. Add audit event types

Open `crates/agentos-audit/src/log.rs`. Add to `AuditEventType`:

```rust
CheckpointWritten,
CheckpointRestored,
CheckpointPruned,
```

### 6. Emit audit events

In `crates/agentos-kernel/src/task_executor.rs`, after a successful checkpoint write:

```rust
let _ = self.audit.append(AuditEntry {
    timestamp: chrono::Utc::now(),
    trace_id: task_trace_id.clone(),
    event_type: AuditEventType::CheckpointWritten,
    agent_id: Some(task.agent_id),
    task_id: Some(task.id),
    tool_id: None,
    details: serde_json::json!({
        "step_num": record.step_num,
        "checkpoint_id": record.id.to_string(),
        "context_entries": context.entries.len(),
    }),
    severity: AuditSeverity::Info,
    reversible: false,
    rollback_ref: None,
});
```

In `crates/agentos-kernel/src/commands/task.rs`, in `cmd_resume_task` after successful restore (before the existing audit entry, replace the `TaskStateChanged` event with `CheckpointRestored`):

```rust
event_type: AuditEventType::CheckpointRestored,
```

### 7. Set `skip_checkpoint` in `cmd_run_task`

Open `crates/agentos-kernel/src/commands/task.rs`. In `cmd_run_task`, when building the `AgentTask`:

```rust
let task = AgentTask {
    // ... existing fields ...
    skip_checkpoint: no_checkpoint,
};
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/timeout_checker.rs` | Add checkpoint pruning to sweep cycle |
| `crates/agentos-types/src/task.rs` | Add `skip_checkpoint: bool` to `AgentTask` |
| `crates/agentos-cli/src/commands/mod.rs` | Add `--no-checkpoint` flag to `task run` |
| `crates/agentos-bus/src/message.rs` | Add `no_checkpoint: bool` to `KernelCommand::RunTask` |
| `crates/agentos-kernel/src/task_executor.rs` | Guard checkpoint writes with `!task.skip_checkpoint` |
| `crates/agentos-audit/src/log.rs` | Add `CheckpointWritten`, `CheckpointRestored`, `CheckpointPruned` event types |
| `crates/agentos-kernel/src/commands/task.rs` | Emit `CheckpointRestored` audit event; pass `no_checkpoint` to `AgentTask` |

## Prerequisites

[[03-crash-recovery]] must be complete first -- this phase builds on the resume command and checkpoint infrastructure.

## Test Plan

- **Unit test `test_checkpoint_pruning`:** Create a checkpoint with `created_at` 73 hours ago. Run the sweep. Assert the checkpoint is deleted. Create another with `created_at = now`. Assert it survives.
- **Unit test `test_no_checkpoint_flag`:** Create an `AgentTask` with `skip_checkpoint: true`. Run task execution with a mock tool. Assert `CheckpointStore.get_latest()` returns `None`.
- **Unit test `test_checkpoint_written_audit_event`:** Run a task with checkpointing enabled. Query audit log for `CheckpointWritten` events. Assert at least one exists with correct `task_id`.
- **Unit test `test_checkpoint_restored_audit_event`:** Resume a task from checkpoint. Query audit log for `CheckpointRestored`. Assert one exists.
- **Unit test `test_skip_checkpoint_default_false`:** Deserialize an `AgentTask` from JSON without `skip_checkpoint` field. Assert it defaults to `false`.

## Verification

```bash
cargo build --workspace
cargo test --workspace -- --nocapture
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
