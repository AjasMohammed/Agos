---
title: Wire AgentManual into ToolRunner and Registry
tags:
  - tools
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 1h
priority: high
---

# Wire AgentManual into ToolRunner and Registry

> Register the `AgentManualTool` in `ToolRunner` so the kernel dispatches `agent-manual` tool calls to it. Build tool summaries from the tool registry at construction time.

---

## Why This Subtask

The tool implementation exists (subtasks 01-02), but the kernel cannot dispatch to it until it is registered in `ToolRunner::tools`. This subtask connects the tool to the runtime. The key design challenge is that `AgentManualTool` needs a snapshot of the tool registry at construction time, but `ToolRunner` is constructed before the tool registry is fully populated. The solution: add a `register_agent_manual()` method that the kernel calls after tool registry initialization.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `ToolRunner` registration | `agent-manual` not registered | `agent-manual` registered with tool summaries from `ToolRegistry` |
| `ToolRunner::register_agent_manual()` | Does not exist | New method accepting `Vec<ToolSummary>` |
| `Kernel::boot()` | Does not construct `AgentManualTool` | Calls `tool_runner.register_agent_manual(summaries)` after loading tool registry |

---

## What to Do

### 1. Add `register_agent_manual()` to `ToolRunner`

Open `crates/agentos-tools/src/runner.rs`. Add this method to the `impl ToolRunner` block:

```rust
/// Register the agent-manual tool with a snapshot of tool summaries.
/// Called by the kernel after the tool registry is fully loaded, so the
/// manual has an accurate view of all available tools.
pub fn register_agent_manual(
    &mut self,
    tool_summaries: Vec<crate::agent_manual::ToolSummary>,
) {
    self.register(Box::new(
        crate::agent_manual::AgentManualTool::new(tool_summaries),
    ));
}
```

Also add the import at the top of `runner.rs` if not already present:

```rust
use crate::agent_manual::AgentManualTool;
```

### 2. Wire in `Kernel::boot()`

Open `crates/agentos-kernel/src/kernel.rs`. After the `ToolRunner` is created and after the `ToolRegistry` is loaded (after the WASM tool registration block, around line 320 where `let tool_runner = Arc::new(tool_runner);` is), insert the following **before** wrapping in `Arc::new(tool_runner)`:

```rust
// Register agent-manual tool with a snapshot of all registered tools.
{
    let registry_read = tool_registry.read().await;
    let all_tools: Vec<&agentos_types::RegisteredTool> = registry_read.list_all();
    let summaries = agentos_tools::agent_manual::AgentManualTool::summaries_from_registry(&all_tools);
    tool_runner.register_agent_manual(summaries);
}
```

This goes just before `let tool_runner = Arc::new(tool_runner);` (around line 320 in kernel.rs).

**Important:** The `tool_runner` variable is still a `ToolRunner` (not yet `Arc<ToolRunner>`) at this point, so `&mut self` calls work.

### 3. Update `list_tools()` expectation

The `test_tool_runner_lists_all_built_in_tools` test in `crates/agentos-tools/src/lib.rs` asserts `tools.len() >= 5`. After adding `agent-manual`, the count increases by 1. This test should still pass because it uses `>=`, but verify.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/runner.rs` | Add `register_agent_manual()` method, import `AgentManualTool` |
| `crates/agentos-kernel/src/kernel.rs` | Add agent-manual registration block after WASM tool registration |

---

## Prerequisites

[[27-02-Implement Section Content Generators]] must be complete first (the tool must be fully functional before wiring it in).

---

## Test Plan

- `cargo build --workspace` must compile.
- `cargo test -p agentos-tools -- test_tool_runner_lists_all_built_in_tools` must still pass and the tool list must include `agent-manual`.
- Add a test to `crates/agentos-tools/src/lib.rs` tests:

```rust
#[tokio::test]
async fn test_agent_manual_tool_via_runner() {
    let dir = TempDir::new().unwrap();
    let runner = ToolRunner::new(dir.path()).unwrap();
    let tools = runner.list_tools();
    assert!(tools.contains(&"agent-manual".to_string()), "agent-manual should be registered");
}
```

Note: `ToolRunner::new()` does not call `register_agent_manual()` (since there is no registry), so `agent-manual` will NOT be in the list from `ToolRunner::new()`. The test above would need to explicitly register it. Better test approach:

```rust
#[tokio::test]
async fn test_agent_manual_registered_with_summaries() {
    let dir = TempDir::new().unwrap();
    let mut runner = ToolRunner::new(dir.path()).unwrap();
    runner.register_agent_manual(vec![
        crate::agent_manual::ToolSummary {
            name: "test-tool".into(),
            description: "A test".into(),
            version: "0.1.0".into(),
            permissions: vec![],
            input_schema: None,
            trust_tier: "core".into(),
        },
    ]);
    let tools = runner.list_tools();
    assert!(tools.contains(&"agent-manual".to_string()));

    let ctx = make_context(dir.path());
    let result = runner.execute(
        "agent-manual",
        serde_json::json!({"section": "index"}),
        ctx,
    ).await.unwrap();
    assert_eq!(result["section"], "index");
}
```

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-tools -- agent_manual --nocapture
cargo test -p agentos-tools -- test_tool_runner --nocapture
cargo clippy --workspace -- -D warnings
```
