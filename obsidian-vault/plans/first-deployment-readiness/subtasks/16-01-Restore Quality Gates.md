---
title: Restore Quality Gates
tags:
  - quality
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 1d
priority: critical
---

# Restore Quality Gates

> Make all required release gates pass and add CI enforcement so deployment candidates are trustworthy.

## Why this sub-task

Current deployment readiness is blocked by failing formatting and clippy gates. Without CI automation, fixes will regress. Shipping with red gates increases regression risk and makes release status ambiguous.

## Current -> Target State

- **Current:** `cargo fmt --all -- --check` fails; `cargo clippy --workspace -- -D warnings` fails (10 errors across 4 crates); no CI workflow.
- **Target:** `fmt`, strict `clippy`, `test`, and `release build` all pass in one clean run; `.github/workflows/release-gate.yml` enforces gates on every push/PR.

## What to Do

1. Run and collect failing diagnostics:
   - `cargo fmt --all -- --check`
   - `cargo clippy --workspace -- -D warnings`
2. Fix clippy blockers in known files:
   - `crates/agentos-sandbox/src/executor.rs` — replace manual ceil division with `.div_ceil()`; replace `std::io::Error::new(ErrorKind::Other, ...)` with `std::io::Error::other(...)`
   - `crates/agentos-sandbox/src/filter.rs` — replace `or_insert_with(Vec::new)` with `or_default()`
   - `crates/agentos-audit/src/log.rs` — reduce `compute_entry_hash` argument count (11 args, max 7) by introducing a struct parameter
   - `crates/agentos-memory/src/episodic.rs` — refactor `record(...)` (8 args, max 7) into typed input struct
   - `crates/agentos-memory/src/types.rs` — implement `std::str::FromStr` trait or rename `from_str` method
   - `crates/agentos-pipeline/src/types.rs` — implement `std::str::FromStr` or rename `from_str` (2 sites: lines 41 and 88)
3. Apply workspace formatting and re-run check:
   - `cargo fmt --all`
   - `cargo fmt --all -- --check`
4. Re-run strict gate sequence:
   - `cargo clippy --workspace -- -D warnings`
   - `cargo test --workspace`
   - `cargo build --workspace --release`
5. **Create CI workflow** (`.github/workflows/release-gate.yml`):
   - Trigger: push to `main`, pull requests
   - Steps: checkout → install Rust stable → `cargo fmt --all -- --check` → `cargo clippy --workspace -- -D warnings` → `cargo test --workspace` → `cargo build --workspace --release`
   - Fail-fast on any step failure
6. Capture final green evidence in release notes/checklist.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-sandbox/src/executor.rs` | Fix clippy `manual_div_ceil` and `io_other_error` |
| `crates/agentos-sandbox/src/filter.rs` | Replace `or_insert_with(Vec::new)` with `or_default()` |
| `crates/agentos-audit/src/log.rs` | Refactor function arguments or use typed struct |
| `crates/agentos-memory/src/episodic.rs` | Reduce argument count via struct/params object |
| `crates/agentos-memory/src/types.rs` | Implement `FromStr` or rename `from_str` |
| `crates/agentos-pipeline/src/types.rs` | Implement `FromStr` or rename `from_str` (2 sites) |
| `.github/workflows/release-gate.yml` | New CI workflow |

## Expected Inputs and Outputs

- **Input:** Current codebase with failing quality checks.
- **Output:** Quality gate report with all required commands passing; CI workflow committed and green.

## Prerequisites

- [[16-00-Code Safety Hardening]]

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

Pass criteria:
- All commands exit with code `0`.
- `.github/workflows/release-gate.yml` exists and passes.
