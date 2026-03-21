---
title: "Phase 03: Per-Category RLIMIT_AS Formula"
tags:
  - kernel
  - sandbox
  - security
  - v3
  - plan
date: 2026-03-21
status: complete
effort: 4h
priority: critical
---

# Phase 03: Per-Category RLIMIT_AS Formula

> Replace the one-size-fits-all RLIMIT_AS formula (parent VmSize / 2, 1 GiB floor) with per-category baselines that make tool manifest resource declarations meaningful.

---

## Why This Phase

After Phase 02, sandbox children no longer eagerly initialize all 35+ tools. But the RLIMIT_AS formula in `SandboxExecutor::spawn()` still uses the parent kernel's VmSize as the baseline, which is typically 2-4 GB. This means even a `datetime` tool (4 MB declared) gets 1+ GiB of address space.

With per-category baselines, a stateless tool like `datetime` gets `192 MB + 4 MB = 196 MB` instead of `1 GiB + 4 MB`. This keeps the manifest's `max_memory_mb` meaningful while leaving enough address-space headroom for the dynamically linked `agentctl` binary to map its shared libraries.

---

## Before -> Implemented State

| Aspect | Current | Target |
|--------|---------|--------|
| RLIMIT_AS formula | `(parent_vmsize / 2).max(1 GiB) + manifest.max_memory_bytes` | `config.rlimit_as_bytes(category_overhead_bytes)` |
| Baseline for stateless | ~1 GiB (parent vmsize / 2) | 192 MB |
| Baseline for memory tools | ~1 GiB | 768 MB (embedder ONNX + 3 SQLite mmap regions) |
| Baseline for network tools | ~1 GiB | 256 MB (TLS + connection pools) |
| Baseline for HAL tools | ~1 GiB | 192 MB |
| `SANDBOX_AS_MINIMUM` | 1 GiB | Removed (replaced by category baselines) |
| `read_self_vmsize_bytes()` | Used for baseline calculation | Removed (no longer needed) |
| Category source | Not available | Determined in `Kernel::sandbox_plan_for_tool()` via `tool_category()` |

---

## What Was Implemented

The final implementation ended up slightly cleaner than the original proposal: the kernel extracts a reusable `sandbox_plan_for_tool()` helper, and the sandbox crate owns the RLIMIT_AS arithmetic via `SandboxConfig::rlimit_as_bytes()`.

### 1. Compute a reusable sandbox plan in `task_executor.rs`

In `crates/agentos-kernel/src/task_executor.rs`, the kernel now determines whether a tool should run in the lightweight sandbox and computes the per-category overhead alongside the `SandboxConfig`:

```rust
fn sandbox_overhead_for_category(category: ToolCategory) -> u64 {
    match category {
        ToolCategory::Stateless => SandboxConfig::OVERHEAD_STATELESS,
        ToolCategory::Memory => SandboxConfig::OVERHEAD_MEMORY,
        ToolCategory::Network => SandboxConfig::OVERHEAD_NETWORK,
        ToolCategory::Hal => SandboxConfig::OVERHEAD_HAL,
    }
}

async fn sandbox_plan_for_tool(&self, tool_name: &str) -> Option<(SandboxConfig, u64)> {
    let registry = self.tool_registry.read().await;
    let tool = registry.get_by_name(tool_name)?;

    if tool.manifest.executor.executor_type != ExecutorType::Inline {
        return None;
    }

    let category = agentos_tools::tool_category(tool_name)?;
    let config = SandboxConfig::from_manifest(&tool.manifest.sandbox);
    let overhead_bytes = Self::sandbox_overhead_for_category(category);
    Some((config, overhead_bytes))
}
```

Both the parallel and sequential tool-execution paths now call `sandbox.spawn(request, &config, timeout, category_overhead_bytes)` using that helper output.

### 2. Update `SandboxExecutor::spawn()` to accept `SandboxExecRequest`

The sandbox executor signature was updated to accept the fully typed request struct rather than separate tool name and payload arguments:

```rust
pub async fn spawn(
    &self,
    request: SandboxExecRequest,
    config: &SandboxConfig,
    timeout: Duration,
    category_overhead_bytes: u64,
) -> Result<SandboxResult, AgentOSError> {
```

### 3. Move the RLIMIT_AS formula into `SandboxConfig`

Instead of keeping the address-space arithmetic inline in `executor.rs`, the current code centralizes it in `crates/agentos-sandbox/src/config.rs`:

```rust
/// Per-category process overhead baselines for RLIMIT_AS calculation.
/// These represent the minimum virtual address space a sandbox child needs
/// beyond the tool's declared max_memory_mb, accounting for:
/// - Rust runtime + tokio current_thread
/// - Shared library mappings (including OpenSSL/libcrypto in the CLI binary)
/// - Category-specific dependencies
impl SandboxConfig {
    /// Overhead for stateless tools (datetime, think, file-*).
    /// Rust runtime + dynamic library mappings.
    pub const OVERHEAD_STATELESS: u64 = 192 * 1024 * 1024;   // 192 MB

    /// Overhead for memory tools (memory-search, archival-*, procedure-*).
    /// Includes ONNX runtime for fastembed (~200 MB VM), 3 SQLite mmap regions.
    pub const OVERHEAD_MEMORY: u64 = 768 * 1024 * 1024;      // 768 MB

    /// Overhead for network tools (http-client, web-fetch).
    /// Includes rustls TLS provider, connection pool buffers.
    pub const OVERHEAD_NETWORK: u64 = 256 * 1024 * 1024;     // 256 MB

    /// Overhead for HAL tools (hardware-info, sys-monitor, process-manager).
    /// Lightweight system info gathering.
    pub const OVERHEAD_HAL: u64 = 192 * 1024 * 1024;         // 192 MB

    /// Default overhead when category is unknown (generous fallback).
    pub const OVERHEAD_DEFAULT: u64 = 512 * 1024 * 1024;     // 512 MB

    /// Compute the RLIMIT_AS value by combining the tool's declared budget with
    /// the baseline overhead needed to start a child process in this category.
    pub const fn rlimit_as_bytes(&self, category_overhead_bytes: u64) -> u64 {
        self.max_memory_bytes.saturating_add(category_overhead_bytes)
    }
}
```

`SandboxExecutor::spawn()` now calls that helper directly:

```rust
let max_memory = config.rlimit_as_bytes(category_overhead_bytes);
```

### 4. Remove the old parent-`VmSize` baseline logic

The original `read_self_vmsize_bytes()` helper and `SANDBOX_AS_MINIMUM` floor are gone. Per-category constants now define the minimum startup headroom explicitly.

### 5. Update tests and call sites

Integration tests that invoke `spawn()` directly now pass `category_overhead_bytes`, and the sandbox config tests cover the helper formula.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-sandbox/src/executor.rs` | Add `category_overhead_bytes` param to `spawn()`, remove `read_self_vmsize_bytes()`, update RLIMIT_AS formula |
| `crates/agentos-sandbox/src/config.rs` | Add `OVERHEAD_*` constants |
| `crates/agentos-kernel/src/task_executor.rs` | Pass category overhead to `sandbox.spawn()` (both call sites) |

---

## Prerequisites

[[02-sandbox-child-lazy-init]] must be complete so the child actually initializes less, making the tighter RLIMIT_AS safe.

---

## Test Plan

- `cargo build --workspace` must succeed
- `cargo test -p agentos-sandbox` must pass (updated spawn signature)
- `cargo test -p agentos-kernel` must pass
- Manual test: run `datetime` tool in sandbox, verify via `/proc/<child_pid>/status` that VmSize is under 200 MB (was >1 GiB)
- Manual test: run `memory-search` tool in sandbox, verify it still has enough memory to load embedder
- Verify that the `Sandbox child spawned` log now shows `rlimit_as_mb` matching the per-category formula:
  - `datetime`: `rlimit_as_mb = 196` (192 + 4)
  - `memory-search`: `rlimit_as_mb = 896` (768 + 128)
  - `web-fetch`: `rlimit_as_mb = 320` (256 + 64)

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-sandbox
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Verified in the current branch with:

```bash
cargo test -p agentos-sandbox
cargo test -p agentos-kernel
cargo clippy -p agentos-sandbox -p agentos-kernel -- -D warnings
```
