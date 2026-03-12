---
title: Command Bus Wiring — Router Coverage Audit
tags:
  - next-steps
  - infrastructure
  - bus
date: 2026-03-11
status: done
effort: 2h
priority: high
---

# Command Bus Wiring — Router Coverage Audit

> Verify that every `KernelCommand` variant added in the last session has a corresponding dispatch path in the kernel's run loop and router.

---

## Background

New `KernelCommand` variants were added to `crates/agentos-bus/src/message.rs`:
- `ListResourceLocks`
- `ReleaseResourceLock { resource_id, agent_name }`
- `ReleaseAllResourceLocks { agent_name }`
- `GetCostReport { agent_id, period }`
- `EscalationList` / escalation resolution commands

CLI handlers and kernel-side `cmd_*` functions exist (in `commands/resource.rs`, `commands/cost.rs`, `commands/escalation.rs`). But the **router / run_loop dispatch** needs to be verified — there must be a `match` arm for each command in the kernel's message processing loop.

---

## Audit Checklist

### Step 1 — Map All KernelCommand Variants

```bash
grep -n "^\s\+[A-Z][A-Za-z]" crates/agentos-bus/src/message.rs
```

List every variant and mark whether it has a dispatch arm.

### Step 2 — Check Router/RunLoop Dispatch

**File:** `crates/agentos-kernel/src/router.rs` (or `run_loop.rs` — wherever `KernelCommand` is matched)

```bash
grep -n "KernelCommand::" crates/agentos-kernel/src/router.rs
grep -n "KernelCommand::" crates/agentos-kernel/src/run_loop.rs
```

For every variant in Step 1, verify a `KernelCommand::VariantName` match arm exists.

### Step 3 — Check for `_ =>` Catch-All

If the router has a `_ => { /* unhandled */ }` catch-all, newly-added commands silently fail instead of erroring at compile time. The catch-all should be removed or replaced with explicit `compile_error!` — Rust's exhaustive match will then flag any unhandled variant at build time.

---

## Commands to Verify

| Command | `cmd_*` function | Router arm | CLI handler |
|---|---|---|---|
| `ListResourceLocks` | `cmd_resource_list()` | ❓ Verify | `ResourceCommands::List` |
| `ReleaseResourceLock` | `cmd_resource_release()` | ❓ Verify | `ResourceCommands::Release` |
| `ReleaseAllResourceLocks` | `cmd_resource_release_all()` | ❓ Verify | `ResourceCommands::ReleaseAll` |
| `GetCostReport` | `cmd_cost_report()` | ❓ Verify | `CostCommands::Report` |
| Escalation commands | `cmd_escalation_*()` | ❓ Verify | `EscalationCommands::*` |

---

## If a Router Arm Is Missing

Add the dispatch arm in the appropriate match block. Pattern:

```rust
// In router.rs or run_loop.rs:
KernelCommand::ListResourceLocks => {
    let result = kernel.cmd_resource_list().await;
    KernelResponse::ResourceLocks(result)
}

KernelCommand::ReleaseResourceLock { resource_id, agent_name } => {
    let result = kernel.cmd_resource_release(&resource_id, &agent_name).await;
    KernelResponse::Ok(result)
}

KernelCommand::ReleaseAllResourceLocks { agent_name } => {
    let result = kernel.cmd_resource_release_all(&agent_name).await;
    KernelResponse::Ok(result)
}

KernelCommand::GetCostReport { agent_id, period } => {
    let result = kernel.cmd_cost_report(&agent_id, &period).await;
    KernelResponse::CostReport(result)
}
```

---

## KernelResponse Variants to Verify

Similarly, check that `KernelResponse` has variants for all expected return types:

```bash
grep -n "^pub enum KernelResponse\|^\s\+[A-Z]" crates/agentos-bus/src/message.rs | head -60
```

Expected to exist:
- `ResourceLocks(Vec<serde_json::Value>)` or `ResourceLocks(Vec<LockSummary>)`
- `CostReport(Vec<CostSnapshot>)`
- `EscalationList(Vec<serde_json::Value>)`

---

## End-to-End Smoke Tests

After verifying dispatch, run integration tests:

```bash
cargo test -p agentos-cli -- --test integration_test
```

Check `crates/agentos-cli/tests/integration_test.rs` for coverage of the new commands. If not covered, add:

```rust
#[tokio::test]
async fn test_resource_list_command() {
    // Spawn kernel, acquire a lock, run `resource list`, verify output
}

#[tokio::test]
async fn test_cost_report_command() {
    // Run a task, then query cost report, verify structure
}
```

---

## Files Likely Changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/router.rs` | Add missing `KernelCommand::*` match arms |
| `crates/agentos-kernel/src/run_loop.rs` | Same if dispatch is here |
| `crates/agentos-cli/tests/integration_test.rs` | Add smoke tests for new commands |

---

## Related

- [[Index]] — Back to dashboard
- [[reference/Message Bus]] — Existing bus documentation
