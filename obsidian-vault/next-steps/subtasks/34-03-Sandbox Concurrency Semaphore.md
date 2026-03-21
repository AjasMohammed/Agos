---
title: Sandbox Concurrency Semaphore
tags:
  - sandbox
  - kernel
  - v3
  - next-steps
date: 2026-03-21
status: planned
effort: 4h
priority: critical
---

# Sandbox Concurrency Semaphore

> Add a `tokio::sync::Semaphore` to `SandboxExecutor` and set `RAYON_NUM_THREADS=1` in sandbox child environments to prevent thread pool exhaustion.

---

## Why This Subtask

When Community/Verified tools run in sandbox children in parallel, each child initializes rayon with `num_cpus` threads. With 5+ parallel tools, this creates hundreds of threads and triggers `EAGAIN` panics. This subtask caps concurrent children via a semaphore and forces each child to use a single rayon thread.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `SandboxExecutor` fields | `data_dir`, `executable_path` | Adds `concurrency_semaphore: Arc<Semaphore>` |
| `SandboxExecutor::new()` | `fn new(data_dir: PathBuf)` | `fn new(data_dir: PathBuf, max_concurrent: usize)` |
| `SandboxExecutor::with_executable()` | 2 params | 3 params (adds `max_concurrent`) |
| `spawn()` start | Immediate execution | Acquires semaphore permit first |
| Child env | `PATH`, `HOME`, `LANG` | Also `RAYON_NUM_THREADS=1` |

## What to Do

1. Open `crates/agentos-sandbox/src/executor.rs`

2. Add imports:
```rust
use std::sync::Arc;
use tokio::sync::Semaphore;
```

3. Add field to struct:
```rust
pub struct SandboxExecutor {
    data_dir: PathBuf,
    executable_path: Option<PathBuf>,
    concurrency_semaphore: Arc<Semaphore>,
}
```

4. Update `new()`:
```rust
pub fn new(data_dir: PathBuf, max_concurrent: usize) -> Self {
    Self {
        data_dir,
        executable_path: None,
        concurrency_semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
    }
}
```

5. Update `with_executable()`:
```rust
pub fn with_executable(data_dir: PathBuf, executable_path: PathBuf, max_concurrent: usize) -> Self {
    Self {
        data_dir,
        executable_path: Some(executable_path),
        concurrency_semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
    }
}
```

6. Add semaphore acquisition at the start of `spawn()`:
```rust
pub async fn spawn(
    &self,
    request: SandboxExecRequest,
    config: &SandboxConfig,
    timeout: Duration,
    category_overhead_bytes: u64,
) -> Result<SandboxResult, AgentOSError> {
    let _permit = self
        .concurrency_semaphore
        .acquire()
        .await
        .map_err(|_| AgentOSError::SandboxSpawnFailed {
            reason: "Sandbox concurrency semaphore closed".to_string(),
        })?;

    let start = Instant::now();
    let tool_name = request.tool_name.clone();
    // ... rest of existing body unchanged ...
```

7. Add `RAYON_NUM_THREADS=1` to child env (find `cmd.env_clear()` block):
```rust
cmd.env_clear()
    .env("PATH", "/usr/bin:/bin")
    .env("HOME", &self.data_dir)
    .env("LANG", "C.UTF-8")
    .env("RAYON_NUM_THREADS", "1");
```

8. Open `crates/agentos-kernel/src/kernel.rs` and find `SandboxExecutor::new(`. Update:
```rust
// Before:
let sandbox = Arc::new(SandboxExecutor::new(data_dir.clone()));
// After:
let sandbox = Arc::new(SandboxExecutor::new(
    data_dir.clone(),
    config.kernel.max_concurrent_sandbox_children,
));
```

9. Search for all other `SandboxExecutor::new` and `SandboxExecutor::with_executable` call sites:
```bash
grep -rn "SandboxExecutor::new\|SandboxExecutor::with_executable" crates/
```
Update each call site to pass a `max_concurrent` value. For tests, use a reasonable default like `4` or `8`.

10. Update tests in `executor.rs`:
```rust
#[test]
fn test_sandbox_executor_new() {
    let dir = std::env::temp_dir();
    let executor = SandboxExecutor::new(dir.clone(), 4);
    assert_eq!(executor.data_dir(), &dir);
    assert!(executor.executable_path.is_none());
}

#[test]
fn test_sandbox_executor_with_executable_override() {
    let dir = std::env::temp_dir();
    let executable_path = PathBuf::from("/tmp/agentctl-test");
    let executor = SandboxExecutor::with_executable(dir.clone(), executable_path.clone(), 4);
    assert_eq!(executor.data_dir(), &dir);
    assert_eq!(executor.executable_path.as_ref(), Some(&executable_path));
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-sandbox/src/executor.rs` | Add semaphore field, update constructors, acquire permit in `spawn()`, add `RAYON_NUM_THREADS=1` |
| `crates/agentos-kernel/src/kernel.rs` | Pass `max_concurrent_sandbox_children` to `SandboxExecutor::new()` |
| Any test files constructing `SandboxExecutor` | Add `max_concurrent` parameter |

## Prerequisites

[[34-01-Add SandboxPolicy Config]] must be complete for `max_concurrent_sandbox_children` in config.

This subtask can be done in parallel with [[34-02-Trust-Aware Dispatch]].

## Test Plan

- `test_sandbox_executor_new` compiles with new signature
- `test_sandbox_executor_with_executable_override` compiles with new signature
- All existing sandbox tests pass (request file permissions, parse result, etc.)
- `cargo build --workspace` succeeds (no broken call sites)
- Manual: run 10 parallel sandbox tools with `max_concurrent = 2`, verify no `EAGAIN` panic

## Verification

```bash
cargo build -p agentos-sandbox
cargo test -p agentos-sandbox -- --nocapture
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- --nocapture
cargo build --workspace
cargo clippy --workspace -- -D warnings
```
