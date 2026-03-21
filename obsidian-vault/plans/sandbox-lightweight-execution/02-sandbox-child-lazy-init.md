---
title: "Phase 02: Wire Sandbox Child to Use Factory"
tags:
  - kernel
  - sandbox
  - cli
  - v3
  - plan
date: 2026-03-21
status: planned
effort: 4h
priority: critical
---

# Phase 02: Wire Sandbox Child to Use Factory

> Replace `ToolRunner::new(&data_dir)` in `run_sandbox_exec()` with `build_single_tool()` so sandbox children only initialize the one tool they need.

---

## Why This Phase

Phase 01 created the `build_single_tool()` factory function. This phase wires it into the actual sandbox child entry point (`run_sandbox_exec()` in `crates/agentos-cli/src/main.rs`), which is the function called when `agentctl --sandbox-exec <request-file>` is invoked by `SandboxExecutor::spawn()`.

Currently, line 379 of main.rs does:
```rust
let tool_runner = agentos_tools::ToolRunner::new(&data_dir)?;
```
This constructs all 35+ tools, loads the embedder, and opens 3 SQLite databases. After this phase, it will construct only the single requested tool.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `run_sandbox_exec()` tool init | `ToolRunner::new(&data_dir)` | `build_single_tool(tool_name, &data_dir)` |
| Tool execution | `tool_runner.execute(tool_name, payload, ctx)` | `tool.execute(payload, ctx)` directly |
| Permission check | `ToolRunner::execute()` does defense-in-depth check | Sandbox child re-checks `tool.required_permissions()` against request permissions |
| Error on unknown tool | `ToolNotFound` from ToolRunner hashmap | `ToolNotFound` from factory returning None |

---

## What to Do

### 1. Modify `run_sandbox_exec()` in `crates/agentos-cli/src/main.rs`

Replace the current implementation (lines 364-405) with:

```rust
async fn run_sandbox_exec(request_path: &str) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(request_path)
        .map_err(|e| anyhow::anyhow!("sandbox-exec: cannot read request file: {}", e))?;
    let request: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("sandbox-exec: invalid JSON in request file: {}", e))?;

    let tool_name = request["tool_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("sandbox-exec: missing tool_name"))?;
    let payload = request["payload"].clone();
    let data_dir_str = request["data_dir"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("sandbox-exec: missing data_dir"))?;
    let data_dir = std::path::PathBuf::from(data_dir_str);

    // Build only the single tool we need -- avoids initializing the embedder,
    // SQLite stores, and 34 other unused tools that ToolRunner::new() would create.
    let tool = agentos_tools::build_single_tool(tool_name, &data_dir)
        .map_err(|e| anyhow::anyhow!("sandbox-exec: failed to build tool '{}': {}", tool_name, e))?
        .ok_or_else(|| anyhow::anyhow!(
            "sandbox-exec: tool '{}' cannot run in sandbox (kernel-context or unknown)",
            tool_name,
        ))?;

    let ctx = agentos_tools::ToolExecutionContext {
        data_dir,
        task_id: agentos_types::TaskID::new(),
        agent_id: agentos_types::AgentID::new(),
        trace_id: agentos_types::TraceID::new(),
        // Permissions were already validated by the kernel before spawning.
        permissions: agentos_types::PermissionSet::new(),
        vault: None,
        hal: None,
        file_lock_registry: None,
        agent_registry: None,
        task_registry: None,
        workspace_paths: vec![],
        cancellation_token: tokio_util::sync::CancellationToken::new(),
    };

    let result = tool
        .execute(payload, ctx)
        .await
        .map_err(|e| anyhow::anyhow!("sandbox-exec: tool error: {}", e))?;

    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
```

Key changes from the current code:
- `let tool_runner = agentos_tools::ToolRunner::new(&data_dir)?` is replaced with `agentos_tools::build_single_tool(tool_name, &data_dir)?`
- The request JSON evolves into a typed `SandboxExecRequest` so the child can preserve `task_id`, `agent_id`, `trace_id`, `permissions`, and `workspace_paths`
- `tool_runner.execute(tool_name, payload, ctx)` is replaced with `tool.execute(payload, ctx)` but the child still performs a lightweight permission check against `tool.required_permissions()` for defense in depth
- HAL-category tools construct a local sandbox HAL so `hardware-info`, `sys-monitor`, `process-manager`, `log-reader`, and `network-monitor` still have their runtime dependencies
- Error messages are more descriptive about sandbox-specific failure modes

### 2. Verify the import path

`crates/agentos-cli/Cargo.toml` still needs `agentos-tools`, and the real implementation also imports `agentos-sandbox` for the typed request struct plus `agentos-hal` for the local sandbox HAL.

### 3. Verify the `ToolExecutionContext` fields

The sandbox child should preserve the real `task_id`, `agent_id`, `trace_id`, `permissions`, and `workspace_paths` from the request when present, falling back to defaults only when the request omits them. This keeps audit trails and permission enforcement consistent with the in-process path.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-cli/src/main.rs` | Replace `run_sandbox_exec()` body: `ToolRunner::new` -> `build_single_tool` |

---

## Prerequisites

[[01-single-tool-factory]] must be complete (provides `build_single_tool` function).

---

## Test Plan

- `cargo build -p agentos-cli` must succeed
- `cargo test -p agentos-cli test_sandbox_exec_datetime_smoke -- --nocapture` should pass as a real `SandboxExecutor` -> `agentctl --sandbox-exec` smoke test
- Manual test: run `agentctl start`, then `agentctl task run --agent <name> "What time is it?"` -- the `datetime` tool should execute in sandbox without OOM
- Verify via kernel logs that sandbox child spawns successfully:
  - Look for `Sandbox child spawned` log with `tool = "datetime"`
  - Look for `Sandbox child completed` with `exit_code = 0`
  - Confirm no `Sandbox child killed by signal` errors
- Run `cargo test -p agentos-cli` to verify CLI parsing tests still pass

---

## Verification

```bash
cargo build -p agentos-cli
cargo test -p agentos-cli
cargo clippy -p agentos-cli --tests -- -D warnings
```

To verify sandbox behavior manually:
```bash
# Create a test request file
echo '{"tool_name":"datetime","payload":{},"data_dir":"/tmp/agentos/data"}' > /tmp/sandbox-test.json

# Run directly (outside sandbox, no seccomp)
./target/debug/agentctl --sandbox-exec /tmp/sandbox-test.json

# Expected: JSON output like {"iso8601":"2026-03-21T...","unix_secs":...}
```
