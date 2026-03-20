---
title: File Grep Tool
tags: [tools, file-io]
date: 2026-03-18
status: complete
effort: 2h
priority: high
---

# File Grep Tool

> Implement `file-grep`: recursive regex content search across files with context lines and output modes.

## What to Do

1. Add `regex = { workspace = true }` to `crates/agentos-tools/Cargo.toml`
2. Create `crates/agentos-tools/src/file_grep.rs`
3. Input:
   - `pattern` (required) — regex string
   - `path` (optional, default `.`) — directory within data_dir
   - `glob` (optional) — filename filter, e.g. `*.rs`
   - `context_lines` (optional, default 0) — lines before/after each match
   - `output_mode` (optional) — `"content"` | `"files_with_matches"` | `"count"` (default `"files_with_matches"`)
   - `max_results` (optional, default 50) — max number of matching files
   - `case_insensitive` (optional, default false)
4. Compile regex with `regex::RegexBuilder`; return `SchemaValidation` error on invalid pattern
5. Canonicalize search root, enforce data_dir containment
6. Walk directory recursively in `spawn_blocking` using a VecDeque queue
7. For each file: match filename against glob filter (using `glob::Pattern`) if provided
8. Read text files, run regex, collect match lines with optional context
9. Return structured JSON: matches array with file path, line number, matched line, context

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/Cargo.toml` | Add `regex = { workspace = true }` |
| `crates/agentos-tools/src/file_grep.rs` | New — `FileGrep` struct + `AgentTool` impl |
| `crates/agentos-tools/src/lib.rs` | Add module + pub use |
| `crates/agentos-tools/src/runner.rs` | Register `FileGrep::new()` |
| `tools/core/file-grep.toml` | Manifest, `fs.user_data:r` |

## Prerequisites

None — independent of other subtasks.

## Verification

`cargo test -p agentos-tools -- test_file_grep`
