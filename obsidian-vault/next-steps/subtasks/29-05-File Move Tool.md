---
title: File Move Tool
tags: [tools, file-io]
date: 2026-03-18
status: complete
effort: 30m
priority: high
---

# File Move Tool

> Implement `file-move`: rename or move a file within data_dir.

## What to Do

1. Create `crates/agentos-tools/src/file_move.rs`
2. Input: `{ "from": string, "to": string }`
3. Source (`from`): canonicalize (must exist), enforce data_dir containment
4. Destination (`to`): normalize lexically (may not exist yet), enforce data_dir containment
5. Acquire write lock on canonical source path
6. Create parent dirs for destination
7. `tokio::fs::rename(&canonical_from, &normalized_to)`
8. Return `{ from, to, success: true }`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/file_move.rs` | New — `FileMove` struct + `AgentTool` impl |
| `crates/agentos-tools/src/lib.rs` | Add module + pub use |
| `crates/agentos-tools/src/runner.rs` | Register `FileMove::new()` |
| `tools/core/file-move.toml` | Manifest, `fs.user_data:w` |

## Prerequisites

None.

## Verification

`cargo test -p agentos-tools -- test_file_move`
