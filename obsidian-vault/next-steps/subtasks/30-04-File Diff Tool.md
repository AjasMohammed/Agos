---
title: 30-04 File Diff Tool
tags:
  - tools
  - file
  - next-steps
  - subtask
date: 2026-03-18
status: planned
effort: 3h
priority: medium
---

# 30-04 — File Diff Tool

> Add `file-diff`: compute a unified diff between two files within `data_dir` so agents can compare versions without a `shell-exec diff` workaround.

---

## Why This Phase

Agents editing files with `file-editor` (exact string replace) need to verify their changes. Currently the only option is `shell-exec diff`, which requires `process.exec:x` permission — far broader than needed. `file-diff` requires only `fs.user_data:r`.

---

## Current → Target State

| Capability | Current | Target |
|-----------|---------|--------|
| Diff two files | `shell-exec diff a b` (requires process.exec:x) | `file-diff` (requires only fs.user_data:r) |
| Diff strings | not possible | `file-diff` with `mode: "strings"` |

---

## What to Do

### Step 1 — Add `similar` to `crates/agentos-tools/Cargo.toml`

Read `Cargo.toml` first. Add:
```toml
similar = { version = "2", features = ["text"] }
```

`similar` is pure-Rust, MIT/Apache dual-licensed, used by `cargo` itself for diff output.

### Step 2 — Create `crates/agentos-tools/src/file_diff.rs`

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use similar::{ChangeTag, TextDiff};
use std::fmt::Write as FmtWrite;

pub struct FileDiff;

impl FileDiff {
    pub fn new() -> Self { Self }
}

impl Default for FileDiff {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl AgentTool for FileDiff {
    fn name(&self) -> &str { "file-diff" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("fs.user_data", PermissionOp::Read) {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".to_string(),
                operation: "Read".to_string(),
            });
        }

        let mode = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("files");

        let (text_a, text_b, label_a, label_b) = match mode {
            "strings" => {
                let a = payload.get("text_a").and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::SchemaValidation(
                        "file-diff mode=strings requires 'text_a'".into()
                    ))?.to_string();
                let b = payload.get("text_b").and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::SchemaValidation(
                        "file-diff mode=strings requires 'text_b'".into()
                    ))?.to_string();
                (a, b, "a".to_string(), "b".to_string())
            }
            _ => {
                // mode = "files" (default)
                let path_a = payload.get("file_a").and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::SchemaValidation(
                        "file-diff requires 'file_a'".into()
                    ))?;
                let path_b = payload.get("file_b").and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::SchemaValidation(
                        "file-diff requires 'file_b'".into()
                    ))?;

                // Path traversal guard
                for p in [path_a, path_b] {
                    if p.contains("..") {
                        return Err(AgentOSError::PermissionDenied {
                            resource: p.to_string(),
                            operation: "Read (path traversal blocked)".to_string(),
                        });
                    }
                }

                let full_a = context.data_dir.join(path_a);
                let full_b = context.data_dir.join(path_b);

                // Confirm both paths are within data_dir
                let canon_a = full_a.canonicalize().map_err(|_| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-diff".into(),
                    reason: format!("file_a not found: {}", path_a),
                })?;
                let canon_b = full_b.canonicalize().map_err(|_| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-diff".into(),
                    reason: format!("file_b not found: {}", path_b),
                })?;

                let data_dir_canon = context.data_dir.canonicalize().unwrap_or(context.data_dir.clone());
                if !canon_a.starts_with(&data_dir_canon) || !canon_b.starts_with(&data_dir_canon) {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "file outside data_dir".to_string(),
                        operation: "Read".to_string(),
                    });
                }

                let a = tokio::fs::read_to_string(&canon_a).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "file-diff".into(),
                        reason: format!("Read failed for file_a: {}", e),
                    }
                })?;
                let b = tokio::fs::read_to_string(&canon_b).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "file-diff".into(),
                        reason: format!("Read failed for file_b: {}", e),
                    }
                })?;

                (a, b, path_a.to_string(), path_b.to_string())
            }
        };

        let context_lines = payload.get("context_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as usize;

        let diff = TextDiff::from_lines(&text_a, &text_b);
        let mut unified = String::new();
        let _ = writeln!(unified, "--- {}", label_a);
        let _ = writeln!(unified, "+++ {}", label_b);

        for group in diff.grouped_ops(context_lines) {
            for op in &group {
                for change in diff.iter_inline_changes(op) {
                    let prefix = match change.tag() {
                        ChangeTag::Delete => "-",
                        ChangeTag::Insert => "+",
                        ChangeTag::Equal  => " ",
                    };
                    let _ = write!(unified, "{}", prefix);
                    for (_, value) in change.iter_strings_lossy() {
                        let _ = write!(unified, "{}", value);
                    }
                    if change.missing_newline() {
                        let _ = writeln!(unified);
                    }
                }
            }
        }

        let is_identical = diff.ratio() >= 1.0;

        Ok(serde_json::json!({
            "label_a": label_a,
            "label_b": label_b,
            "identical": is_identical,
            "diff": unified,
            "similarity_ratio": diff.ratio(),
        }))
    }
}
```

### Step 3 — Register in `lib.rs`

```rust
pub mod file_diff;
pub use file_diff::FileDiff;
```

### Step 4 — Register in `runner.rs`

```rust
use crate::file_diff::FileDiff;
// In file tool registration block (near FileReader, FileWriter, etc.):
runner.register(Box::new(FileDiff::new()));
```

### Step 5 — Create `tools/core/file-diff.toml`

```toml
[manifest]
name        = "file-diff"
version     = "1.0.0"
description = "Compute a unified diff between two files (mode=files) or two inline strings (mode=strings). Returns diff text and similarity ratio."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["fs.user_data:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "FileDiffIntent"
output = "FileDiffResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 64
max_cpu_ms    = 5000
syscalls      = []
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/Cargo.toml` | Add `similar = { version = "2", features = ["text"] }` |
| `crates/agentos-tools/src/file_diff.rs` | Create |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod file_diff;` and re-export |
| `crates/agentos-tools/src/runner.rs` | Register `FileDiff` |
| `tools/core/file-diff.toml` | Create |

---

## Prerequisites

None — independent of all other phases.

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- file_diff
```

Unit tests:
```rust
#[tokio::test]
async fn diff_identical_strings() {
    let tool = FileDiff::new();
    let result = tool.execute(serde_json::json!({
        "mode": "strings",
        "text_a": "hello\nworld\n",
        "text_b": "hello\nworld\n",
    }), ctx()).await.unwrap();
    assert_eq!(result["identical"], true);
}

#[tokio::test]
async fn diff_changed_strings_produces_output() {
    let tool = FileDiff::new();
    let result = tool.execute(serde_json::json!({
        "mode": "strings",
        "text_a": "hello\nworld\n",
        "text_b": "hello\nearth\n",
    }), ctx()).await.unwrap();
    assert_eq!(result["identical"], false);
    assert!(result["diff"].as_str().unwrap().contains("-world"));
    assert!(result["diff"].as_str().unwrap().contains("+earth"));
}

#[tokio::test]
async fn diff_rejects_path_traversal() {
    let tool = FileDiff::new();
    let result = tool.execute(serde_json::json!({
        "mode": "files",
        "file_a": "../secret",
        "file_b": "normal.txt",
    }), ctx()).await;
    assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
}
```
