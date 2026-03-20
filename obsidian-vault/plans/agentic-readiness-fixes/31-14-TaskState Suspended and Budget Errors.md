---
title: "Add TaskState::Suspended and Budget Error Variants"
tags:
  - next-steps
  - types
  - task
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 2h
priority: high
---

# Add TaskState::Suspended and Budget Error Variants

> Add the missing `TaskState::Suspended` variant to match `BudgetAction::Suspend`, and add `AgentOSError::BudgetExceeded` and `RateLimited` error variants.

## What to Do

`BudgetAction::Suspend` exists but there's no corresponding `TaskState::Suspended`. Budget violations use generic `KernelError`. Rate limit hits are indistinguishable from other errors.

### Steps

1. **Add `TaskState::Suspended`** in `crates/agentos-types/src/task.rs`:
   - Add variant to the `TaskState` enum
   - Update `can_transition_to()`: Running → Suspended allowed, Suspended → Running allowed (resume), Suspended → Cancelled allowed
   - Update `is_terminal()`: Suspended is NOT terminal
   - Update serialization

2. **Add `AgentOSError::BudgetExceeded`** in `crates/agentos-types/src/error.rs`:
   ```rust
   #[error("Budget exceeded for agent {agent_id}: {detail}")]
   BudgetExceeded { agent_id: String, detail: String },
   ```

3. **Add `AgentOSError::RateLimited`** in `crates/agentos-types/src/error.rs`:
   ```rust
   #[error("Rate limited: {detail}")]
   RateLimited { detail: String },
   ```

4. **Wire `BudgetAction::Suspend`** in `task_executor.rs`:
   - When budget check returns `Suspend`, transition task to `TaskState::Suspended`
   - Emit `TaskEvent::TaskSuspended` event

5. **Wire `BudgetExceeded`** in `cost_tracker.rs`:
   - When budget action is `Kill`, return `AgentOSError::BudgetExceeded` instead of generic error

6. **Wire `RateLimited`** in `rate_limit.rs`:
   - When rate limit exceeded, return `AgentOSError::RateLimited`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/task.rs` | Add `Suspended` variant, update state machine |
| `crates/agentos-types/src/error.rs` | Add `BudgetExceeded`, `RateLimited` variants |
| `crates/agentos-kernel/src/task_executor.rs` | Wire Suspended transition |
| `crates/agentos-kernel/src/cost_tracker.rs` | Return `BudgetExceeded` |
| `crates/agentos-kernel/src/rate_limit.rs` | Return `RateLimited` |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-types
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: task state transition Running → Suspended → Running works. Budget kill returns `BudgetExceeded`. Rate limit returns `RateLimited`.
