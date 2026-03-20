---
title: "Configurable Max Iterations Per Task"
tags:
  - next-steps
  - kernel
  - task-execution
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 4h
priority: critical
---

# Configurable Max Iterations Per Task

> Remove the hardcoded 10-iteration cap in task_executor.rs and make it configurable per-task based on complexity level.

## What to Do

The agentic loop in `task_executor.rs` has `max_iterations = 10` hardcoded. Complex tasks (multi-file refactoring, research, debugging) routinely need 20-50+ tool calls. This is the single biggest blocker for agent autonomy.

### Steps

1. **Add iteration limit config to `config/default.toml`:**
   ```toml
   [kernel.task_limits]
   max_iterations_low = 10
   max_iterations_medium = 25
   max_iterations_high = 50
   ```

2. **Add `task_limits` to `KernelSettings`** in `crates/agentos-kernel/src/config.rs`:
   - Add a `TaskLimitsConfig` struct with low/medium/high fields
   - Parse from config TOML

3. **Modify `task_executor.rs`** to use the config:
   - Find the `max_iterations = 10` constant (around the agentic loop)
   - Replace with a lookup based on `task.reasoning_hints.estimated_complexity`:
     - `ComplexityLevel::Low` → `config.task_limits.max_iterations_low`
     - `ComplexityLevel::Medium` → `config.task_limits.max_iterations_medium`
     - `ComplexityLevel::High` → `config.task_limits.max_iterations_high`
   - Tasks without reasoning hints fall back to the low-tier limit for backward compatibility

4. **Add per-task override** in `AgentTask`:
   - Add `max_iterations: Option<u32>` field to `AgentTask` in `agentos-types/src/task.rs`
   - If `Some(n)`, use that instead of the config-based default
   - Add `#[serde(default)]` annotation

5. **Add tests** for nested config parsing and iteration-cap resolution

## Files Changed

| File | Change |
|------|--------|
| `config/default.toml` | Add `[kernel.task_limits]` section |
| `crates/agentos-kernel/src/config.rs` | Add `TaskLimitsConfig` struct, parse nested kernel config |
| `crates/agentos-kernel/src/task_executor.rs` | Replace hardcoded `10` with config lookup |
| `crates/agentos-types/src/task.rs` | Add `max_iterations: Option<u32>` to `AgentTask` |
| `crates/agentos-kernel/tests/e2e/common.rs` | Update test config builder for new kernel setting |
| `crates/agentos-cli/tests/common.rs` | Update test config builder for new kernel setting |
| `crates/agentos-agent-tester/src/harness.rs` | Update test config builder for new kernel setting |

## Prerequisites

None — this is the first subtask.

## Verification

```bash
cargo test -p agentos-kernel
cargo test -p agentos-types
cargo clippy --workspace -- -D warnings
```

Confirm: a task with `ComplexityLevel::High` uses the configured high-tier cap, unless `AgentTask.max_iterations` explicitly overrides it.
