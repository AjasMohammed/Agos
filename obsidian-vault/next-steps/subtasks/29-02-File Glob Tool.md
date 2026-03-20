---
title: File Glob Tool
tags: [tools, file-io]
date: 2026-03-18
status: complete
effort: 1h
priority: high
---

# File Glob Tool

> Implement `file-glob`: recursive pattern-based file discovery using `glob` crate, bounded to data_dir.

## What to Do

1. Add `glob = "0.3"` to `crates/agentos-tools/Cargo.toml`
2. Create `crates/agentos-tools/src/file_glob.rs`
3. Input: `{ "pattern": "**/*.rs", "path": "src/" }` — path is optional subdirectory
4. Reject patterns containing `..` or starting with `/`
5. Root the glob pattern: `format!("{}/{}/{}", canonical_data_dir, path, pattern)`
6. Use `glob::glob()` to collect matches in `spawn_blocking`
7. For each match: verify it starts with `canonical_data_dir` (security), compute relative path, get size + mtime metadata
8. Sort by mtime descending (most recently modified first)
9. Return `{ pattern, path, matches: [{path, size_bytes, modified_at, is_dir}], count }`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/Cargo.toml` | Add `glob = "0.3"` |
| `crates/agentos-tools/src/file_glob.rs` | New — `FileGlob` struct + `AgentTool` impl |
| `crates/agentos-tools/src/lib.rs` | Add module + pub use |
| `crates/agentos-tools/src/runner.rs` | Register `FileGlob::new()` |
| `tools/core/file-glob.toml` | Manifest, `fs.user_data:r` |

## Prerequisites

29-01 not required — independent tool.

## Verification

`cargo test -p agentos-tools -- test_file_glob`
