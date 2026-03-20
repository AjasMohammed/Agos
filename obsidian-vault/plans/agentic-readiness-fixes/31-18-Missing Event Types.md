---
title: "Add Missing Event Types"
tags:
  - next-steps
  - types
  - events
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 2h
priority: medium
---

# Add Missing Event Types

> Add `BudgetWarning`, `BudgetExhausted`, `ToolCallStarted`, and `ToolCallCompleted` event types so agents can subscribe to budget alerts and observe tool execution lifecycle.

## What to Do

The event system has 50+ event types but is missing critical lifecycle events. Agents can't subscribe to budget alerts or observe successful tool execution (only `ToolExecutionFailed` exists).

### Steps

1. **Add event types** to `EventType` in `crates/agentos-types/src/event.rs`:
   ```rust
   // Budget events
   BudgetWarning,      // Agent approaching budget limit
   BudgetExhausted,    // Agent hit budget limit

   // Tool lifecycle events
   ToolCallStarted,    // Tool execution beginning
   ToolCallCompleted,  // Tool execution succeeded
   ```

2. **Map to categories** in the `category()` method:
   - `BudgetWarning` / `BudgetExhausted` → `EventCategory::System` (or new `Budget` category)
   - `ToolCallStarted` / `ToolCallCompleted` → `EventCategory::Tool`

3. **Emit budget events** from `cost_tracker.rs`:
   - When `BudgetCheckResult::Warning` → emit `BudgetWarning` with `{ agent_id, usage_pct, limit }`
   - When `BudgetCheckResult::Kill` → emit `BudgetExhausted` with `{ agent_id, budget, actual }`

4. **Emit tool lifecycle events** from `task_executor.rs`:
   - Before `tool_runner.execute()` → emit `ToolCallStarted` with `{ tool_name, task_id, agent_id }`
   - After successful execution → emit `ToolCallCompleted` with `{ tool_name, task_id, duration_ms }`
   - `ToolExecutionFailed` already exists for failures

5. **Update agent-manual events section** with the new event types

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/event.rs` | Add 4 new event type variants |
| `crates/agentos-kernel/src/cost_tracker.rs` | Emit budget events |
| `crates/agentos-kernel/src/task_executor.rs` | Emit tool lifecycle events |
| `crates/agentos-tools/src/agent_manual.rs` | Document new event types |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-types
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```
