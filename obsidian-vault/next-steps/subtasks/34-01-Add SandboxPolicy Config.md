---
title: Add SandboxPolicy Config
tags:
  - kernel
  - sandbox
  - v3
  - next-steps
date: 2026-03-21
status: planned
effort: 3h
priority: critical
---

# Add SandboxPolicy Config

> Add a `SandboxPolicy` enum and `max_concurrent_sandbox_children` setting to `KernelSettings` so the execution policy is configurable at runtime.

---

## Why This Subtask

This is the foundation for trust-aware dispatch and concurrency control. The enum and config fields must exist before any other phase can read them. All new fields use `#[serde(default)]` so existing configs parse without changes.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `SandboxPolicy` type | Does not exist | `enum SandboxPolicy { TrustAware, Always, Never }` with `Default = TrustAware` |
| `KernelSettings.sandbox_policy` | Missing | `sandbox_policy: SandboxPolicy` with `#[serde(default)]` |
| `KernelSettings.max_concurrent_sandbox_children` | Missing | `max_concurrent_sandbox_children: usize` defaulting to `available_parallelism()` |
| `config/default.toml` | No sandbox policy | `sandbox_policy = "trust_aware"` under `[kernel]` |
| Config validation | No sandbox validation | Rejects `max_concurrent_sandbox_children = 0` |

## What to Do

1. Open `crates/agentos-kernel/src/config.rs`

2. Add the `SandboxPolicy` enum (before `KernelSettings`, around line 5):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPolicy {
    #[default]
    TrustAware,
    Always,
    Never,
}
```

3. Add fields to `KernelSettings` (after `events: EventChannelConfig`):
```rust
    #[serde(default)]
    pub sandbox_policy: SandboxPolicy,
    #[serde(default = "default_max_concurrent_sandbox_children")]
    pub max_concurrent_sandbox_children: usize,
```

4. Add default function:
```rust
fn default_max_concurrent_sandbox_children() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(2)
}
```

5. Add validation in `load_config()` after `validate_logging_settings(&config.logging)?;`:
```rust
validate_sandbox_settings(&config.kernel)?;
```

6. Add validation function:
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

7. Open `config/default.toml` and add under `[kernel]` section (before `[kernel.task_limits]`):
```toml
# Sandbox execution policy for tool calls.
# "trust_aware" (default): Core tools run in-process, Community/Verified tools sandboxed.
# "always": All sandbox-eligible tools run in sandbox children (legacy behavior).
# "never": No sandboxing (development only, NOT for production).
sandbox_policy = "trust_aware"
# Maximum concurrent sandbox child processes. Default: number of logical CPUs.
# max_concurrent_sandbox_children = 8
```

8. Add tests at the bottom of the `#[cfg(test)]` module in `config.rs`:
```rust
#[test]
fn sandbox_policy_defaults_to_trust_aware() {
    let config: KernelConfig = toml::from_str(MINIMAL_TOML).expect("should parse");
    assert_eq!(config.kernel.sandbox_policy, SandboxPolicy::TrustAware);
}

#[test]
fn sandbox_policy_parses_always() {
    let toml_str = format!("{}\nsandbox_policy = \"always\"\n", MINIMAL_TOML)
        .replace("[kernel]\n", "[kernel]\nsandbox_policy = \"always\"\n");
    // Simpler: just add it to the kernel section inline
    let toml_str = MINIMAL_TOML.replace(
        "context_window_token_budget = 8000",
        "context_window_token_budget = 8000\nsandbox_policy = \"always\"",
    );
    let config: KernelConfig = toml::from_str(&toml_str).expect("should parse");
    assert_eq!(config.kernel.sandbox_policy, SandboxPolicy::Always);
}

#[test]
fn sandbox_policy_parses_never() {
    let toml_str = MINIMAL_TOML.replace(
        "context_window_token_budget = 8000",
        "context_window_token_budget = 8000\nsandbox_policy = \"never\"",
    );
    let config: KernelConfig = toml::from_str(&toml_str).expect("should parse");
    assert_eq!(config.kernel.sandbox_policy, SandboxPolicy::Never);
}

#[test]
fn max_concurrent_sandbox_children_defaults_nonzero() {
    let config: KernelConfig = toml::from_str(MINIMAL_TOML).expect("should parse");
    assert!(config.kernel.max_concurrent_sandbox_children >= 2);
}

#[test]
fn max_concurrent_sandbox_children_rejects_zero() {
    let toml_str = MINIMAL_TOML.replace(
        "context_window_token_budget = 8000",
        "context_window_token_budget = 8000\nmax_concurrent_sandbox_children = 0",
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, toml_str).unwrap();
    let err = load_config(&path).unwrap_err();
    assert!(
        err.to_string().contains("must be > 0"),
        "expected concurrency error, got: {err}"
    );
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/config.rs` | Add `SandboxPolicy` enum, 2 fields to `KernelSettings`, default fn, validation fn, 5 tests |
| `config/default.toml` | Add `sandbox_policy` and commented `max_concurrent_sandbox_children` |

## Prerequisites

None -- this is the first subtask.

## Test Plan

- All existing config tests pass unchanged (new fields use `#[serde(default)]`)
- `sandbox_policy_defaults_to_trust_aware` -- parse MINIMAL_TOML, assert default
- `sandbox_policy_parses_always` -- explicit value parses correctly
- `sandbox_policy_parses_never` -- explicit value parses correctly
- `max_concurrent_sandbox_children_defaults_nonzero` -- runtime default >= 2
- `max_concurrent_sandbox_children_rejects_zero` -- validation rejects 0

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- config --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
