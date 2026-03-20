---
title: File Operations Expansion
tags:
  - tools
  - file-io
  - next-steps
date: 2026-03-18
status: in-progress
effort: 3d
priority: high
---

# File Operations Expansion

> Add 5 missing file tools (editor, glob, grep, delete, move) to bring AgentOS file I/O to parity with Claude Code's tool surface.

---

## Current State

Only two file tools exist: `file-reader` (read + flat list) and `file-writer` (write/append/create_only). No precise editing, no pattern-based discovery, no content search, no delete, no move/rename.

## Goal / Target State

5 new tools complete the file I/O surface:
- `file-editor` — exact string replacement (Edit equivalent)
- `file-glob` — recursive pattern-based file discovery (Glob equivalent)
- `file-grep` — regex content search across files (Grep equivalent)
- `file-delete` — delete a file within data_dir
- `file-move` — rename/move a file within data_dir

## Sub-tasks

| # | Task | File | Status |
|---|------|------|--------|
| 01 | [[29-01-File Editor Tool]] | `crates/agentos-tools/src/file_editor.rs` | complete |
| 02 | [[29-02-File Glob Tool]] | `crates/agentos-tools/src/file_glob.rs` | complete |
| 03 | [[29-03-File Grep Tool]] | `crates/agentos-tools/src/file_grep.rs` | complete |
| 04 | [[29-04-File Delete Tool]] | `crates/agentos-tools/src/file_delete.rs` | complete |
| 05 | [[29-05-File Move Tool]] | `crates/agentos-tools/src/file_move.rs` | complete |

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/Cargo.toml` | Add `glob = "0.3"`, `regex` from workspace |
| `crates/agentos-tools/src/file_editor.rs` | New tool |
| `crates/agentos-tools/src/file_glob.rs` | New tool |
| `crates/agentos-tools/src/file_grep.rs` | New tool |
| `crates/agentos-tools/src/file_delete.rs` | New tool |
| `crates/agentos-tools/src/file_move.rs` | New tool |
| `crates/agentos-tools/src/lib.rs` | Add 5 module declarations + pub use |
| `crates/agentos-tools/src/runner.rs` | Register 5 new tools |
| `tools/core/file-editor.toml` | New manifest |
| `tools/core/file-glob.toml` | New manifest |
| `tools/core/file-grep.toml` | New manifest |
| `tools/core/file-delete.toml` | New manifest |
| `tools/core/file-move.toml` | New manifest |

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools
cargo clippy -p agentos-tools -- -D warnings
cargo fmt --all -- --check
```
