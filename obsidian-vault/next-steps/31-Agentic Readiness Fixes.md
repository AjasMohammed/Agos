---
title: "Agentic Readiness Fixes"
tags:
  - next-steps
  - agentic-readiness
  - phase-v3
date: 2026-03-19
status: planned
effort: 6w
priority: critical
---

# Agentic Readiness Fixes

> Address all 30 audit issues to bring AgentOS from 3.4/5 to 4.2+/5 for production agentic workflows.

---

## Current State

Audit scored agent autonomy at 2.5/5. 8 critical issues block production use. See [[00-Master Audit Summary]] and [[Agentic Readiness Fixes Plan]] for design rationale.

## Goal / Target State

All critical and high-priority issues resolved. Agent autonomy 4.0+/5. Production-ready for staging deployment.

---

## Sub-tasks

### Phase 1: Unblock Agent Autonomy (~2w)

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[31-01-Configurable Max Iterations Per Task]] | `config.rs`, `task_executor.rs`, `agentos-types/task.rs`, `config/default.toml` | complete |
| 02 | [[31-02-Multi Tool Call Parsing]] | `tool_call.rs`, `task_executor.rs` | planned |
| 03 | [[31-03-Input Schemas for TOML Manifests]] | `tools/core/*.toml`, `agent_manual.rs`, `tool_registry.rs` | planned |
| 04 | [[31-04-OpenAI Tool Call Extraction]] | `agentos-llm/src/openai.rs` | planned |

### Phase 2: Add Reliability (~2w)

| # | Task | Files | Status |
|---|------|-------|--------|
| 05 | [[31-05-Kernel State Persistence]] | `scheduler.rs`, `escalation.rs`, `cost_tracker.rs` | planned |
| 06 | [[31-06-Async Mutex Migration for Memory]] | `semantic.rs`, `episodic.rs`, `procedural.rs` | planned |
| 07 | [[31-07-Tool Output Size Limits]] | `task_executor.rs`, `runner.rs` | planned |
| 08 | [[31-08-Context Compiler Token Estimation]] | `context_compiler.rs`, `context.rs` | planned |
| 09 | [[31-09-Event Channel Backpressure]] | `event_dispatch.rs`, `run_loop.rs` | planned |

### Phase 3: Harden Security (~1w, parallel with Phase 2)

| # | Task | Files | Status |
|---|------|-------|--------|
| 10 | [[31-10-Secure Agent Pubkey Registration]] | `agent_message_bus.rs`, `kernel.rs` | planned |
| 11 | [[31-11-Pipeline Variable Sanitization]] | `pipeline/engine.rs` | planned |
| 12 | [[31-12-Proxy Token Rotation Invalidation]] | `agentos-vault/src/lib.rs` | planned |
| 13 | [[31-13-Audit Chain Verification at Startup]] | `agentos-audit/src/log.rs` | planned |

### Phase 4: Agent Ergonomics & Types (~1w)

| # | Task | Files | Status |
|---|------|-------|--------|
| 14 | [[31-14-TaskState Suspended and Budget Errors]] | `agentos-types/src/task.rs`, `error.rs` | planned |
| 15 | [[31-15-Missing HardwareResource Variants]] | `agentos-types/src/intent.rs` | planned |
| 16 | [[31-16-Configurable Anthropic Max Tokens]] | `agentos-llm/src/anthropic.rs` | planned |
| 17 | [[31-17-Tools For Prompt with Schemas]] | `tool_registry.rs`, `agent_manual.rs` | planned |
| 18 | [[31-18-Missing Event Types]] | `agentos-types/src/event.rs`, `event_dispatch.rs` | planned |
| 19 | [[31-19-PermissionOp IntentType Alignment]] | `agentos-types/src/capability.rs` | planned |
| 20 | [[31-20-Agent Self-Introspection Types]] | `agentos-types/src/` | planned |

### Phase 5: Operational Polish (~1w)

| # | Task | Files | Status |
|---|------|-------|--------|
| 21 | [[31-21-Injection Scanner False Positive Reduction]] | `injection_scanner.rs` | planned |
| 22 | [[31-22-Workspace Directory Mapping]] | `agentos-tools/src/traits.rs`, file tools | planned |
| 23 | [[31-23-Agent Registry Error Handling]] | `agent_registry.rs` | planned |
| 24 | [[31-24-Pipeline Retry Backoff]] | `pipeline/engine.rs` | planned |
| 25 | [[31-25-Tool Cancellation Support]] | `agentos-tools/src/traits.rs`, `task_executor.rs` | planned |

---

## Verification

After all phases:
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Related

- [[Agentic Readiness Fixes Plan]] — design rationale and architecture decisions
- [[00-Master Audit Summary]] — source audit with scores and recommendations
- [[01-Type System and Intent Protocol]] through [[07-Web UI Pipeline and HAL]] — detailed findings
