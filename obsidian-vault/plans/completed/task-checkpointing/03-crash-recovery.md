---
title: "Phase 3: Crash Recovery"
tags:
  - kernel
  - durability
  - plan
date: 2026-04-03
status: complete
effort: 2d
priority: high
---

# Phase 3: Crash Recovery

> Add a kernel boot step that discovers checkpointed tasks and a CLI command `agentos task resume <task-id>` that restores a task from its checkpoint -- deserializing the context window, decrypting it, and re-enqueuing the task on the scheduler.

---

## Why This Phase

Phases 1-2 write checkpoints but never read them. This phase closes the loop: on kernel boot, the system discovers which tasks have checkpoints available (logging them for the operator), and the CLI provides an explicit `task resume` command to restore a specific task.

Recovery is opt-in (not automatic) because silent auto-resume could cause duplicate side effects -- a task that called a shell command or sent an email before crashing would re-execute those actions.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Boot recovery | 17-step boot sequence; no checkpoint scan | 18-step boot: Step 18 queries `CheckpointStore` and logs recoverable tasks |
| CLI resume command | Does not exist | `agentos task resume <task-id>` sends `KernelCommand::ResumeTask` |
| Task restoration | Not possible | Deserialize checkpoint, decrypt context, rebuild `AgentTask`, re-enqueue at saved step |
| `KernelCommand` | No resume variant | New `ResumeTask { task_id: TaskID }` variant |
| `KernelResponse` | No resume response | Reuses `TaskStarted { task_id }` from Phase 1 of [[Event-Driven Completion Plan]] |

## What to Do

### 1. Add boot checkpoint discovery

Open `crates/agentos-kernel/src/run_loop.rs`. In the boot sequence (after the existing 17 steps), add:

```rust
// Step 18: Discover checkpointed tasks (informational -- no auto-resume).
if let Some(ref cp_store) = kernel.checkpoint_store {
    match cp_store.list_checkpointed_tasks().await {
        Ok(task_ids) if !task_ids.is_empty() => {
            tracing::info!(
                count = task_ids.len(),
                task_ids = ?task_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "Boot: found {} tasks with checkpoints -- use 'agentos task resume <id>' to restore",
                task_ids.len()
            );
        }
        Ok(_) => {
            tracing::debug!("Boot: no checkpointed tasks found");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Boot: failed to query checkpoint store");
        }
    }
}
```

### 2. Add `ResumeTask` command

Open `crates/agentos-bus/src/message.rs`. Add to `KernelCommand`:

```rust
/// Resume a task from its latest checkpoint.
ResumeTask {
    task_id: TaskID,
},
```

### 3. Add `ListCheckpoints` command

Open `crates/agentos-bus/src/message.rs`. Add to `KernelCommand`:

```rust
/// List all tasks that have checkpoints available for resume.
ListCheckpoints,
```

Add to `KernelResponse`:

```rust
/// List of task IDs with available checkpoints.
CheckpointList(Vec<serde_json::Value>),
```

### 4. Implement `cmd_resume_task`

Open `crates/agentos-kernel/src/commands/task.rs`. Add:

```rust
pub(crate) async fn cmd_resume_task(&self, task_id: TaskID) -> KernelResponse {
    let cp_store = match &self.checkpoint_store {
        Some(store) => store,
        None => {
            return KernelResponse::Error {
                message: "checkpointing is not enabled".to_string(),
            };
        }
    };

    // 1. Load the latest checkpoint for this task.
    let record = match cp_store.get_latest(&task_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return KernelResponse::Error {
                message: format!("no checkpoint found for task '{}'", task_id),
            };
        }
        Err(e) => {
            return KernelResponse::Error {
                message: format!("failed to load checkpoint: {e}"),
            };
        }
    };

    // 2. Decrypt and deserialize the context window.
    let context_json = if let Some(ref vault) = self.vault {
        match vault.decrypt_bytes(&record.context_blob).await {
            Ok(decrypted) => decrypted,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("failed to decrypt checkpoint context: {e}"),
                };
            }
        }
    } else {
        record.context_blob.clone()
    };

    let context: ContextWindow = match serde_json::from_slice(&context_json) {
        Ok(ctx) => ctx,
        Err(e) => {
            return KernelResponse::Error {
                message: format!("failed to deserialize checkpoint context: {e}"),
            };
        }
    };

    // 3. Deserialize task state.
    let cp_state: CheckpointTaskState = match serde_json::from_str(&record.task_state_json) {
        Ok(s) => s,
        Err(e) => {
            return KernelResponse::Error {
                message: format!("failed to deserialize checkpoint task state: {e}"),
            };
        }
    };

    // 4. Verify the agent still exists and is online.
    let agent = {
        let registry = self.agent_registry.read().await;
        match registry.get_by_id(&cp_state.agent_id) {
            Some(a) if a.status != AgentStatus::Offline => a.clone(),
            Some(_) => {
                return KernelResponse::Error {
                    message: format!(
                        "agent '{}' is offline -- cannot resume task",
                        cp_state.agent_id
                    ),
                };
            }
            None => {
                return KernelResponse::Error {
                    message: format!(
                        "agent '{}' not found -- cannot resume task",
                        cp_state.agent_id
                    ),
                };
            }
        }
    };

    // 5. Issue a fresh capability token (old one may be expired).
    let effective_permissions = {
        let registry = self.agent_registry.read().await;
        registry.compute_effective_permissions(&agent.id)
    };
    let task_timeout = Duration::from_secs(self.config.kernel.default_task_timeout_secs);
    let capability_token = match self.capability_engine.issue_token(
        task_id,
        agent.id,
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::from([
            IntentTypeFlag::Read, IntentTypeFlag::Write, IntentTypeFlag::Execute,
            IntentTypeFlag::Query, IntentTypeFlag::Observe, IntentTypeFlag::Message,
            IntentTypeFlag::Delegate, IntentTypeFlag::Broadcast, IntentTypeFlag::Escalate,
            IntentTypeFlag::Subscribe, IntentTypeFlag::Unsubscribe,
        ]),
        effective_permissions,
        task_timeout,
    ) {
        Ok(t) => t,
        Err(e) => {
            return KernelResponse::Error {
                message: format!("failed to issue capability token for resumed task: {e}"),
            };
        }
    };

    // 6. Rebuild AgentTask.
    let task = AgentTask {
        id: task_id,
        state: TaskState::Queued,
        agent_id: agent.id,
        capability_token,
        assigned_llm: Some(agent.id),
        priority: 5,
        created_at: chrono::Utc::now(),
        started_at: None,
        timeout: task_timeout,
        original_prompt: cp_state.original_prompt,
        history: Vec::new(),
        parent_task: cp_state.parent_task_id,
        reasoning_hints: None,
        max_iterations: None,
        trigger_source: None,
        autonomous: cp_state.autonomous,
        parent_task_id: cp_state.parent_task_id,
        spawn_depth: cp_state.spawn_depth,
        is_team_coordinator: false,
    };

    // 7. Restore context window.
    self.context_manager.replace_context(&task_id, context).await.ok();

    // 8. Enqueue and start.
    self.scheduler.register_external(task.clone()).await;
    self.scheduler
        .update_state_if_not_terminal(&task.id, TaskState::Running)
        .await
        .ok();

    tracing::info!(
        task_id = %task_id,
        agent_name = %agent.name,
        step_restored = record.step_num,
        "Task resumed from checkpoint"
    );

    // 9. Audit entry.
    let _ = self.audit.append(agentos_audit::AuditEntry {
        timestamp: chrono::Utc::now(),
        trace_id: TraceID::new(),
        event_type: agentos_audit::AuditEventType::TaskStateChanged,
        agent_id: Some(agent.id),
        task_id: Some(task_id),
        tool_id: None,
        details: serde_json::json!({
            "kind": "task_resumed_from_checkpoint",
            "step_restored": record.step_num,
            "checkpoint_id": record.id.to_string(),
        }),
        severity: agentos_audit::AuditSeverity::Info,
        reversible: false,
        rollback_ref: None,
    });

    KernelResponse::TaskStarted { task_id }
}
```

### 5. Wire dispatch in `run_loop.rs`

Open `crates/agentos-kernel/src/run_loop.rs`. Add dispatch arms:

```rust
KernelCommand::ResumeTask { task_id } => {
    kernel.cmd_resume_task(task_id).await
}
KernelCommand::ListCheckpoints => {
    kernel.cmd_list_checkpoints().await
}
```

### 6. Add CLI subcommand

Open `crates/agentos-cli/src/commands/mod.rs`. Add `task resume` subcommand:

```rust
/// Resume a task from its latest checkpoint.
Resume {
    /// Task ID to resume.
    task_id: String,
},
```

Wire it to send `KernelCommand::ResumeTask` to the bus.

### 7. Add `cmd_list_checkpoints`

Open `crates/agentos-kernel/src/commands/task.rs`. Add:

```rust
pub(crate) async fn cmd_list_checkpoints(&self) -> KernelResponse {
    let cp_store = match &self.checkpoint_store {
        Some(store) => store,
        None => return KernelResponse::CheckpointList(vec![]),
    };
    match cp_store.list_checkpointed_tasks().await {
        Ok(ids) => {
            let entries: Vec<serde_json::Value> = ids.iter().map(|id| {
                serde_json::json!({ "task_id": id.to_string() })
            }).collect();
            KernelResponse::CheckpointList(entries)
        }
        Err(e) => KernelResponse::Error {
            message: format!("failed to list checkpoints: {e}"),
        },
    }
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-bus/src/message.rs` | Add `ResumeTask`, `ListCheckpoints` to `KernelCommand`; `CheckpointList` to `KernelResponse` |
| `crates/agentos-kernel/src/run_loop.rs` | Add boot step 18 (checkpoint discovery); dispatch arms for `ResumeTask`, `ListCheckpoints` |
| `crates/agentos-kernel/src/commands/task.rs` | Add `cmd_resume_task`, `cmd_list_checkpoints` |
| `crates/agentos-cli/src/commands/mod.rs` | Add `task resume` subcommand |

## Prerequisites

[[02-state-serialization]] must be complete first -- this phase reads checkpoints written by Phase 2.

## Test Plan

- **Unit test `test_resume_restores_context`:** Write a checkpoint with a known context window (3 entries). Call `cmd_resume_task`. Assert `context_manager.get_context()` returns a window with 3 entries.
- **Unit test `test_resume_fails_for_missing_checkpoint`:** Call `cmd_resume_task` with an unknown `TaskID`. Assert `KernelResponse::Error` with "no checkpoint found".
- **Unit test `test_resume_fails_for_offline_agent`:** Write a checkpoint referencing an agent that is offline. Call `cmd_resume_task`. Assert error about agent being offline.
- **Unit test `test_list_checkpoints_returns_task_ids`:** Write checkpoints for 2 tasks. Call `cmd_list_checkpoints`. Assert 2 entries returned.
- **Unit test `test_boot_logs_checkpointed_tasks`:** Boot a kernel with a pre-populated checkpoint DB. Assert log output contains "found N tasks with checkpoints".

## Verification

```bash
cargo build -p agentos-kernel -p agentos-bus -p agentos-cli
cargo test -p agentos-kernel -- checkpoint --nocapture
cargo test -p agentos-kernel -- resume --nocapture
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
