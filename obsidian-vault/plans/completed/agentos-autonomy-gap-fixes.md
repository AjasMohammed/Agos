---
title: Autonomy Gap Fixes
tags:
  - kernel
  - channels
  - multi-agent
  - scheduler
  - v3
date: 2026-04-14
status: complete
effort: 1d
priority: high
---

# Autonomy Gap Fixes

> Three targeted fixes to close the remaining gaps preventing fully autonomous agent operation without human intervention.

---

## Gaps Addressed

### Gap 1 (Non-issue): Response → context injection
`execute_ask_user` already blocks on an async oneshot receiver, restores `TaskState::Running` on response, and returns the response text as a `KernelActionResult`. The task_executor pushes this into context normally. **No fix needed.**

### Gap 2: Escalation → Channel Bridge
**Problem:** When an agent creates a blocking escalation (`EscalateToHuman`), the only external notification is an HTTP webhook POST (if `notify_url` is configured). There is no built-in channel (Slack, Discord, Telegram, etc.) notification. Humans must actively poll `agentos escalation list` or wire an external webhook relay.

**Fix:** In `execute_escalation()` (`crates/agentos-kernel/src/kernel_action.rs`), after `escalation_manager.create_escalation()`, call `self.notification_router.deliver()` with a non-blocking `UserMessage` (kind: Notification) containing the escalation ID, decision point, options, and the CLI command to resolve it. This routes via all registered channel adapters.

**Files changed:** `crates/agentos-kernel/src/kernel_action.rs`

### Gap 3: Sub-agent Blocking Await
**Problem:** `KernelAction::AwaitAgents` calls `cmd_await_sub_agents`, which is a snapshot poll. If children are still running it returns their state immediately, forcing the parent LLM to burn iterations polling. Auto-injection (task_completion.rs:221-249) still injects results on completion, but the parent wastes LLM calls in the meantime.

**Fix:**
1. Add `completion_notifiers: RwLock<HashMap<TaskID, Vec<oneshot::Sender<()>>>>` to `TaskScheduler`.
2. Add `async fn register_completion_notifier(task_id: TaskID) -> oneshot::Receiver<()>` — fires immediately if the task is already terminal.
3. In `update_state()` and `update_state_if_not_terminal()`, fire and drain notifiers when a task reaches terminal state.
4. In `dispatch_kernel_action` for `AwaitAgents`: if any children are non-terminal, register notifiers, park parent in `Waiting`, `tokio::select!` on all receivers + cancellation_token, then restore to `Running` and return all child results.

**Files changed:** `crates/agentos-kernel/src/scheduler.rs`, `crates/agentos-kernel/src/kernel_action.rs`

### Gap 4: Scheduled Job Output
**Problem:** `ScheduledJob.output_destination: Option<String>` exists in `agentos-types` but is always `None` — never set in `create_job()`, never passed through the CLI, never read in `task_completion.rs`.

**Fix:** Thread `output_destination` through the full stack:
- `KernelCommand::CreateSchedule` → add `output_destination: Option<String>` field
- `ScheduleManager::create_job()` → accept and store it
- `Kernel::cmd_create_schedule()` → pass it through
- CLI `schedule create` → add `--output <path>` flag
- `complete_task_success()` in `task_completion.rs` → after `emit_task_completed`, if `output_destination` is set, write the task result to that file path using `tokio::fs::write`

**Files changed:** `crates/agentos-bus/src/message.rs`, `crates/agentos-kernel/src/schedule_manager.rs`, `crates/agentos-kernel/src/commands/schedule.rs`, `crates/agentos-kernel/src/task_completion.rs`, `crates/agentos-cli/src/commands/schedule.rs`

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
