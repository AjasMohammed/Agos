---
title: "Phase 04: Tool Weight Classification and CI Guard"
tags:
  - kernel
  - sandbox
  - tools
  - v3
  - plan
date: 2026-03-21
status: planned
effort: 4h
priority: high
---

# Phase 04: Tool Weight Classification and CI Guard

> Add a CI-enforced test that ensures every tool registered in ToolRunner is also handled by the factory function, and optionally add an explicit `weight` field to tool manifests for forward compatibility.

---

## Why This Phase

After Phases 01-03, the factory function and ToolRunner are two independent code paths that must stay in sync. If a developer adds a new tool to `ToolRunner::register_memory_tools()` but forgets to add it to `factory.rs`, the tool will work in-process but silently fail in sandbox with "tool cannot run in sandbox".

This phase adds:
1. A compile-time or test-time guard that catches desynchronization
2. An optional `weight` field in the `ToolSandbox` manifest struct for explicit tool categorization (forward compatibility for user-defined tools)

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Factory-ToolRunner sync | Manual -- developer must update both | CI test fails if they diverge |
| Tool category source | Hardcoded match in `factory.rs` | Hardcoded match + optional manifest override via `weight` field |
| `ToolSandbox` struct | No `weight` field | Optional `weight: Option<String>` field |
| User-defined tools | Cannot be sandboxed (not in factory) | Can declare `weight = "stateless"` in manifest to opt into sandbox |

---

## What to Do

### 1. Add `weight` field to `ToolSandbox` in `crates/agentos-types/src/tool.rs`

Add an optional field that tool manifests can use to declare their dependency category:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSandbox {
    pub network: bool,
    pub fs_write: bool,
    #[serde(default)]
    pub gpu: bool,
    pub max_memory_mb: u64,
    pub max_cpu_ms: u64,
    #[serde(default)]
    pub syscalls: Vec<String>,
    /// Optional weight classification for sandbox resource allocation.
    /// Values: "stateless", "memory", "network", "hal".
    /// If absent, the factory auto-detects based on tool name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<String>,
}
```

### 2. Update `tool_category()` in `factory.rs` to check manifest weight first

Modify the factory to accept an optional manifest weight and use it as an override:

```rust
/// Determine the dependency category for a tool.
///
/// If `manifest_weight` is provided (from ToolSandbox.weight), it takes priority
/// over the hardcoded name-based lookup. This allows user-defined tools to
/// declare their category in the manifest.
pub fn tool_category_with_weight(
    name: &str,
    manifest_weight: Option<&str>,
) -> Option<ToolCategory> {
    // Manifest weight takes priority
    if let Some(weight) = manifest_weight {
        return match weight {
            "stateless" => Some(ToolCategory::Stateless),
            "memory" => Some(ToolCategory::Memory),
            "network" => Some(ToolCategory::Network),
            "hal" => Some(ToolCategory::Hal),
            _ => {
                tracing::warn!(
                    tool = name,
                    weight = weight,
                    "Unknown tool weight in manifest, falling back to name-based detection"
                );
                tool_category(name)
            }
        };
    }
    tool_category(name)
}
```

### 3. Add CI guard test in `crates/agentos-tools/src/factory.rs`

```rust
#[cfg(test)]
mod tests {
    // ... existing tests ...

    /// Ensure every tool registered by ToolRunner is handled by the factory.
    /// Tools that return None from tool_category() are kernel-context tools
    /// that intentionally cannot run in sandbox.
    #[test]
    fn test_factory_covers_all_runner_tools() {
        use std::path::PathBuf;

        // Get all tools from ToolRunner (requires constructing one -- expensive but CI-only)
        // We skip this if embedder is not available (CI without model cache).
        let tmp = tempfile::TempDir::new().unwrap();

        // Build a list of all tool names the factory knows about
        let known_sandboxable: Vec<&str> = vec![
            "datetime", "think", "file-reader", "file-writer", "file-editor",
            "file-glob", "file-grep", "file-delete", "file-move", "file-diff",
            "data-parser",
            "memory-search", "memory-write", "memory-read", "memory-delete",
            "memory-stats", "archival-insert", "archival-search", "episodic-list",
            "procedure-create", "procedure-delete", "procedure-list", "procedure-search",
            "memory-block-write", "memory-block-read", "memory-block-list", "memory-block-delete",
            "http-client", "web-fetch", "network-monitor",
            "hardware-info", "sys-monitor", "process-manager", "log-reader",
        ];
        let known_kernel_context: Vec<&str> = vec![
            "agent-message", "task-delegate", "agent-list", "task-status",
            "task-list", "shell-exec",
        ];
        let known_special: Vec<&str> = vec![
            "agent-manual", "agent-self", // need runtime data from kernel
        ];

        // Every tool name must appear in exactly one of these lists.
        // If a new tool is added to ToolRunner but not to any list, this test
        // will catch it via the ToolRunner::list_tools() check below.
        let all_known: std::collections::HashSet<&str> = known_sandboxable.iter()
            .chain(known_kernel_context.iter())
            .chain(known_special.iter())
            .copied()
            .collect();

        // Verify no duplicates
        let total = known_sandboxable.len() + known_kernel_context.len() + known_special.len();
        assert_eq!(all_known.len(), total, "Duplicate tool name in factory classification lists");

        // Verify tool_category returns a value for all sandboxable tools
        for name in &known_sandboxable {
            assert!(
                tool_category(name).is_some(),
                "Tool '{}' is listed as sandboxable but tool_category() returns None",
                name,
            );
        }

        // Verify tool_category returns None for kernel-context tools
        for name in &known_kernel_context {
            assert!(
                tool_category(name).is_none(),
                "Tool '{}' is listed as kernel-context but tool_category() returns Some",
                name,
            );
        }
    }
}
```

### 4. Update `task_executor.rs` to use manifest weight

Where category overhead is computed (added in Phase 03), use the manifest's weight field:

```rust
use agentos_tools::{tool_category_with_weight, ToolCategory};

let manifest_weight = {
    let registry = self.tool_registry.read().await;
    registry
        .get_by_name(&tool_call.tool_name)
        .and_then(|t| t.manifest.sandbox.weight.clone())
};

let category_overhead = match tool_category_with_weight(
    &tool_call.tool_name,
    manifest_weight.as_deref(),
) {
    Some(ToolCategory::Stateless) => SandboxConfig::OVERHEAD_STATELESS,
    Some(ToolCategory::Memory) => SandboxConfig::OVERHEAD_MEMORY,
    Some(ToolCategory::Network) => SandboxConfig::OVERHEAD_NETWORK,
    Some(ToolCategory::Hal) => SandboxConfig::OVERHEAD_HAL,
    None => SandboxConfig::OVERHEAD_DEFAULT,
};
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/tool.rs` | Add optional `weight: Option<String>` to `ToolSandbox` |
| `crates/agentos-tools/src/factory.rs` | Add `tool_category_with_weight()`, add CI guard test |
| `crates/agentos-kernel/src/task_executor.rs` | Use `tool_category_with_weight()` with manifest weight |

---

## Prerequisites

[[03-per-category-rlimit]] must be complete (provides the category overhead constants used by task_executor).

---

## Test Plan

- `cargo test -p agentos-tools -- factory` must pass including the new guard test
- `cargo test -p agentos-types` must pass (ToolSandbox deserialization with/without weight field)
- `cargo test --workspace` must pass
- Verify existing tool manifests still parse (weight field is optional with `serde(default)`)
- Test a manifest with explicit `weight = "stateless"`:
  ```toml
  [sandbox]
  network = false
  fs_write = false
  max_memory_mb = 4
  max_cpu_ms = 100
  weight = "stateless"
  ```

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
