---
title: Sandbox Execution Policy
tags:
  - kernel
  - sandbox
  - v3
  - next-steps
date: 2026-03-21
status: complete
effort: 5d
priority: critical
---

# Sandbox Execution Policy

> Eliminate fork+exec overhead for Core tools by routing them in-process via ToolRunner, while keeping Community/Verified tools sandboxed with concurrency control.

---

## Current State

Every Inline tool with a known `ToolCategory` is forked into a sandbox child process regardless of trust tier. Core tools (our code, distribution-trusted) pay 30-430ms per call for fork+exec+seccomp+runtime bootstrap. Parallel memory tools exhaust OS thread limits via unbounded rayon thread pools.

## Goal / Target State

- Core tools execute in-process via the kernel's `ToolRunner` (shared `Arc<Embedder>`, no fork overhead)
- Community/Verified tools remain sandboxed (no security regression)
- Configurable via `sandbox_policy` in `config/default.toml` (`trust_aware` | `always` | `never`)
- Sandbox children limited by `tokio::sync::Semaphore` to prevent thread exhaustion
- `RAYON_NUM_THREADS=1` set in sandbox child env to prevent thread multiplication

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[34-01-Add SandboxPolicy Config]] | `config.rs`, `default.toml` | complete |
| 02 | [[34-02-Trust-Aware Dispatch]] | `task_executor.rs` | complete |
| 03 | [[34-03-Sandbox Concurrency Semaphore]] | `executor.rs`, `kernel.rs` | complete |
| 04 | [[34-04-In-Process Safety Hardening]] | `task_executor.rs` | complete |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

## Related

[[Sandbox Execution Policy Plan]], [[Sandbox Lightweight Execution Plan]], [[33-Sandbox Lightweight Execution]]
