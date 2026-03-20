---
title: File Delete Tool
tags: [tools, file-io]
date: 2026-03-18
status: complete
effort: 30m
priority: high
---

# File Delete Tool

> Implement `file-delete`: remove a file within data_dir with write-lock protection.

## What to Do

1. Create `crates/agentos-tools/src/file_delete.rs`
2. Input: `{ "path": string }`
3. Canonicalize path, enforce data_dir containment (file must exist for canonicalize)
4. Acquire write lock via `WriteLockGuard::acquire`
5. `tokio::fs::remove_file(&canonical)` — returns clear error if not found
6. Return `{ path, success: true }`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/file_delete.rs` | New — `FileDelete` struct + `AgentTool` impl |
| `crates/agentos-tools/src/lib.rs` | Add module + pub use |
| `crates/agentos-tools/src/runner.rs` | Register `FileDelete::new()` |
| `tools/core/file-delete.toml` | Manifest, `fs.user_data:w` |

## Prerequisites

None.

## Verification

`cargo test -p agentos-tools -- test_file_delete`
