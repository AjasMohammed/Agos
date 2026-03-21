---
title: Phase 2 — Tools Crate Logging
tags:
  - observability
  - logging
  - tools
  - phase-2
  - next-steps
date: 2026-03-21
status: planned
effort: 1d
priority: high
---

# Phase 2 — Tools Crate Logging

> Add structured `tracing` calls to every tool in `agentos-tools` so all tool execution is visible at `debug` level — file reads, shell commands, HTTP requests, memory ops — rather than silently succeeding or failing.

---

## Why This Phase

Currently only 6 tracing calls exist across all of `agentos-tools`. When a tool fails in production, there is zero log output from the tool itself — only the kernel error propagation. This makes it impossible to distinguish "file not found" from "permission denied" from "path traversal blocked" without adding ad-hoc `println!` statements.

After Phase 1 establishes span context (task_id, agent_id), Phase 2 tools will automatically inherit that context — so every tool log line will be stamped with the task that triggered it.

---

## Current → Target State

**Current:** File tool executes, fails, returns Err — no trace.
**Target:**
```
DEBUG tool_call{tool=file-reader task_id=task-abc}: agentos_tools::file_editor: Reading file path="/workspace/src/main.rs" size_hint=None
DEBUG tool_call{tool=file-reader task_id=task-abc}: agentos_tools::file_editor: File read complete bytes=4096
```

---

## Tool Inventory and Logging Plan

### File Editor / Reader (`src/file_editor.rs`, `src/file_reader.rs`)

These are the most critical — file I/O failures are silent today.

```rust
// On entry
tracing::debug!(path = %path.display(), op = "read", "File operation starting");

// On success
tracing::debug!(path = %path.display(), bytes = content.len(), "File read complete");

// On permission/path error (already returns Err, but log before returning)
tracing::warn!(path = %path.display(), "Path traversal blocked");
tracing::warn!(path = %path.display(), error = %e, "File read failed");
```

**What NOT to log:** File contents (security risk). Only log path, byte counts, and errors.

---

### Shell Runner (`src/runner.rs`)

Shell execution has the most debugging value — log command, exit code, stderr.

```rust
// Before exec
tracing::debug!(command = %cmd, working_dir = %cwd, "Shell command starting");

// After exec
tracing::debug!(
    command = %cmd,
    exit_code = %output.status.code().unwrap_or(-1),
    stdout_bytes = output.stdout.len(),
    stderr_bytes = output.stderr.len(),
    "Shell command completed"
);

// On non-zero exit (not necessarily an error, but worth logging)
if !output.status.success() {
    tracing::warn!(
        command = %cmd,
        exit_code = %code,
        stderr = %String::from_utf8_lossy(&output.stderr),
        "Shell command exited with non-zero status"
    );
}
```

**What NOT to log:** stdin contents or full stdout (may contain secrets). Only log exit code, byte counts, and stderr for failures.

---

### HTTP Client (`src/http_client.rs`)

```rust
// Before request
tracing::debug!(method = %method, url = %url, "HTTP request starting");

// After response
tracing::debug!(
    method = %method,
    url = %url,
    status = %response.status().as_u16(),
    "HTTP request completed"
);

// On error
tracing::warn!(method = %method, url = %url, error = %e, "HTTP request failed");
```

**SSRF warning already exists in PermissionSet.check() — do not duplicate. Log only at the tool callsite.**

---

### Web Fetch (`src/web_fetch.rs`)

Same pattern as HTTP client (it wraps it). Add at the web_fetch level only to avoid duplicate lines.

---

### Memory Tool (`src/memory.rs` if present)

```rust
tracing::debug!(operation = "store", key = %key, "Memory write");
tracing::debug!(operation = "retrieve", key = %key, found = result.is_some(), "Memory read");
```

---

### Data Parser (`src/data_parser.rs`)

```rust
tracing::debug!(format = %format, input_bytes = input.len(), "Parsing data");
tracing::warn!(format = %format, error = %e, "Parse failed");
```

---

### Agent Manual (`src/agent_manual.rs`)

```rust
tracing::debug!(query = %query, "Agent manual lookup");
```

---

## Implementation Pattern (Template)

For every tool, use this minimal pattern — add more fields where useful, but every tool must have at minimum an entry debug line and an error warn line:

```rust
use tracing;

pub async fn execute(&self, input: MyInput, ...) -> Result<ToolOutput, AgentOSError> {
    tracing::debug!(/* key fields */, "Tool: <name> starting");

    let result = inner_logic(&input).await;

    match &result {
        Ok(_) => tracing::debug!(/* key fields */, "Tool: <name> succeeded"),
        Err(e) => tracing::warn!(error = %e, /* key fields */, "Tool: <name> failed"),
    }

    result
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/file_editor.rs` | Entry debug + error warn + traversal warn |
| `crates/agentos-tools/src/runner.rs` | Command debug + exit warn on non-zero |
| `crates/agentos-tools/src/http_client.rs` | Request debug + status debug + error warn |
| `crates/agentos-tools/src/web_fetch.rs` | Fetch debug + error warn |
| `crates/agentos-tools/src/memory.rs` | Store/retrieve debug |
| `crates/agentos-tools/src/data_parser.rs` | Parse debug + error warn |
| `crates/agentos-tools/src/agent_manual.rs` | Query debug |
| `crates/agentos-tools/src/lib.rs` | Verify `tracing` in Cargo.toml deps |

---

## Dependencies

- [[01-span-instrumentation]] must be complete so tool logs inherit task_id/agent_id from the parent span.

---

## Test Plan

1. `cargo build -p agentos-tools` — must compile clean
2. `cargo clippy -p agentos-tools -- -D warnings` — no warnings
3. Run with `RUST_LOG=agentos_tools=debug` — tool execution lines should appear
4. Trigger a file-not-found error — warn line should appear with path and error
5. Trigger a shell command failure — warn with exit code and stderr should appear

---

## Verification

```bash
cargo build -p agentos-tools
cargo clippy -p agentos-tools -- -D warnings
cargo test -p agentos-tools

# Runtime: submit a task that reads a file
RUST_LOG=agentos_tools=debug agentctl task run --agent mock --goal "read file /etc/hostname" 2>&1 | grep "file_editor"
# Expected: DEBUG lines with path and byte counts
```

---

## Related

- [[Logging Observability Plan]] — master plan
- [[01-span-instrumentation]] — prerequisite (span context that tools inherit)
- [[04-production-structured-logging]] — Phase 4 (JSON output of these tool logs)
