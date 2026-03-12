---
title: Code Safety Hardening
tags:
  - safety
  - v3
  - plan
date: 2026-03-12
status: complete
effort: 4h
priority: critical
---

# Code Safety Hardening

> Eliminate runtime panic paths and lock-poisoning vulnerabilities that can crash the kernel in production.

## Why this phase

Quality gates (fmt, clippy) check style and common mistakes, but they do not catch `panic!()` in production code paths or `RwLock` poisoning cascades. These are harder deployment blockers than formatting — a single bad message can bring down the entire kernel. This phase must run before any other deployment work.

## Current -> Target state

- **Current:** `panic!()` reachable via normal agent message flow; `RwLock::write().unwrap()` in capability engine means one poisoned lock crashes all subsequent permission checks; 6 CLI integration tests hang indefinitely.
- **Target:** All production code paths return `Result` instead of panicking; locks recover from poisoning; integration tests either pass or are marked `#[ignore]` with a tracking issue.

## Detailed subtasks

### 1. Remove `panic!()` from `agent_message_bus.rs`

**File:** `crates/agentos-kernel/src/agent_message_bus.rs:458`

The `send_direct_signed()` method contains:
```rust
panic!("Expected text message")
```
This triggers when any agent sends a non-text message content type. Replace with:
```rust
return Err(AgentOSError::InvalidInput("Expected text message for signing".into()));
```

### 2. Fix RwLock poisoning in `agentos-capability`

**Files:**
- `crates/agentos-capability/src/engine.rs` (lines 82, 92, 101, 107)
- `crates/agentos-capability/src/profiles.rs` (lines 38, 59, 72, 78)

Every `.write().unwrap()` and `.read().unwrap()` on `RwLock` must use poison recovery:
```rust
// Before (crashes on poisoned lock):
let mut map = self.agent_permissions.write().unwrap();

// After (recovers from poison):
let mut map = self.agent_permissions.write().unwrap_or_else(|e| {
    tracing::warn!("Recovered from poisoned lock in capability engine");
    e.into_inner()
});
```

Apply this pattern to all 8 call sites across both files.

### 3. Fix RwLock poisoning in `agentos-hal`

**File:** `crates/agentos-hal/src/registry.rs` (lines 63, 91)

Same pattern as capability engine — replace `.write().unwrap()` and `.read().unwrap()` with poison-recovering variants.

### 4. Fix or skip hanging CLI integration tests

**File:** `crates/agentos-cli/tests/integration_test.rs`

6 integration tests hang because they require a running kernel/bus. Options:
- Add `#[ignore]` with `// Requires running kernel` comment
- Or fix test harness to spawn a kernel in-process

Recommended: `#[ignore]` for now, with a tracking note in `obsidian-vault/roadmap/Issues and Fixes.md`.

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/agent_message_bus.rs` | Confirmed documented production `panic!()` path already removed (test-only panic remains) |
| `crates/agentos-capability/src/engine.rs` | Poison-recovering lock access (4 sites) |
| `crates/agentos-capability/src/profiles.rs` | Poison-recovering lock access (4 sites) |
| `crates/agentos-hal/src/registry.rs` | Poison-recovering lock access (all read/write lock call sites) |
| `crates/agentos-cli/tests/integration_test.rs` | Mark hanging tests `#[ignore]` |
| `obsidian-vault/roadmap/Issues and Fixes.md` | Track integration test debt |

## Dependencies

- **Requires:** none — this is the first phase.
- **Blocks:** [[01-quality-gates-stabilization]], [[02-production-config-baseline]], [[03-containerization-and-runtime]], [[04-security-gate-closure]], [[05-release-process-and-cutover]].

## Test plan

- After each fix, run `cargo test -p <affected-crate>` to confirm no regressions.
- Verify the panic path is gone: search for `panic!` in non-test code and confirm zero hits.
- Verify lock recovery: intentionally poison a lock in a unit test and confirm the engine recovers.

## Verification

```bash
# No panics in production code
grep -rn 'panic!' crates/ --include='*.rs' | grep -v '#\[cfg(test)\]' | grep -v '#\[test\]' | grep -v 'tests/'

# All crates build and test
cargo test --workspace

# Specifically test affected crates
cargo test -p agentos-kernel
cargo test -p agentos-capability
cargo test -p agentos-hal
```

## Related

- [[First Deployment Readiness Plan]]
- [[01-quality-gates-stabilization]]

## Implementation Notes (2026-03-12)

- `agent_message_bus.rs` no longer has the documented production `panic!()` path; remaining `panic!()` usage in that file is test-only.
- `RwLock` poisoning recovery was added to capability engine/profile manager and hardware registry lock access.
- CLI integration tests in `agentos-cli` were marked `#[ignore]` with tracking in [[Issues and Fixes]].
