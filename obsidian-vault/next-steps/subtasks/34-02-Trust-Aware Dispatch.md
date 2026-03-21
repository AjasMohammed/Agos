---
title: Trust-Aware Dispatch
tags:
  - kernel
  - sandbox
  - v3
  - next-steps
date: 2026-03-21
status: planned
effort: 4h
priority: critical
---

# Trust-Aware Dispatch

> Modify `sandbox_plan_for_tool()` in `task_executor.rs` to check the tool's `TrustTier` against the kernel's `SandboxPolicy`, routing Core tools in-process when the policy is `TrustAware`.

---

## Why This Subtask

`sandbox_plan_for_tool()` at line 81 of `crates/agentos-kernel/src/task_executor.rs` is the single decision point that determines sandbox vs in-process execution. Currently it returns `Some(SandboxConfig)` for every Inline tool with a known `ToolCategory`. This subtask adds the trust-tier check so Core tools return `None` (in-process) under `TrustAware` policy, eliminating fork+exec overhead for all 30+ built-in tools.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `sandbox_plan_for_tool()` return | `Some` for all categorized Inline tools | `None` for Core tools when `TrustAware`; `None` for all when `Never` |
| Trust tier read | Not accessed in dispatch | `tool.manifest.manifest.trust_tier` checked |
| Debug logging | None | `tracing::debug!` logs tool name, trust tier, policy, and decision |

## What to Do

1. Open `crates/agentos-kernel/src/task_executor.rs`

2. Find `sandbox_plan_for_tool()` (line 81). Replace the body with:

```rust
async fn sandbox_plan_for_tool(
    &self,
    tool_name: &str,
) -> Option<(SandboxConfig, u64, Option<String>)> {
    let registry = self.tool_registry.read().await;
    let tool = registry.get_by_name(tool_name)?;

    if tool.manifest.executor.executor_type != ExecutorType::Inline {
        return None;
    }

    let manifest_weight = tool.manifest.sandbox.weight.clone();
    // Kernel-context and special tools return None -- they must run in-process.
    let category = tool_category_with_weight(tool_name, manifest_weight.as_deref())?;

    // Check sandbox policy against tool trust tier.
    let trust_tier = tool.manifest.manifest.trust_tier;
    let should_sandbox = match self.config.kernel.sandbox_policy {
        crate::config::SandboxPolicy::Never => false,
        crate::config::SandboxPolicy::Always => true,
        crate::config::SandboxPolicy::TrustAware => {
            trust_tier != agentos_types::TrustTier::Core
        }
    };

    tracing::debug!(
        tool = tool_name,
        ?trust_tier,
        sandbox_policy = ?self.config.kernel.sandbox_policy,
        should_sandbox,
        "Sandbox dispatch decision"
    );

    if !should_sandbox {
        return None;
    }

    let config = SandboxConfig::from_manifest(&tool.manifest.sandbox);
    let overhead_bytes = Self::sandbox_overhead_for_category(category);
    Some((config, overhead_bytes, manifest_weight))
}
```

3. Verify imports. The file should already have access to:
   - `agentos_types::TrustTier` (via `use agentos_types::*;` or similar)
   - `crate::config::SandboxPolicy` (new from Phase 01)
   - `ExecutorType` (via agentos_types)
   - `tool_category_with_weight` (from `agentos_tools::factory`)
   - `SandboxConfig` (from `agentos_sandbox`)

   Check the imports at the top of `task_executor.rs` and add any missing ones.

4. Add unit tests. Since `sandbox_plan_for_tool()` is private and depends on the full `TaskExecutor`, extract the decision logic into a pure function for testability:

```rust
/// Determine whether a tool should be sandboxed based on policy and trust tier.
/// Extracted for testability.
fn should_sandbox_tool(
    policy: crate::config::SandboxPolicy,
    trust_tier: agentos_types::TrustTier,
) -> bool {
    match policy {
        crate::config::SandboxPolicy::Never => false,
        crate::config::SandboxPolicy::Always => true,
        crate::config::SandboxPolicy::TrustAware => {
            trust_tier != agentos_types::TrustTier::Core
        }
    }
}

#[cfg(test)]
mod sandbox_dispatch_tests {
    use super::*;
    use crate::config::SandboxPolicy;
    use agentos_types::TrustTier;

    #[test]
    fn trust_aware_core_runs_in_process() {
        assert!(!should_sandbox_tool(SandboxPolicy::TrustAware, TrustTier::Core));
    }

    #[test]
    fn trust_aware_verified_sandboxed() {
        assert!(should_sandbox_tool(SandboxPolicy::TrustAware, TrustTier::Verified));
    }

    #[test]
    fn trust_aware_community_sandboxed() {
        assert!(should_sandbox_tool(SandboxPolicy::TrustAware, TrustTier::Community));
    }

    #[test]
    fn always_sandboxes_core() {
        assert!(should_sandbox_tool(SandboxPolicy::Always, TrustTier::Core));
    }

    #[test]
    fn never_skips_sandbox_for_community() {
        assert!(!should_sandbox_tool(SandboxPolicy::Never, TrustTier::Community));
    }

    #[test]
    fn never_skips_sandbox_for_verified() {
        assert!(!should_sandbox_tool(SandboxPolicy::Never, TrustTier::Verified));
    }

    #[test]
    fn trust_aware_blocked_would_sandbox() {
        // Blocked tools are rejected earlier at registration, but if they
        // somehow reach dispatch, they should be treated as untrusted.
        assert!(should_sandbox_tool(SandboxPolicy::TrustAware, TrustTier::Blocked));
    }
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Modify `sandbox_plan_for_tool()`, add `should_sandbox_tool()` helper, add 7 unit tests |

## Prerequisites

[[34-01-Add SandboxPolicy Config]] must be complete -- `SandboxPolicy` must exist in `config.rs`.

## Test Plan

- 7 unit tests covering all (policy, trust_tier) combinations
- `cargo test -p agentos-kernel` must pass (existing tests unaffected since Core tools now run in-process, which is the same `ToolRunner::execute()` path that tests already use)
- Manual verification: run kernel with `RUST_LOG=agentos_kernel=debug`, submit a task, confirm "Sandbox dispatch decision" log appears with correct values

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- sandbox_dispatch --nocapture
cargo test -p agentos-kernel -- --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
