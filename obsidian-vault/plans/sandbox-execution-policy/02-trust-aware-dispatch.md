---
title: Trust-Aware Sandbox Dispatch
tags:
  - kernel
  - sandbox
  - v3
  - plan
date: 2026-03-21
status: planned
effort: 4h
priority: critical
---

# Trust-Aware Sandbox Dispatch

> Modify `sandbox_plan_for_tool()` in `task_executor.rs` to check the tool's `TrustTier` against the kernel's `SandboxPolicy`, routing Core tools in-process when the policy is `TrustAware`.

---

## Why This Phase

This is the critical change that eliminates fork+exec overhead for Core tools. The function `sandbox_plan_for_tool()` is the single decision point that determines whether a tool call goes through `SandboxExecutor::spawn()` (fork+exec) or `ToolRunner::execute()` (in-process). Currently it returns `Some(SandboxConfig)` for every Inline tool with a known `ToolCategory`, regardless of trust tier. After this change, Core tools will return `None` (in-process) when the policy is `TrustAware`.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `sandbox_plan_for_tool()` | Returns `Some` for all Inline tools with a `ToolCategory` | Returns `None` for Core tools when `SandboxPolicy::TrustAware`; returns `None` for ALL tools when `SandboxPolicy::Never` |
| Trust tier check | Not performed in dispatch path | Reads `tool.manifest.manifest.trust_tier` and branches on it |
| Tracing | No log about why a tool was routed in-process vs sandbox | Debug-level span logs the routing decision with tool name, trust tier, and policy |

## What to Do

1. Open `crates/agentos-kernel/src/task_executor.rs`

2. Locate the `sandbox_plan_for_tool()` method (currently at line 81). The current implementation is:

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
    let category = tool_category_with_weight(tool_name, manifest_weight.as_deref())?;
    let config = SandboxConfig::from_manifest(&tool.manifest.sandbox);
    let overhead_bytes = Self::sandbox_overhead_for_category(category);
    Some((config, overhead_bytes, manifest_weight))
}
```

3. Replace it with the trust-aware version:

```rust
async fn sandbox_plan_for_tool(
    &self,
    tool_name: &str,
) -> Option<(SandboxConfig, u64, Option<String>)> {
    let registry = self.tool_registry.read().await;
    let tool = registry.get_by_name(tool_name)?;

    // Non-Inline tools (e.g. WASM) have their own execution path.
    if tool.manifest.executor.executor_type != ExecutorType::Inline {
        return None;
    }

    let manifest_weight = tool.manifest.sandbox.weight.clone();
    // Kernel-context and special tools (agent-list, task-list, agent-self, etc.)
    // return None from tool_category_with_weight -- they must execute in-process,
    // not in a sandbox child where they lack access to kernel state.
    let category = tool_category_with_weight(tool_name, manifest_weight.as_deref())?;

    // Trust-tier-aware dispatch: check the sandbox policy to decide whether
    // this tool should be sandboxed or executed in-process.
    let trust_tier = tool.manifest.manifest.trust_tier;
    let sandbox_policy = self.config.kernel.sandbox_policy;

    let should_sandbox = match sandbox_policy {
        crate::config::SandboxPolicy::Never => false,
        crate::config::SandboxPolicy::Always => true,
        crate::config::SandboxPolicy::TrustAware => {
            // Core tools are distribution-trusted -- run in-process.
            // Verified and Community tools are untrusted -- sandbox them.
            trust_tier != TrustTier::Core
        }
    };

    tracing::debug!(
        tool = tool_name,
        ?trust_tier,
        ?sandbox_policy,
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

4. Ensure the necessary imports are present at the top of `task_executor.rs`:
   - `use crate::config::SandboxPolicy;` -- add if not present
   - `use agentos_types::TrustTier;` -- already available via `use agentos_types::*;` in most kernel modules, but verify

5. The `TrustTier` type is in `crates/agentos-types/src/tool.rs` and is already `#[derive(PartialEq)]`, so the `!= TrustTier::Core` comparison works.

6. The `self.config` field is of type `KernelConfig` which now has `kernel.sandbox_policy: SandboxPolicy` from Phase 01.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Modify `sandbox_plan_for_tool()` to check trust tier against sandbox policy |

## Prerequisites

[[01-execution-policy-config]] must be complete -- `SandboxPolicy` enum must exist in `config.rs` and `KernelSettings` must have the `sandbox_policy` field.

## Test Plan

Testing this change requires either:
- (A) A unit test that constructs a `TaskExecutor` with a mock registry containing tools with different trust tiers, or
- (B) An integration test that boots a kernel with a specific `sandbox_policy` and submits tool calls.

Since `sandbox_plan_for_tool()` is a private method on the `TaskExecutor` struct, the most practical approach is integration testing via the existing e2e test infrastructure:

- **Test `core_tool_skips_sandbox_with_trust_aware_policy`**: Boot a kernel with `sandbox_policy = "trust_aware"`. Register a Core tool. Submit a task that calls it. Verify via tracing output or audit log that the tool executed in-process (look for `"Executing tool"` log from `ToolRunner::execute()` rather than `"Sandbox child spawned"` from `SandboxExecutor::spawn()`).

- **Test `community_tool_uses_sandbox_with_trust_aware_policy`**: Same setup but register a Community-tier tool. Verify sandbox execution path is taken.

- **Test `core_tool_sandboxed_with_always_policy`**: Boot kernel with `sandbox_policy = "always"`. Register Core tool. Verify sandbox path is taken.

- **Test `all_tools_in_process_with_never_policy`**: Boot kernel with `sandbox_policy = "never"`. Register Community tool. Verify in-process execution.

If full e2e tests are too heavy, add a focused test that extracts the dispatch logic into a helper function that takes `(SandboxPolicy, TrustTier) -> bool`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn should_sandbox(policy: SandboxPolicy, trust_tier: TrustTier) -> bool {
        match policy {
            SandboxPolicy::Never => false,
            SandboxPolicy::Always => true,
            SandboxPolicy::TrustAware => trust_tier != TrustTier::Core,
        }
    }

    #[test]
    fn trust_aware_core_skips_sandbox() {
        assert!(!should_sandbox(SandboxPolicy::TrustAware, TrustTier::Core));
    }

    #[test]
    fn trust_aware_community_uses_sandbox() {
        assert!(should_sandbox(SandboxPolicy::TrustAware, TrustTier::Community));
    }

    #[test]
    fn trust_aware_verified_uses_sandbox() {
        assert!(should_sandbox(SandboxPolicy::TrustAware, TrustTier::Verified));
    }

    #[test]
    fn always_policy_sandboxes_core() {
        assert!(should_sandbox(SandboxPolicy::Always, TrustTier::Core));
    }

    #[test]
    fn never_policy_skips_sandbox_for_community() {
        assert!(!should_sandbox(SandboxPolicy::Never, TrustTier::Community));
    }
}
```

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- sandbox --nocapture
cargo test -p agentos-kernel -- --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
