---
title: Add SandboxPolicy Enum and Config Fields
tags:
  - kernel
  - sandbox
  - v3
  - plan
date: 2026-03-21
status: planned
effort: 3h
priority: critical
---

# Add SandboxPolicy Enum and Config Fields

> Add a `SandboxPolicy` enum and `max_concurrent_sandbox_children` setting to the kernel config so subsequent phases can read the execution policy at runtime.

---

## Why This Phase

This is the foundation for all other phases. The sandbox policy and concurrency limit must exist in the config before `sandbox_plan_for_tool()` (Phase 02) can branch on them or `SandboxExecutor` (Phase 03) can read the concurrency limit. We add the types and config fields first so the rest of the codebase can compile incrementally.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `SandboxPolicy` type | Does not exist | `enum SandboxPolicy { TrustAware, Always, Never }` in `config.rs` |
| `KernelSettings` | No sandbox policy field | Has `sandbox_policy: SandboxPolicy` field |
| `KernelSettings` | No concurrency limit field | Has `max_concurrent_sandbox_children: usize` (default: `num_cpus::get()`) |
| `config/default.toml` | No sandbox section under `[kernel]` | Has `sandbox_policy = "trust_aware"` and `max_concurrent_sandbox_children` |

## What to Do

1. Open `crates/agentos-kernel/src/config.rs`

2. Add the `SandboxPolicy` enum before the `KernelSettings` struct (around line 5):

```rust
/// Controls whether tools are executed in a sandbox child process or in-process.
///
/// - `TrustAware` (default): Core-tier tools run in-process via ToolRunner;
///   Community and Verified tools are sandboxed.
/// - `Always`: Every tool with a sandbox-eligible category runs in a sandbox child
///   (current pre-v3.1 behavior). Use for high-security deployments.
/// - `Never`: No sandboxing at all. Development/testing only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPolicy {
    #[default]
    TrustAware,
    Always,
    Never,
}
```

3. Add two new fields to the `KernelSettings` struct (after the `events` field, around line 69):

```rust
    /// Sandbox execution policy. Controls which tools are sandboxed vs in-process.
    /// Default: `trust_aware` (Core tools run in-process, Community/Verified sandboxed).
    #[serde(default)]
    pub sandbox_policy: SandboxPolicy,
    /// Maximum number of concurrent sandbox child processes. Prevents thread
    /// exhaustion when multiple tools are sandboxed in parallel.
    /// Default: number of logical CPUs on the host.
    #[serde(default = "default_max_concurrent_sandbox_children")]
    pub max_concurrent_sandbox_children: usize,
```

4. Add the default function:

```rust
fn default_max_concurrent_sandbox_children() -> usize {
    num_cpus::get().max(2)
}
```

5. Verify that the `num_cpus` crate is already a dependency of `agentos-kernel`. If not, check if `std::thread::available_parallelism()` can be used instead:

```rust
fn default_max_concurrent_sandbox_children() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(2)
}
```

6. Open `config/default.toml` and add the following lines inside the `[kernel]` section (after the `per_agent_rate_limit` line or after the `[kernel.events]` block):

```toml
# Sandbox execution policy for tool calls.
# "trust_aware" (default): Core tools run in-process, Community/Verified tools sandboxed.
# "always": All sandbox-eligible tools run in sandbox children (legacy behavior).
# "never": No sandboxing (development only, NOT for production).
sandbox_policy = "trust_aware"
# Maximum concurrent sandbox child processes. Prevents thread/memory exhaustion
# when many tools execute in parallel. Default: number of logical CPUs.
# max_concurrent_sandbox_children = 8
```

Note: `max_concurrent_sandbox_children` is commented out in default.toml so it uses the runtime default (`num_cpus` / `available_parallelism`). Users can uncomment and override.

7. Add a validation check in `load_config()` (after `validate_logging_settings`):

```rust
validate_sandbox_settings(&config.kernel)?;
```

And the validation function:

```rust
fn validate_sandbox_settings(kernel: &KernelSettings) -> Result<(), anyhow::Error> {
    if kernel.max_concurrent_sandbox_children == 0 {
        anyhow::bail!(
            "kernel.max_concurrent_sandbox_children must be > 0 (got 0); \
             at least one sandbox child slot is required"
        );
    }
    Ok(())
}
```

8. Update the existing `MINIMAL_TOML` test constant in `config.rs` -- it should continue to parse without the new fields since both have `#[serde(default)]`.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/config.rs` | Add `SandboxPolicy` enum, two fields to `KernelSettings`, default fn, validation fn |
| `config/default.toml` | Add `sandbox_policy` and commented `max_concurrent_sandbox_children` |

## Prerequisites

None -- this is the first phase.

## Test Plan

- **Existing tests pass unchanged**: The `MINIMAL_TOML` test and all other config tests must continue to pass because the new fields use `#[serde(default)]`.
- **Add test `sandbox_policy_defaults_to_trust_aware`**: Parse `MINIMAL_TOML`, assert `config.kernel.sandbox_policy == SandboxPolicy::TrustAware`.
- **Add test `sandbox_policy_parses_always`**: Parse TOML with `sandbox_policy = "always"`, assert `SandboxPolicy::Always`.
- **Add test `sandbox_policy_parses_never`**: Parse TOML with `sandbox_policy = "never"`, assert `SandboxPolicy::Never`.
- **Add test `max_concurrent_sandbox_children_defaults_nonzero`**: Parse `MINIMAL_TOML`, assert `config.kernel.max_concurrent_sandbox_children >= 2`.
- **Add test `max_concurrent_sandbox_children_rejects_zero`**: Write TOML with `max_concurrent_sandbox_children = 0`, call `load_config()`, assert error contains "must be > 0".
- **Add test `sandbox_policy_rejects_unknown_value`**: Parse TOML with `sandbox_policy = "bogus"`, assert deserialization error.

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- config --nocapture
cargo test -p agentos-kernel -- --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
