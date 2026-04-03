---
title: "Phase 03 — Coordination Tools"
tags:
  - kernel
  - agents
  - tools
  - v4
  - plan
date: 2026-04-02
status: planned
effort: 2d
priority: high
---

# Phase 03 — Coordination Tools

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> Implement `spawn_agent`, `delegate_task`, and `await_agents` as first-class `AgentTool` instances so the LLM can call them like any other tool — no special handling needed in the inference loop.

---

## Why This Phase

Phases 1 and 2 add the kernel machinery, but agents can't use it yet. An LLM reasons through tools — it needs `spawn_agent` to appear in its tool list with a clear schema, be callable via the normal tool dispatch path, and return structured output it can reason about. This phase writes those tools and their SKILL.toml manifests.

---

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Multi-agent tools | None | `spawn_agent`, `delegate_task`, `await_agents` in `tools/core/` |
| Tool manifests | — | `spawn-agent.toml`, `delegate-task.toml`, `await-agents.toml` |
| Tool impl | — | `crates/agentos-tools/src/coordination.rs` |
| LLM tool list | No coordination tools | Three tools visible when `permissions` include `spawn` |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/coordination.rs` | New — tool implementations |
| `crates/agentos-tools/src/lib.rs` | Register coordination tools |
| `tools/core/spawn-agent.toml` | Tool manifest |
| `tools/core/delegate-task.toml` | Tool manifest |
| `tools/core/await-agents.toml` | Tool manifest |
| `crates/agentos-kernel/src/core_manifests.rs` | Include the three new manifests |

---

## Detailed Tasks

### Task 1: Write the tool implementations

**Files:**
- Create: `crates/agentos-tools/src/coordination.rs`

- [ ] **Step 1: Read how an existing tool is implemented**

Read `crates/agentos-tools/src/memory.rs` or `crates/agentos-tools/src/file.rs` to understand the `AgentTool` trait implementation pattern used in this codebase.

- [ ] **Step 2: Create `coordination.rs`**

Create `crates/agentos-tools/src/coordination.rs`:

```rust
//! Coordination tools: spawn_agent, delegate_task, await_agents.
//!
//! These tools let an LLM spawn sub-agents and aggregate their results
//! without any special-casing in the inference loop.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use agentos_types::{TaskID, ToolOutput};
use crate::{AgentTool, ToolContext, ToolError};

// ---------------------------------------------------------------------------
// spawn_agent
// ---------------------------------------------------------------------------

pub struct SpawnAgentTool;

#[derive(Debug, Deserialize)]
struct SpawnAgentInput {
    /// Name of the registered agent to run.
    agent: String,
    /// Goal / prompt for the sub-agent.
    prompt: String,
    /// Permissions to request (must be subset of caller's permissions).
    #[serde(default)]
    permissions: Vec<String>,
    /// How many of the parent's most recent context messages to pass to the child.
    #[serde(default = "default_context_messages")]
    context_messages: usize,
}

fn default_context_messages() -> usize { 10 }

#[derive(Debug, Serialize)]
struct SpawnAgentOutput {
    child_task_id: String,
    status: &'static str,
}

#[async_trait]
impl AgentTool for SpawnAgentTool {
    fn name(&self) -> &str { "spawn_agent" }

    fn description(&self) -> &str {
        "Spawn a sub-agent to handle a specific subtask. The child runs independently and \
         its result is automatically injected back into your context when it completes. \
         Use this to parallelize work or delegate specialist tasks."
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SpawnAgentInput = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;

        // Build optional context slice from parent's window.
        let context_slice = if args.context_messages > 0 {
            ctx.context_window().map(|w| {
                agentos_types::ContextSlice::last_n(w, args.context_messages, "parent-slice")
            })
        } else {
            None
        };

        let resp = ctx
            .kernel_client()
            .spawn_sub_agent(
                ctx.task_id(),
                &args.agent,
                &args.prompt,
                &args.permissions,
                context_slice,
            )
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let output = SpawnAgentOutput {
            child_task_id: resp.child_task_id.to_string(),
            status: "spawned",
        };

        Ok(ToolOutput::json(serde_json::to_value(output).unwrap()))
    }
}

// ---------------------------------------------------------------------------
// await_agents
// ---------------------------------------------------------------------------

pub struct AwaitAgentsTool;

#[derive(Debug, Deserialize)]
struct AwaitAgentsInput {
    /// Task IDs returned by spawn_agent calls.
    task_ids: Vec<String>,
}

#[async_trait]
impl AgentTool for AwaitAgentsTool {
    fn name(&self) -> &str { "await_agents" }

    fn description(&self) -> &str {
        "Wait for one or more spawned sub-agents to complete and retrieve their results. \
         Pass the task IDs returned by spawn_agent. Results are also injected automatically \
         into your context window."
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: AwaitAgentsInput = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;

        let task_ids: Vec<TaskID> = args
            .task_ids
            .iter()
            .map(|s| s.parse::<TaskID>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ToolError::InvalidInput(format!("invalid task id: {}", e)))?;

        let results = ctx
            .kernel_client()
            .await_sub_agents(ctx.task_id(), &task_ids)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let output = serde_json::json!({
            "results": results.iter().map(|(id, summary)| serde_json::json!({
                "task_id": id.to_string(),
                "output": summary,
            })).collect::<Vec<_>>()
        });

        Ok(ToolOutput::json(output))
    }
}
```

- [ ] **Step 3: Register in `lib.rs`**

In `crates/agentos-tools/src/lib.rs`, add:

```rust
pub mod coordination;
pub use coordination::{AwaitAgentsTool, SpawnAgentTool};
```

And in the function that returns all built-in tools (search for `all_tools()` or similar):

```rust
Box::new(SpawnAgentTool),
Box::new(AwaitAgentsTool),
```

- [ ] **Step 4: Write the unit test**

In `crates/agentos-tools/src/coordination.rs` `#[cfg(test)]`:

```rust
#[tokio::test]
async fn test_await_agents_invalid_task_id_returns_error() {
    let tool = AwaitAgentsTool;
    let ctx = ToolContext::for_test();
    let result = tool
        .execute(
            serde_json::json!({ "task_ids": ["not-a-uuid"] }),
            &ctx,
        )
        .await;
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}
```

- [ ] **Step 5: Run the test**

```bash
cargo test -p agentos-tools test_await_agents_invalid_task_id_returns_error
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-tools/src/coordination.rs crates/agentos-tools/src/lib.rs
git commit -m "feat(tools): add spawn_agent and await_agents coordination tools"
```

---

### Task 2: Write tool manifests

**Files:**
- Create: `tools/core/spawn-agent.toml`
- Create: `tools/core/await-agents.toml`

- [ ] **Step 1: Read an existing core manifest for format**

Read `tools/core/memory-write.toml` or any existing manifest to confirm the exact TOML schema fields used.

- [ ] **Step 2: Create `spawn-agent.toml`**

```toml
name = "spawn_agent"
version = "1.0.0"
description = "Spawn a sub-agent to handle a specific subtask."
trust_tier = "core"
permissions = ["spawn"]

[input_schema]
type = "object"
required = ["agent", "prompt"]

[input_schema.properties.agent]
type = "string"
description = "Name of the registered agent to run."

[input_schema.properties.prompt]
type = "string"
description = "Goal or prompt for the sub-agent."

[input_schema.properties.permissions]
type = "array"
items = { type = "string" }
description = "Permissions to request for the child (subset of your own)."

[input_schema.properties.context_messages]
type = "integer"
description = "Number of recent context messages to pass to the child. Default: 10."
```

- [ ] **Step 3: Create `await-agents.toml`**

```toml
name = "await_agents"
version = "1.0.0"
description = "Wait for spawned sub-agents to complete and retrieve their results."
trust_tier = "core"
permissions = ["spawn"]

[input_schema]
type = "object"
required = ["task_ids"]

[input_schema.properties.task_ids]
type = "array"
items = { type = "string" }
description = "Task IDs returned by spawn_agent calls to wait for."
```

- [ ] **Step 4: Register manifests in `core_manifests.rs`**

In `crates/agentos-kernel/src/core_manifests.rs`, add to the existing include list:

```rust
("spawn-agent.toml", include_str!("../../../tools/core/spawn-agent.toml")),
("await-agents.toml", include_str!("../../../tools/core/await-agents.toml")),
```

- [ ] **Step 5: Build to confirm manifests parse correctly**

```bash
cargo build -p agentos-kernel 2>&1 | grep -i "error\|panic"
```

Expected: clean

- [ ] **Step 6: Commit**

```bash
git add tools/core/spawn-agent.toml tools/core/await-agents.toml \
        crates/agentos-kernel/src/core_manifests.rs
git commit -m "feat(tools): add spawn_agent and await_agents core tool manifests"
```

---

### Task 3: Add `spawn` permission to `PermissionSet`

The manifests use `permissions = ["spawn"]` — this permission needs to exist.

- [ ] **Step 1: Check if `spawn` is a known permission string**

```bash
grep -rn '"spawn"' crates/agentos-capability/src/
```

- [ ] **Step 2: Add it if missing**

If `PermissionSet` validates against an allowlist, add `"spawn"` to it. If it's open-ended strings, no change needed.

- [ ] **Step 3: Write a test confirming `spawn` permission gates these tools**

```rust
#[tokio::test]
async fn test_spawn_agent_tool_requires_spawn_permission() {
    // A ToolContext with no permissions should be denied.
    let ctx = ToolContext::with_permissions(&[]);
    let tool = SpawnAgentTool;
    let result = tool
        .execute(
            serde_json::json!({ "agent": "worker", "prompt": "do it" }),
            &ctx,
        )
        .await;
    assert!(
        matches!(result, Err(ToolError::PermissionDenied(_))),
        "expected permission denied without spawn permission"
    );
}
```

- [ ] **Step 4: Run it**

```bash
cargo test -p agentos-tools test_spawn_agent_tool_requires_spawn_permission
```

Expected: PASS

- [ ] **Step 5: Run full suite**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-capability/src/ crates/agentos-tools/src/
git commit -m "feat(capability): add spawn permission for multi-agent coordination tools"
```

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

## Dependencies

- Requires: [[01-sub-agent-spawning]], [[02-context-handoff]]
- Blocks: [[04-agent-teams]]
