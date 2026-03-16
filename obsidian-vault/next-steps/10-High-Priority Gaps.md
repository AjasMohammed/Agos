---
title: High-Priority Spec Gaps — Comprehensive Fix
tags:
  - kernel
  - security
  - cli
  - phase-3
  - next-steps
  - feature
date: 2026-03-11
completed: 2026-03-16
status: complete
effort: 1d
priority: critical
---

# High-Priority Spec Gaps — Comprehensive Fix

> Implementing the 7 highest-priority gaps identified in the full spec audit against `agos-implementation-spec.md`.

---

## Current State

A full audit against all 12 specs revealed these critical/high-priority gaps:

| # | Spec | Gap | Impact |
|---|------|-----|--------|
| 1 | #12 | CLI `agentctl escalation list/resolve` not wired | Users cannot respond to approval requests |
| 2 | #6  | High-taint injection output not blocked — only wrapped | Injection attacks flow into context |
| 3 | #5  | Checkpoint/rollback not wired — fields exist, never populated | No rollback capability |
| 4 | #7  | A2A messages unsigned — no `signature`/`ttl_seconds` field | Message forgery possible |
| 5 | #8  | No deadlock detection — wait-for graph absent | Agents can deadlock indefinitely |
| 6 | #11 | Token budget enforcement missing — entry count only, not token count | Context overflow undetected |
| 7 | #3  | Zero-exposure secret proxy missing (tracked separately) | Agents see plaintext secrets |

---

## Goal / Target State

- `agentctl escalation list` and `agentctl escalation resolve` work end-to-end
- High-taint (`ThreatLevel::High`) tool output triggers blocking escalation, not just taint-wrap
- Write tool calls snapshot context to `data_dir/snapshots/` and log `SnapshotTaken` with `rollback_ref`
- `agentctl audit rollback --task <id>` command restores context from snapshot
- `AgentMessage` carries `signature: Option<String>` and `ttl_seconds: u64`; messages are signed with sender's Ed25519 key
- `ResourceArbiter` tracks wait-for graph, runs DFS on each new waiter, returns `Err` on cycle detection
- `ContextManager` tracks estimated token total per task; at 80% evicts least-important entries; at 95% records checkpoint event

---

## Step-by-Step Plan

1. **Write obsidian doc** ✅ (this file)
2. **CLI escalation** — create `crates/agentos-cli/src/commands/escalation.rs`, add `Commands::Escalation` to `main.rs`, wire in `commands/mod.rs`
3. **High-taint gate** — in `task_executor.rs` post-scan block (line ~854), if `max_threat == High` create blocking escalation and bail
4. **Checkpoint/rollback** — snapshot context to JSON before write operations; add `RollbackTask` kernel command; wire CLI `agentctl audit rollback`
5. **A2A signing** — add `signature` + `ttl_seconds` to `AgentMessage`; sign in `execute_send_message` using identity manager
6. **Deadlock detection** — add `wait_for: HashMap<AgentID, AgentID>` to `ResourceArbiter`; DFS cycle check before queuing waiter
7. **Token budget** — add `total_tokens` tracking to `ContextManager`; apply 80%/95% thresholds

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-cli/src/commands/escalation.rs` | NEW — `EscalationCommands`, `handle()` |
| `crates/agentos-cli/src/main.rs` | Add `Commands::Escalation`, import |
| `crates/agentos-cli/src/commands/mod.rs` | Add `pub mod escalation`, dispatch arm |
| `crates/agentos-cli/src/commands/audit.rs` | Add `AuditCommands::Rollback`, handler |
| `crates/agentos-bus/src/message.rs` | Add `RollbackTask` command + response |
| `crates/agentos-kernel/src/task_executor.rs` | High-taint gate; checkpoint before write |
| `crates/agentos-kernel/src/commands/audit.rs` | NEW (or update) — `cmd_rollback_task` |
| `crates/agentos-types/src/agent_message.rs` | Add `signature`, `ttl_seconds`, `expires_at` |
| `crates/agentos-kernel/src/kernel_action.rs` | Sign messages in `execute_send_message` |
| `crates/agentos-kernel/src/resource_arbiter.rs` | Add wait-for graph + DFS deadlock detection |
| `crates/agentos-kernel/src/context.rs` | Add token tracking + 80%/95% thresholds |

---

## Verification

```bash
cargo build --workspace
cargo test --workspace

# Test escalation CLI
agentctl escalation list
agentctl escalation list --all
agentctl escalation resolve 1 --decision "Approved"

# Test rollback
agentctl audit rollback --task <task-id>

# Verify audit chain integrity
agentctl audit verify
```

---

## Related

[[agos-implementation-spec]]
[[03-Snapshot Rollback]]
[[09-Signed Skill Registry]]
