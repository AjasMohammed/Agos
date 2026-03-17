---
title: Unwired Features
tags:
  - kernel
  - event-system
  - security
  - web
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 5d
priority: critical
---

# Unwired Features

> Close four critical gaps: 31 never-emitted EventType variants, pipeline executor security bypass, disconnected web UI crate, and stale plan doc statuses.

---

## Current State

Code review found 31 of 47 EventType variants are defined but never emitted from any code path. The pipeline executor bypasses all security checks (empty `PermissionSet`, no injection scanning, no intent validation). The `agentos-web` crate is orphaned with no entry point. Ten event-trigger plan docs have incorrect `status:` frontmatter.

## Goal / Target State

- 43 of 47 EventType variants emitted (4 external events deferred -- need new subsystems)
- Pipeline executor enforces capability tokens, permissions, injection scanning
- Web UI accessible via `agentctl web serve` CLI command
- All plan doc statuses accurate

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[01-emit-missing-event-types]] | `task_executor.rs`, `resource_arbiter.rs`, `run_loop.rs`, `health_monitor.rs`, `commands/hal.rs`, `commands/secret.rs`, `event_dispatch.rs`, `tool_registry.rs`, `task_completion.rs`, `agent_message_bus.rs`, `context.rs`, `retrieval_gate.rs` | planned |
| 02 | [[02-pipeline-security-hardening]] | `commands/pipeline.rs`, `message.rs` | planned |
| 03 | [[03-web-ui-integration]] | `agentos-cli/Cargo.toml`, `commands/web.rs`, `commands/mod.rs`, `main.rs` | planned |
| 04 | [[04-stale-docs-cleanup]] | 10 files in `obsidian-vault/plans/event-trigger-completion/` | complete |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings

# Verify event emissions:
grep -r "emit_event" crates/agentos-kernel/src/ | wc -l
# Should be significantly higher than current count (~22)

# Verify pipeline uses real permissions:
grep "PermissionSet::new()" crates/agentos-kernel/src/commands/pipeline.rs
# Should return 0 matches (all replaced with resolved permissions)

# Verify web command exists:
cargo run -p agentos-cli -- web --help
```

## Related

- [[Unwired Features Plan]] -- Master plan with full architecture, audit tables, and design decisions
- [[Event Trigger Completion Plan]] -- Original event trigger plan
- [[Event Trigger Completion Data Flow]] -- Event flow diagram
