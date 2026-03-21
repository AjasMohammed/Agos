---
title: Sandbox Concurrency Semaphore
tags:
  - sandbox
  - kernel
  - v3
  - plan
date: 2026-03-21
status: planned
effort: 4h
priority: critical
---

# Sandbox Concurrency Semaphore

> Add a `tokio::sync::Semaphore` to `SandboxExecutor` to limit concurrent sandbox children, and set `RAYON_NUM_THREADS=1` in the child environment to prevent thread pool exhaustion.

---

## Why This Phase

Even with trust-aware dispatch (Phase 02) routing Core tools in-process, Community and Verified tools still run in sandbox children. When multiple such tools execute in parallel, each child spawns `num_cpus` rayon threads, exhausting OS thread limits and triggering `EAGAIN` panics. This phase solves the root cause in two ways:

1. **Semaphore**: Limits the number of simultaneous sandbox children to `max_concurrent_sandbox_children` (default: num_cpus). Excess calls queue and wait for a permit rather than spawning unbounded children.
2. **RAYON_NUM_THREADS=1**: Each sandbox child executes a single tool -- it does not need a multi-threaded rayon pool. Setting this env var forces rayon to use exactly one thread, eliminating the thread multiplication problem at its source.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `SandboxExecutor` struct | Two fields: `data_dir`, `executable_path` | Three fields: adds `concurrency_semaphore: Arc<Semaphore>` |
| `SandboxExecutor::new()` | Takes `PathBuf` | Takes `PathBuf` and `max_concurrent: usize` |
| `SandboxExecutor::with_executable()` | Takes two `PathBuf`s | Takes two `PathBuf`s and `max_concurrent: usize` |
| `SandboxExecutor::spawn()` | Spawns child immediately | Acquires semaphore permit first, then spawns |
| Child env vars | `PATH`, `HOME`, `LANG` only | Also sets `RAYON_NUM_THREADS=1` |
| Kernel boot | `SandboxExecutor::new(data_dir)` | `SandboxExecutor::new(data_dir, config.kernel.max_concurrent_sandbox_children)` |

## What to Do

1. Open `crates/agentos-sandbox/src/executor.rs`

2. Add the import at the top:
```rust
use std::sync::Arc;
use tokio::sync::Semaphore;
```

3. Modify the `SandboxExecutor` struct:

```rust
pub struct SandboxExecutor {
    /// Working directory for tool execution.
    data_dir: PathBuf,
    /// Optional override for the sandbox child executable.
    executable_path: Option<PathBuf>,
    /// Limits concurrent sandbox child processes to prevent thread/memory exhaustion.
    concurrency_semaphore: Arc<Semaphore>,
}
```

4. Update constructors:

```rust
impl SandboxExecutor {
    /// Create a new sandbox executor with the given data directory.
    ///
    /// `max_concurrent` limits the number of simultaneous sandbox child
    /// processes. When all permits are held, additional `spawn()` calls
    /// block until a child exits and releases its permit.
    pub fn new(data_dir: PathBuf, max_concurrent: usize) -> Self {
        Self {
            data_dir,
            executable_path: None,
            concurrency_semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
        }
    }

    /// Create a sandbox executor that launches a specific executable.
    pub fn with_executable(
        data_dir: PathBuf,
        executable_path: PathBuf,
        max_concurrent: usize,
    ) -> Self {
        Self {
            data_dir,
            executable_path: Some(executable_path),
            concurrency_semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
        }
    }
```

5. In the `spawn()` method, acquire a semaphore permit at the very beginning (before writing the request file):

```rust
pub async fn spawn(
    &self,
    request: SandboxExecRequest,
    config: &SandboxConfig,
    timeout: Duration,
    category_overhead_bytes: u64,
) -> Result<SandboxResult, AgentOSError> {
    // Acquire a concurrency permit. This blocks if max_concurrent children
    // are already running. The permit is released when `_permit` is dropped
    // at the end of this scope (after child exit or timeout kill).
    let _permit = self
        .concurrency_semaphore
        .acquire()
        .await
        .map_err(|_| AgentOSError::SandboxSpawnFailed {
            reason: "Sandbox concurrency semaphore closed".to_string(),
        })?;

    let start = Instant::now();
    // ... rest of existing spawn() body unchanged ...
```

The `_permit` variable holds the `SemaphorePermit` and is automatically dropped when `spawn()` returns (whether success, error, or timeout), releasing the slot for the next caller.

6. In the same `spawn()` method, add `RAYON_NUM_THREADS=1` to the child's environment. Find the `cmd.env_clear()` block (currently around line 171) and add the env var:

```rust
cmd.env_clear()
    .env("PATH", "/usr/bin:/bin")
    .env("HOME", &self.data_dir)
    .env("LANG", "C.UTF-8")
    .env("RAYON_NUM_THREADS", "1");
```

7. Open `crates/agentos-kernel/src/kernel.rs` and find where `SandboxExecutor` is constructed. Update the constructor call to pass the concurrency limit from config:

Search for `SandboxExecutor::new(` in `kernel.rs`. The call will look something like:
```rust
let sandbox = Arc::new(SandboxExecutor::new(data_dir.clone()));
```

Change to:
```rust
let sandbox = Arc::new(SandboxExecutor::new(
    data_dir.clone(),
    config.kernel.max_concurrent_sandbox_children,
));
```

If `SandboxExecutor::with_executable()` is used elsewhere (e.g., in tests), update those call sites too to add the `max_concurrent` parameter.

8. Search for any other callers of `SandboxExecutor::new` or `SandboxExecutor::with_executable` across the workspace and update them:

```bash
grep -rn "SandboxExecutor::new\|SandboxExecutor::with_executable" crates/
```

Common locations: test helpers, agent-tester harness. Update all call sites.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-sandbox/src/executor.rs` | Add `concurrency_semaphore` field, update constructors, add permit acquisition in `spawn()`, add `RAYON_NUM_THREADS=1` env var |
| `crates/agentos-kernel/src/kernel.rs` | Update `SandboxExecutor::new()` call to pass `max_concurrent_sandbox_children` |
| Any test files that construct `SandboxExecutor` | Add `max_concurrent` parameter to constructor calls |

## Prerequisites

[[01-execution-policy-config]] must be complete -- `max_concurrent_sandbox_children` must exist in `KernelSettings`.

Phase 02 is NOT required -- this phase can be done in parallel with Phase 02.

## Test Plan

- **Unit test `test_sandbox_executor_new_with_concurrency`**: Construct `SandboxExecutor::new(dir, 4)`, verify it compiles and the struct is valid. (The semaphore is internal, so we verify through the API.)

- **Unit test `test_sandbox_executor_with_executable_and_concurrency`**: Same for `with_executable(dir, exe, 4)`.

- **Verify existing tests compile**: All tests in `crates/agentos-sandbox/src/executor.rs` that construct `SandboxExecutor` must be updated with the new parameter and still pass.

- **Integration test `test_concurrent_sandbox_children_limited`** (optional, if feasible): Spawn 10 sandbox children with `max_concurrent = 2`. Verify that at most 2 run simultaneously by checking wall-clock timing (10 sequential at ~50ms each should take ~250ms, vs ~50ms if all parallel).

- **Verify RAYON_NUM_THREADS is set**: In the existing spawn tests or a new test, verify the env var is set on the child command. Since we cannot easily inspect `tokio::process::Command` internals, this is best verified via a manual test or by checking that the thread exhaustion error no longer occurs under load.

## Verification

```bash
cargo build -p agentos-sandbox
cargo test -p agentos-sandbox -- --nocapture
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- --nocapture
cargo clippy --workspace -- -D warnings
```
