---
title: "Agent Self-Introspection Types"
tags:
  - next-steps
  - types
  - agent
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 4h
priority: medium
---

# Agent Self-Introspection Types

> Add types and a tool for agents to query their own state: permissions, budget, registered tools, active subscriptions.

## What to Do

There's no type representing "what I know about myself." An agent needs to query:
- Current permissions (exists in `CapabilityToken` but not easily queryable)
- Budget status (exists as `CostSnapshot` but no dedicated tool)
- Registered tools (exists in kernel but no snapshot type)
- Active event subscriptions (no query type)

### Steps

1. **Define `AgentSelfView` type** in `crates/agentos-types/src/`:
   ```rust
   pub struct AgentSelfView {
       pub agent_id: AgentID,
       pub name: String,
       pub status: AgentStatus,
       pub permissions: Vec<String>,        // Current permission set
       pub deny_entries: Vec<String>,        // Denied permissions
       pub budget: Option<BudgetSummary>,    // Current budget state
       pub tools: Vec<String>,              // Available tool names
       pub subscriptions: Vec<SubscriptionSummary>,
       pub active_tasks: Vec<TaskSummary>,
   }

   pub struct BudgetSummary {
       pub input_tokens_used: u64,
       pub output_tokens_used: u64,
       pub total_cost_usd: f64,
       pub budget_limit_usd: Option<f64>,
       pub remaining_usd: Option<f64>,
   }

   pub struct SubscriptionSummary {
       pub subscription_id: SubscriptionID,
       pub event_type: String,
       pub enabled: bool,
   }
   ```

2. **Add `agent-self` tool** in `crates/agentos-tools/src/`:
   - Query the agent registry, cost tracker, event bus, and tool registry
   - Assemble an `AgentSelfView`
   - Return as JSON
   - Requires no special permissions (read-only, own data only)

3. **Add TOML manifest** at `tools/core/agent-self.toml`

4. **Register in `ToolRunner`** and add to agent-manual

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/lib.rs` | Add `AgentSelfView`, `BudgetSummary`, `SubscriptionSummary` |
| `crates/agentos-tools/src/agent_self.rs` | New tool implementation |
| `crates/agentos-tools/src/runner.rs` | Register new tool |
| `tools/core/agent-self.toml` | New manifest |

## Prerequisites

- [[31-14-TaskState Suspended and Budget Errors]] (for `BudgetSummary` to include the new error types)

## Verification

```bash
cargo test -p agentos-types
cargo test -p agentos-tools
cargo clippy --workspace -- -D warnings
```

Test: agent calls `agent-self` tool → receives JSON with their own agent_id, permissions, budget status, and available tools.
