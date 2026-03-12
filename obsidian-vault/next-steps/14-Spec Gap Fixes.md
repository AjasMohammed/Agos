---
title: Spec Gap Fixes
tags:
  - kernel
  - security
  - cli
  - v3
  - next-steps
date: 2026-03-11
status: complete
effort: 3d
priority: critical
---

# Spec Gap Fixes

> Wire missing integrations and add absent CLI commands to close all 18 gaps identified in a full audit of the 12-section implementation spec.

---

## Current State

Build is clean (281 tests pass, 0 fail). Most systems are 75-95% implemented, but several critical integration wires are disconnected and some CLI/feature gaps remain:

- **CRL dead code** — `verify_manifest_with_crl()` exists but `tool_registry.rs` never calls it
- **Zero-exposure proxy unwired** — `issue_proxy_token()` / `resolve_proxy()` exist in vault but tools still get full `Arc<SecretsVault>` with plaintext access
- **Vault proxy sweep missing** — `sweep_expired_proxy_tokens()` never called in kernel loop
- **No Tier 3 persistent memory** — no archival policy or export on SemanticStore / EpisodicStore
- **Missing CLI commands** — vault lockdown, identity show/revoke, audit export, resource contention
- **No soft-approval window** — only hard-approval (blocking escalation) exists
- **No deadlock preemption** — detected but returns error; no priority-based resolution
- **No pipeline cost enforcement** — steps execute without budget checks
- **No wall-time budget** — `AgentBudget` has no `max_wall_time_seconds` field

## Goal / Target State

All 18 gaps closed. Every spec section fully wired end-to-end. `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings` pass clean.

## Step-by-Step Plan

### Critical Priority

| # | Subtask | Files (max 3) | Depends On |
|---|---------|---------------|------------|
| 1 | Wire `vault.sweep_expired_proxy_tokens()` into run_loop timeout checker | `run_loop.rs` | — |
| 2 | CRL enforcement: add `crl` field to `ToolRegistry`, call `verify_manifest_with_crl()` in `register()` | `tool_registry.rs`, `kernel.rs` | — |
| 3a | Create `ProxyVault` wrapper exposing only `resolve_proxy()` | `vault.rs` | — |
| 3b | Replace `Arc<SecretsVault>` with `ProxyVault` in `ToolExecutionContext` | `traits.rs` or `lib.rs` (agentos-tools) | 3a |
| 3c | Wire `ProxyVault::new(vault)` in task executor context construction | `task_executor.rs` | 3b |
| 4a | Add `sweep_old_entries()` + `export_jsonl()` to `SemanticStore` | `semantic.rs` | — |
| 4b | Add `sweep_old_entries()` + `export_jsonl()` to `EpisodicStore` | `episodic.rs` | — |

### Moderate Priority

| # | Subtask | Files (max 3) | Depends On |
|---|---------|---------------|------------|
| 5a | Add `VaultLockdown` variant to `KernelCommand` | `message.rs` | — |
| 5b | Add lockdown handler + CLI `secret lockdown` subcommand | `commands/secret.rs` (kernel), `run_loop.rs`, `commands/secret.rs` (cli) | 5a |
| 6 | Fix CLI scope parsing: pass raw scope string to kernel, resolve agent name there | `message.rs`, `commands/secret.rs` (kernel) | — |
| 7a | Add `IdentityShow` / `IdentityRevoke` to `KernelCommand` | `message.rs` | — |
| 7b | Create CLI identity module with `show` / `revoke` subcommands | `identity.rs` (new, cli), `mod.rs` (cli), `main.rs` | 7a |
| 7c | Create kernel identity handlers + dispatch arms | `identity.rs` (new, kernel), `mod.rs` (kernel), `run_loop.rs` | 7a |
| 8 | Add `revoke_agent()` to `CapabilityEngine`; call from identity revoke handler | `engine.rs` (capability), `identity.rs` (kernel) | 7c |
| 9 | Add `priority: u8` to `LockWaiter`; preempt lower-priority holder on deadlock | `resource_arbiter.rs` | — |
| 10 | Add `check_budget()` to `PipelineExecutor` trait; call before each step | `engine.rs` (pipeline), `commands/pipeline.rs` (kernel) | — |
| 11 | Add `auto_action` field to `PendingEscalation`; create soft-approval with 30s timeout auto-approve | `escalation.rs`, `task_executor.rs` | — |
| 12 | Add `notify_url` to `EscalationManager`; fire async HTTP POST on escalation creation | `escalation.rs` | — |

### Minor Priority

| # | Subtask | Files (max 3) | Depends On |
|---|---------|---------------|------------|
| 13 | Add `max_wall_time_seconds` to `AgentBudget`; check in `check_limits()` | `task.rs`, `cost_tracker.rs` | — |
| 14a | Add `export_chain_json()` to `AuditLog` | `log.rs` | — |
| 14b | Add `ExportAuditChain` command + CLI `audit export` subcommand | `message.rs`, `audit.rs` (cli) | 14a |
| 15 | Add `contention_stats()` to `ResourceArbiter` + CLI `resource contention` | `resource_arbiter.rs`, `resource.rs` (cli) | — |
| 16 | Add `import_jsonl()` to `SemanticStore` and `EpisodicStore` | `semantic.rs`, `episodic.rs` | 4a, 4b |
| 17 | Add `notify_tx` channel to `CostTracker`; send on Warning/PauseRequired | `cost_tracker.rs` | — |

## Files Changed

| File | Subtasks |
|------|----------|
| `crates/agentos-kernel/src/run_loop.rs` | 1, 5b, 7c |
| `crates/agentos-kernel/src/tool_registry.rs` | 2 |
| `crates/agentos-kernel/src/kernel.rs` | 2 |
| `crates/agentos-vault/src/vault.rs` | 3a |
| `crates/agentos-tools/src/lib.rs` (or traits.rs) | 3b |
| `crates/agentos-kernel/src/task_executor.rs` | 3c, 11 |
| `crates/agentos-memory/src/semantic.rs` | 4a, 16 |
| `crates/agentos-memory/src/episodic.rs` | 4b, 16 |
| `crates/agentos-bus/src/message.rs` | 5a, 6, 7a, 14b |
| `crates/agentos-kernel/src/commands/secret.rs` | 5b, 6 |
| `crates/agentos-cli/src/commands/secret.rs` | 5b |
| `crates/agentos-cli/src/commands/identity.rs` (new) | 7b |
| `crates/agentos-cli/src/commands/mod.rs` | 7b |
| `crates/agentos-cli/src/main.rs` | 7b |
| `crates/agentos-kernel/src/commands/identity.rs` (new) | 7c, 8 |
| `crates/agentos-kernel/src/commands/mod.rs` | 7c |
| `crates/agentos-capability/src/engine.rs` | 8 |
| `crates/agentos-kernel/src/resource_arbiter.rs` | 9, 15 |
| `crates/agentos-pipeline/src/engine.rs` | 10 |
| `crates/agentos-kernel/src/commands/pipeline.rs` | 10 |
| `crates/agentos-kernel/src/escalation.rs` | 11, 12 |
| `crates/agentos-types/src/task.rs` | 13 |
| `crates/agentos-kernel/src/cost_tracker.rs` | 13, 17 |
| `crates/agentos-audit/src/log.rs` | 14a |
| `crates/agentos-cli/src/commands/audit.rs` | 14b |
| `crates/agentos-cli/src/commands/resource.rs` | 15 |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## Related

- [[agos-implementation-spec]]
- [[11-Spec Enforcement Hardening]]
- [[12-Production Readiness Audit]]
- [[13-Event Trigger System]]
