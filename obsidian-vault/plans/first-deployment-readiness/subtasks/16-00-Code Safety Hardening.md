---
title: Code Safety Hardening
tags:
  - safety
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 4h
priority: critical
---

# Code Safety Hardening

> Remove runtime panic paths and RwLock poisoning vulnerabilities that can crash the kernel.

## Why this sub-task

A `panic!()` in `agent_message_bus.rs` is reachable through normal agent messaging. RwLock `.unwrap()` in the capability engine means one poisoned lock cascades into kernel-wide failure. These are harder blockers than formatting — they cause production crashes.

## Current -> Target State

- **Current:** `panic!("Expected text message")` at `agent_message_bus.rs:458`; `.write().unwrap()` on 10 RwLock sites across capability and HAL crates; 6 hanging CLI integration tests.
- **Target:** All production paths return `Result`; locks recover from poisoning; hanging tests are `#[ignore]`d.

## What to Do

1. **Replace panic in agent_message_bus.rs:**
   - Open `crates/agentos-kernel/src/agent_message_bus.rs`
   - Find `panic!("Expected text message")` (~line 458)
   - Replace with `return Err(AgentOSError::InvalidInput("Expected text message for signing".into()));`
   - Ensure the enclosing function signature returns `Result`

2. **Fix RwLock poisoning in capability engine (4 sites):**
   - Open `crates/agentos-capability/src/engine.rs`
   - Lines 82, 92, 101, 107: replace `.write().unwrap()` / `.read().unwrap()` with:
     ```rust
     .write().unwrap_or_else(|e| {
         tracing::warn!("Recovered from poisoned lock in capability engine");
         e.into_inner()
     })
     ```

3. **Fix RwLock poisoning in capability profiles (4 sites):**
   - Open `crates/agentos-capability/src/profiles.rs`
   - Lines 38, 59, 72, 78: same poison-recovery pattern

4. **Fix RwLock poisoning in HAL registry (2 sites):**
   - Open `crates/agentos-hal/src/registry.rs`
   - Lines 63, 91: same poison-recovery pattern

5. **Mark hanging CLI integration tests:**
   - Open `crates/agentos-cli/tests/integration_test.rs`
   - Add `#[ignore]` attribute to the 6 hanging tests
   - Add comment: `// Requires running kernel — tracked in Issues and Fixes.md`

6. **Update issues tracker:**
   - Add entry to `obsidian-vault/roadmap/Issues and Fixes.md` for CLI integration test harness

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/agent_message_bus.rs` | Replace `panic!()` with `Err(...)` |
| `crates/agentos-capability/src/engine.rs` | Poison recovery on 4 lock sites |
| `crates/agentos-capability/src/profiles.rs` | Poison recovery on 4 lock sites |
| `crates/agentos-hal/src/registry.rs` | Poison recovery on 2 lock sites |
| `crates/agentos-cli/tests/integration_test.rs` | `#[ignore]` on 6 hanging tests |
| `obsidian-vault/roadmap/Issues and Fixes.md` | Track integration test debt |

## Prerequisites

None — this is the first sub-task in the deployment readiness program.

## Verification

```bash
# Confirm no panic in production code paths
grep -rn 'panic!' crates/ --include='*.rs' | grep -v 'test' | grep -v 'tests/'

# Build and test affected crates
cargo test -p agentos-kernel
cargo test -p agentos-capability
cargo test -p agentos-hal
cargo test -p agentos-cli --lib

# Full workspace test (hanging tests now ignored)
cargo test --workspace
```

Pass criteria:
- Zero `panic!()` in non-test production code paths in affected files.
- All workspace tests pass (ignored tests excluded).
- Lock recovery confirmed by test or code review.
