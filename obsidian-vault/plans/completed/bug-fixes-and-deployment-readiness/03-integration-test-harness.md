---
title: Integration Test Harness
tags:
  - cli
  - kernel
  - v3
  - bugfix
date: 2026-03-13
status: complete
effort: 4h
priority: high
---

# Integration Test Harness

> Remove the `#[ignore]` annotations from the 6 CLI integration tests by building an in-process kernel lifecycle harness that boots, runs the test, and shuts down cleanly via `CancellationToken`.

---

## Why This Phase

Issue #1 from the Issues and Fixes audit was "Integration tests hang indefinitely." The root cause (missing `CancellationToken`) has been fixed -- `Kernel` now has a `cancellation_token` field and all loops use `tokio::select!`. However, the 6 CLI integration tests in `crates/agentos-cli/tests/integration_test.rs` remain marked `#[ignore]` because the test harness has not been updated to use the cancellation token.

These tests cover critical end-to-end scenarios:
- `test_full_lifecycle_with_mock_llm`
- `test_run_task_nonexistent_agent`
- `test_task_with_tool_call`
- `test_resource_list_command`
- `test_cost_report_command`
- `test_escalation_list_command`

Without them running in CI, regressions in the CLI-to-kernel path go undetected.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| 6 integration tests | `#[ignore = "Requires running kernel/bus"]` | Running in CI, pass deterministically |
| Test kernel lifecycle | No shutdown mechanism in tests | `setup_kernel()` returns `Arc<Kernel>` with `CancellationToken`; test calls `kernel.shutdown()` in cleanup |
| Test timeout protection | None -- tests hang forever on failure | `tokio::time::timeout(Duration::from_secs(30), ...)` wraps each test body |
| Bus client connection | Connects to real Unix socket | Same approach, but kernel boots with temp socket path and cleans up |

---

## What to Do

### 1. Update `setup_kernel()` in test common module

Open `crates/agentos-cli/tests/common.rs`.

The `setup_kernel()` function (or equivalent) should:
1. Create a `tempfile::TempDir` for all state (data dir, vault, audit log, socket)
2. Write a temporary config pointing to the temp dir
3. Boot the kernel with `Kernel::boot()`
4. Spawn `kernel.run()` as a background task
5. Return both the `Arc<Kernel>` and a `BusClient` connected to the temp socket
6. The caller can call `kernel.shutdown()` when done

```rust
pub async fn setup_test_kernel() -> (Arc<Kernel>, BusClient, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    // Write temp config...
    let kernel = Arc::new(Kernel::boot(&config_path, "test-passphrase").await.unwrap());
    let kernel_clone = kernel.clone();
    tokio::spawn(async move {
        kernel_clone.run().await.ok();
    });
    // Small delay for bus to start accepting
    tokio::time::sleep(Duration::from_millis(200)).await;
    let client = BusClient::connect(&socket_path).await.unwrap();
    (kernel, client, tmp)
}
```

### 2. Update each test to use the harness

For each of the 6 tests:

1. Remove `#[ignore = "..."]`
2. Wrap the test body in `tokio::time::timeout(Duration::from_secs(30), async { ... })`
3. Call `kernel.shutdown()` at the end (in a `Drop` guard or explicitly)
4. Keep `#[serial]` to avoid socket conflicts

Example pattern:
```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_full_lifecycle_with_mock_llm() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (kernel, mut client, _tmp) = setup_test_kernel().await;
        // ... test body ...
        kernel.shutdown();
    }).await;
    assert!(result.is_ok(), "Test timed out after 30 seconds");
}
```

### 3. Ensure `Kernel::run()` returns when cancelled

Open `crates/agentos-kernel/src/run_loop.rs`.

Verify that `Kernel::run()` returns `Ok(())` when the cancellation token is triggered. The current implementation uses a `JoinSet` with `tokio::select!` -- confirm that when all tasks break out of their loops, the `JoinSet` drains and `run()` returns.

If `run()` currently loops forever waiting for `join_set.join_next()`, add a cancellation check:
```rust
tokio::select! {
    _ = self.cancellation_token.cancelled() => {
        tracing::info!("Kernel shutdown requested");
        break;
    }
    result = join_set.join_next() => {
        // handle task completion/restart
    }
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-cli/tests/common.rs` | Add `setup_test_kernel()` function that boots kernel with temp dirs and returns `(Arc<Kernel>, BusClient, TempDir)` |
| `crates/agentos-cli/tests/integration_test.rs` | Remove `#[ignore]` from 6 tests; wrap bodies in `tokio::time::timeout`; call `kernel.shutdown()` |
| `crates/agentos-kernel/src/run_loop.rs` | Verify `run()` returns on cancellation (may need minor fix) |

---

## Prerequisites

[[01-clippy-ci-gate-fixes]] should be complete first so that clippy passes. Otherwise the test changes may introduce additional lint warnings that block CI.

---

## Test Plan

- Run `cargo test -p agentos-cli` -- all 6 previously-ignored tests must now run and pass
- Run `cargo test -p agentos-cli -- --nocapture` -- verify each test completes within 30 seconds
- Run `cargo test --workspace` -- full workspace test suite must pass
- Verify: no `#[ignore]` annotations remain in `integration_test.rs` (except for tests that genuinely require external services like a running Ollama)

---

## Verification

```bash
# Confirm no ignored tests (except those requiring external services)
grep -c '#\[ignore' crates/agentos-cli/tests/integration_test.rs
# Should return 0

cargo test -p agentos-cli -- --nocapture
cargo test --workspace
```
