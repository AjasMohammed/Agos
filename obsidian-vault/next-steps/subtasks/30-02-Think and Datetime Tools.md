---
title: 30-02 Think and Datetime Tools
tags:
  - tools
  - cognition
  - next-steps
  - subtask
date: 2026-03-18
status: planned
effort: 4h
priority: high
---

# 30-02 — Think and Datetime Tools

> Add two foundational tools: `think` for explicit reasoning (audit-logged scratchpad) and `datetime` for time-awareness.

---

## Why This Phase

Production agentic systems give agents a named `think` tool so reasoning steps appear in the audit trail rather than only in the LLM's internal token stream. Without it, agents either skip explicit planning (degraded quality) or chain dummy tool calls.

`datetime` eliminates the need for `shell-exec date`, which bypasses the capability model and is fragile (date format varies by locale).

Both tools have zero external dependencies and require no changes to `ToolExecutionContext`.

---

## Current → Target State

| Tool | Current | Target |
|------|---------|--------|
| `think` | does not exist | explicit reasoning scratchpad, audit-logged |
| `datetime` | does not exist (agents use `shell-exec date`) | first-class tool, returns UTC + unix timestamp + ISO 8601 |

---

## What to Do

### Step 1 — Create `crates/agentos-tools/src/think.rs`

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct ThinkTool;

impl ThinkTool {
    pub fn new() -> Self { Self }
}

impl Default for ThinkTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl AgentTool for ThinkTool {
    fn name(&self) -> &str { "think" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![] // no permissions required
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let thought = payload
            .get("thought")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("think requires 'thought' field".into())
            })?;

        // The tool is a deliberate no-op: the ToolRunner already records
        // every tool call + result in the audit log, so the thought is
        // captured at the call boundary without any additional write here.
        Ok(serde_json::json!({
            "acknowledged": true,
            "thought_length": thought.len(),
        }))
    }
}
```

### Step 2 — Create `crates/agentos-tools/src/datetime.rs`

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use chrono::Utc;

pub struct DatetimeTool;

impl DatetimeTool {
    pub fn new() -> Self { Self }
}

impl Default for DatetimeTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl AgentTool for DatetimeTool {
    fn name(&self) -> &str { "datetime" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![] // no permissions required
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let now = Utc::now();
        Ok(serde_json::json!({
            "utc_iso8601": now.to_rfc3339(),
            "unix_timestamp_secs": now.timestamp(),
            "unix_timestamp_millis": now.timestamp_millis(),
            "date": now.format("%Y-%m-%d").to_string(),
            "time": now.format("%H:%M:%S").to_string(),
            "timezone": "UTC",
        }))
    }
}
```

**Note:** `chrono` is already a dependency of `agentos-tools` (used in other tools). No `Cargo.toml` change needed.

### Step 3 — Register in `crates/agentos-tools/src/lib.rs`

Add module declarations and re-exports:
```rust
pub mod datetime;
pub mod think;

pub use datetime::DatetimeTool;
pub use think::ThinkTool;
```

### Step 4 — Register in `crates/agentos-tools/src/runner.rs`

In the `register_memory_tools` method or wherever non-memory tools are registered, add:
```rust
use crate::datetime::DatetimeTool;
use crate::think::ThinkTool;

// In the registration block:
runner.register(Box::new(ThinkTool::new()));
runner.register(Box::new(DatetimeTool::new()));
```

Find the `register` method signature first — read `runner.rs` to confirm it takes `Box<dyn AgentTool>` and inserts into `self.tools` by name.

### Step 5 — Create `tools/core/think.toml`

```toml
[manifest]
name        = "think"
version     = "1.0.0"
description = "Record an explicit reasoning step. Use before any irreversible action to think through the approach. The thought is captured in the audit log."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = []

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "ThinkIntent"
output = "ThinkResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 4
max_cpu_ms    = 100
syscalls      = []
```

### Step 6 — Create `tools/core/datetime.toml`

```toml
[manifest]
name        = "datetime"
version     = "1.0.0"
description = "Return the current UTC date and time as ISO 8601, unix timestamp (seconds and milliseconds), and human-readable date/time strings."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = []

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "DatetimeQuery"
output = "DatetimeResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 4
max_cpu_ms    = 100
syscalls      = []
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/think.rs` | Create |
| `crates/agentos-tools/src/datetime.rs` | Create |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod think; pub mod datetime;` and re-exports |
| `crates/agentos-tools/src/runner.rs` | Register `ThinkTool` and `DatetimeTool` |
| `tools/core/think.toml` | Create |
| `tools/core/datetime.toml` | Create |

---

## Prerequisites

None — no external crate deps, no ToolExecutionContext changes.

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- think datetime
# Confirm tool names appear in runner's tool map
cargo test -p agentos-tools -- tool_runner_registers
```

Add inline unit tests in each file:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::ToolExecutionContext;

    fn ctx() -> ToolExecutionContext { /* minimal test context */ }

    #[tokio::test]
    async fn think_returns_acknowledged() {
        let tool = ThinkTool::new();
        let result = tool.execute(serde_json::json!({"thought": "test"}), ctx()).await.unwrap();
        assert_eq!(result["acknowledged"], true);
    }

    #[tokio::test]
    async fn datetime_returns_utc_fields() {
        let tool = DatetimeTool::new();
        let result = tool.execute(serde_json::json!({}), ctx()).await.unwrap();
        assert!(result["utc_iso8601"].as_str().unwrap().contains("T"));
        assert!(result["unix_timestamp_secs"].as_i64().unwrap() > 0);
    }
}
```
