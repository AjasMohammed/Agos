---
title: File Editor Tool
tags: [tools, file-io]
date: 2026-03-18
status: complete
effort: 2h
priority: high
---

# File Editor Tool

> Implement `file-editor`: exact string replacement on files within data_dir, with write-lock protection across the full read-modify-write cycle.

## What to Do

1. Create `crates/agentos-tools/src/file_editor.rs`
2. Input schema: `{ "path": string, "edits": [{ "old_text": string, "new_text": string }] }`
3. Security: canonicalize + `starts_with(data_dir)` check
4. Acquire write lock via `WriteLockGuard::acquire` before reading (prevents TOCTOU)
5. For each edit: verify `old_text` appears exactly once; return error if not found or ambiguous
6. Apply all replacements sequentially to the in-memory string
7. Atomic write result via tmp+rename
8. Return `{ path, edits_applied, bytes_written, success }`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/file_editor.rs` | New — `FileEditor` struct + `AgentTool` impl |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod file_editor; pub use file_editor::FileEditor;` |
| `crates/agentos-tools/src/runner.rs` | Add `self.register(Box::new(FileEditor::new()));` |
| `tools/core/file-editor.toml` | Manifest with `trust_tier = "core"`, `fs.user_data:w` |

## Prerequisites

None — builds on existing `file_lock.rs` and `file_writer.rs` patterns.

## Verification

`cargo test -p agentos-tools -- test_file_editor`
