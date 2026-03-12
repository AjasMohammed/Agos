---
title: Kernel Modularization — Phase 0.1
tags:
  - next-steps
  - refactor
  - kernel
  - phase-0
date: 2026-03-11
status: partial
effort: 4h
priority: medium
feedback-plan-ref: "Phase 0.1"
---

# Kernel Modularization

> `kernel.rs` is the highest-churn file in the project. Splitting it unblocks parallel development and makes each piece independently testable.

---

## Current State

`crates/agentos-kernel/src/kernel.rs` is approximately **2,700 lines**. The `commands/` directory already exists and contains all 40+ `cmd_*` handlers — that work is done. The remaining bloat is:

1. **`execute_task_sync()` context assembly** — lines that build system prompt, inject agent directory, query episodic memory, and assemble initial context before the agent loop begins
2. **`validate_tool_call()`** — the pre-execution structural validation hook (partially moved to `intent_validator.rs` but may still exist inline)
3. **`Kernel::boot()`** — long initialization chain that creates all subsystems

The goal is to reduce `kernel.rs` to ~200 lines: just the `Kernel` struct definition and `boot()`.

---

## Target Module Layout

```
crates/agentos-kernel/src/
├── kernel.rs              ← Struct def + boot() only (~200 lines)
├── run_loop.rs            ← Already exists — main async message loop
├── task_executor.rs       ← Already exists — per-task execution logic
├── kernel_action.rs       ← Already exists — KernelAction dispatch
├── context_injector.rs    ← NEW — pre-task context assembly
├── task_completion.rs     ← NEW — post-task cleanup, episodic write
├── commands/
│   └── ...                ← Already complete
├── cost_tracker.rs        ← Already exists
├── escalation.rs          ← Already exists
├── identity.rs            ← Already exists
├── injection_scanner.rs   ← Already exists
├── intent_validator.rs    ← Already exists
├── resource_arbiter.rs    ← Already exists
├── risk_classifier.rs     ← Already exists
└── snapshot.rs            ← Planned (see [[03-Snapshot Rollback]])
```

---

## What to Extract

### `context_injector.rs` (NEW)

Extract the pre-task context assembly from `execute_task_sync()` in `task_executor.rs`. This currently lives around lines 97–200 of that file and includes:

- Building the system prompt string
- Injecting agent role/directory entries
- Querying episodic memory and injecting `[EPISODIC_RECALL]` block
- Pushing the user's task prompt with `pinned: true`

**Proposed interface:**
```rust
// crates/agentos-kernel/src/context_injector.rs

pub struct ContextInjector<'a> {
    context_manager: &'a Arc<ContextManager>,
    episodic_memory: &'a Arc<EpisodicStore>,
    agent_registry: &'a Arc<AgentRegistry>,
}

impl<'a> ContextInjector<'a> {
    /// Assemble and inject the full initial context for a task.
    /// Returns the number of context entries injected.
    pub async fn inject_task_context(
        &self,
        task: &AgentTask,
        system_prompt: &str,
    ) -> anyhow::Result<usize>;
}
```

### `task_completion.rs` (NEW)

Extract post-task cleanup logic (currently mixed into `execute_task_sync()` success path):

- Writing episodic summary on success (see [[05-Episodic Memory Completion]])
- Clearing task-scoped context entries
- Releasing all resource locks for the agent via `ResourceArbiter::release_all_for_agent()`
- Removing task edges from `TaskDependencyGraph`
- Writing `TaskCompleted` audit entry

**Proposed interface:**
```rust
// crates/agentos-kernel/src/task_completion.rs

pub struct TaskCompletionHandler<'a> {
    episodic_memory: &'a Arc<EpisodicStore>,
    resource_arbiter: &'a Arc<ResourceArbiter>,
    scheduler: &'a Arc<TaskScheduler>,
    audit: &'a Arc<AuditLog>,
}

impl<'a> TaskCompletionHandler<'a> {
    pub async fn on_success(&self, task: &AgentTask, answer: &str, tool_call_count: u32);
    pub async fn on_failure(&self, task: &AgentTask, error: &str);
}
```

---

## Step-by-Step Refactor Plan

> [!warning] Pure Refactor
> No behavior changes. Existing tests must pass before and after. Use `cargo test` as the invariant.

### Step 1 — Create `context_injector.rs`

1. Copy the context-assembly block from `task_executor.rs::execute_task_sync()` into `context_injector.rs`
2. Replace the inline code in `execute_task_sync()` with a call to `ContextInjector::inject_task_context()`
3. Run `cargo test` — must pass

### Step 2 — Create `task_completion.rs`

1. Copy the post-task block (resource release, episodic write, audit entry) from `execute_task_sync()`
2. Replace with `TaskCompletionHandler::on_success()` / `on_failure()` calls
3. Run `cargo test` — must pass

### Step 3 — Trim `kernel.rs`

After Steps 1 and 2, `kernel.rs` should already be significantly smaller. Review remaining content:
- Any inline validation logic that belongs in `intent_validator.rs` → move it
- Any command routing that belongs in `router.rs` → move it
- Target: `Kernel` struct + `boot()` + nothing else

### Step 4 — Update `lib.rs`

```rust
// crates/agentos-kernel/src/lib.rs
pub mod context_injector;
pub mod task_completion;
```

---

## Verification

```bash
# Before refactor
wc -l crates/agentos-kernel/src/kernel.rs   # baseline

# After refactor
wc -l crates/agentos-kernel/src/kernel.rs   # target: <300 lines
cargo test --workspace                       # must still pass
```

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/kernel.rs` | Remove extracted logic, retain struct + boot |
| `crates/agentos-kernel/src/task_executor.rs` | Replace inline blocks with calls to new modules |
| `crates/agentos-kernel/src/context_injector.rs` | **NEW** — context assembly |
| `crates/agentos-kernel/src/task_completion.rs` | **NEW** — post-task cleanup |
| `crates/agentos-kernel/src/lib.rs` | Add two new `pub mod` declarations |

---

## Related

- [[05-Episodic Memory Completion]] — `task_completion.rs` will host the episodic write
- [[Index]] — Back to dashboard
