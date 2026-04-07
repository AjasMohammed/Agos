---
title: "Phase 2: Kernel Fallback Resolver"
tags:
  - kernel
  - tools
  - resilience
  - v4
  - plan
date: 2026-04-07
status: complete
effort: 1.5d
priority: high
---

# Phase 2: Kernel Fallback Resolver

> Implement the kernel-side fallback resolution engine that intercepts tool failures and attempts declared fallback chains before escalating to the LLM.

---

## Why This Phase

Phase 1 defined the schema. This phase is the engine: when a tool fails, the resolver checks the failed tool's manifest for matching fallback rules, transforms the payload, attempts the fallback tool, and either returns success or escalates the original error. The LLM only sees the final outcome.

---

## Current → Target State

**Current:** `ToolRunner::execute()` returns `Result<Value, AgentOSError>`. On error, the task executor injects the error as a tool result message into the context window, and the LLM reasons about what to do next.

**Target:** A `FallbackResolver` wraps `ToolRunner::execute()`. On error, it checks the tool's `FallbackRule` list, attempts matching fallbacks (up to 3 hops), and only escalates to the LLM if all fallbacks fail. Successful fallbacks include `_fallback_used: true` metadata.

---

## Detailed Subtasks

### 1. Create FallbackResolver

**File:** `crates/agentos-kernel/src/fallback_resolver.rs` (new file)

```rust
use agentos_types::*;
use std::collections::HashSet;

pub struct FallbackResolver {
    /// Max total hops across the entire fallback chain (hard ceiling).
    max_chain_depth: u8,
}

impl FallbackResolver {
    pub fn new(max_chain_depth: u8) -> Self {
        Self { max_chain_depth: max_chain_depth.min(3) }
    }

    /// Attempt to resolve a tool failure through fallback chains.
    ///
    /// Returns `Some(result)` if a fallback succeeded, `None` if no
    /// fallback matched or all fallbacks also failed.
    pub async fn try_fallback(
        &self,
        original_tool: &str,
        original_error: &AgentOSError,
        original_payload: &serde_json::Value,
        manifests: &dyn ManifestLookup,
        executor: &dyn FallbackExecutor,
        audit: &dyn FallbackAudit,
    ) -> Option<FallbackResult> {
        let error_cat = original_error.error_category();
        let manifest = manifests.get_manifest(original_tool)?;

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(original_tool.to_string());

        let mut current_tool = original_tool.to_string();
        let mut current_payload = original_payload.clone();
        let mut current_error = error_cat.to_string();
        let mut hop = 0u8;

        while hop < self.max_chain_depth {
            let manifest = manifests.get_manifest(&current_tool)?;
            let rule = manifest.fallbacks.iter()
                .find(|r| r.on_error == current_error)?;

            // Prevent cycles
            if visited.contains(&rule.try_tool) {
                return None;
            }
            visited.insert(rule.try_tool.clone());

            // Apply payload transforms
            let transformed = apply_transforms(&current_payload, &rule.transform);

            // Audit the attempt
            audit.log_fallback_attempt(&current_tool, &rule.try_tool, &current_error, hop);

            // Execute fallback
            match executor.execute_tool(&rule.try_tool, transformed.clone()).await {
                Ok(mut result) => {
                    // Tag the result so the LLM knows a fallback was used
                    if let Some(obj) = result.as_object_mut() {
                        obj.insert("_fallback_used".into(), serde_json::json!(true));
                        obj.insert("_original_tool".into(), serde_json::json!(original_tool));
                        obj.insert("_original_error".into(), serde_json::json!(error_cat));
                        obj.insert("_fallback_chain_length".into(), serde_json::json!(hop + 1));
                    }
                    return Some(FallbackResult {
                        value: result,
                        chain_length: hop + 1,
                    });
                }
                Err(e) => {
                    // Fallback also failed; try chaining further
                    current_tool = rule.try_tool.clone();
                    current_payload = transformed;
                    current_error = e.error_category().to_string();
                    hop += 1;
                }
            }
        }

        None // all fallbacks exhausted
    }
}

pub struct FallbackResult {
    pub value: serde_json::Value,
    pub chain_length: u8,
}

fn apply_transforms(
    payload: &serde_json::Value,
    transforms: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
    let mut result = payload.clone();
    for (key, transform_str) in transforms {
        if let Ok(op) = TransformOp::parse(transform_str) {
            let current = result.get(key).and_then(|v| v.as_str());
            let new_value = op.apply(current);
            if let Some(obj) = result.as_object_mut() {
                obj.insert(key.clone(), serde_json::json!(new_value));
            }
        }
    }
    result
}
```

### 2. Define trait interfaces for testing

**File:** `crates/agentos-kernel/src/fallback_resolver.rs`

```rust
/// Trait for looking up tool manifests (mockable in tests).
#[async_trait]
pub trait ManifestLookup: Send + Sync {
    fn get_manifest(&self, tool_name: &str) -> Option<&ToolManifest>;
}

/// Trait for executing a tool during fallback (mockable in tests).
#[async_trait]
pub trait FallbackExecutor: Send + Sync {
    async fn execute_tool(&self, tool_name: &str, payload: serde_json::Value) -> Result<serde_json::Value, AgentOSError>;
}

/// Trait for auditing fallback attempts (mockable in tests).
pub trait FallbackAudit: Send + Sync {
    fn log_fallback_attempt(&self, from_tool: &str, to_tool: &str, error: &str, hop: u8);
}
```

### 3. Integrate into task executor

**File:** `crates/agentos-kernel/src/task_executor.rs`

In the tool execution path (where `ToolRunner::execute()` is called and the result is processed), wrap the error path:

```rust
// After tool execution fails:
Err(tool_error) => {
    // Try fallback chain before escalating to LLM
    if let Some(fallback_result) = self.fallback_resolver.try_fallback(
        tool_name, &tool_error, &payload,
        &self.manifest_lookup, &self.fallback_executor, &self.audit,
    ).await {
        // Fallback succeeded — inject the result as if the tool succeeded
        inject_tool_result(context, tool_name, fallback_result.value);
    } else {
        // No fallback or all failed — escalate to LLM as before
        inject_tool_error(context, tool_name, &tool_error);
    }
}
```

### 4. Add audit event type

**File:** `crates/agentos-types/src/event.rs`

Add to `EventType`:

```rust
ToolFallbackAttempted,
ToolFallbackSucceeded,
ToolFallbackExhausted,
```

Add to `category()` match returning `EventCategory::ToolEvents`.

### 5. Wire up in kernel boot

**File:** `crates/agentos-kernel/src/kernel.rs`

Construct `FallbackResolver` with `max_chain_depth` from config (default 3), pass to task executor.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/fallback_resolver.rs` | New: `FallbackResolver`, `ManifestLookup`, `FallbackExecutor`, `FallbackAudit` traits |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod fallback_resolver;` |
| `crates/agentos-kernel/src/task_executor.rs` | Integrate fallback resolution in tool error path |
| `crates/agentos-kernel/src/kernel.rs` | Construct and wire `FallbackResolver` |
| `crates/agentos-types/src/event.rs` | Add `ToolFallbackAttempted`, `ToolFallbackSucceeded`, `ToolFallbackExhausted` |

---

## Dependencies

- **Requires:** Phase 1 (manifest schema, error categories, transform ops)
- **Blocks:** Phase 3 (core tool declarations need the resolver to be functional)

---

## Test Plan

1. **Single fallback success** — mock tool A fails with `StorageError`, manifest declares fallback to tool B; verify tool B is called with transformed payload and result has `_fallback_used: true`
2. **Chained fallback** — tool A → B (fails) → C (succeeds); verify chain_length = 2
3. **Cycle detection** — tool A falls back to B, B falls back to A; verify resolver stops and returns None
4. **Max depth enforcement** — chain of 4 fallbacks; verify resolver stops at 3 and returns None
5. **No matching fallback** — tool fails with `Timeout` but only has fallback for `StorageError`; verify resolver returns None
6. **Permission check** — fallback tool requires permission the agent doesn't have; verify resolver skips it
7. **Audit events** — verify `ToolFallbackAttempted` event is emitted for each hop

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- fallback_resolver
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
