---
title: Issues and Fixes Audit Update
tags:
  - roadmap
  - v3
  - plan
date: 2026-03-13
status: complete
effort: 1h
priority: medium
---

# Issues and Fixes Audit Update

> Update the Issues and Fixes document to reflect the actual implementation state as of 2026-03-13. Mark 7 of 9 issues as resolved, document the 4 new clippy errors (resolved by Phase 01), and update the deployment blockers status.

---

## Why This Phase

The Issues and Fixes document (`obsidian-vault/roadmap/Issues and Fixes.md`) was written on 2026-03-10 and lists 9 issues as "Open." A cross-reference against the actual code on 2026-03-13 reveals that 7 issues have already been fixed in commit `f63a02f`. The document is now misleading -- it makes the project look worse than it is and wastes time for anyone who reads it and tries to fix already-fixed issues.

This phase updates the document with verified findings, ensuring the roadmap accurately reflects reality.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Issue #1 status | Open | Resolved (cancellation token exists; note: 6 CLI tests still `#[ignore]` pending test harness) |
| Issue #2 status | Open | Resolved (`set_partition_for_task()` implemented and wired) |
| Issue #3 status | Open | Resolved (all adapters use `active_entries()`) |
| Issue #4 status | Open | Resolved (escalation CLI + kernel commands fully wired) |
| Issue #5 status | Open | Resolved (`parse_uncertainty()` implemented and called post-inference) |
| Issue #6 status | Open | Resolved (`infer_reasoning_hints()` auto-infers from prompt) |
| Issue #7 status | Open | Resolved (all `.record()` calls use `tracing::warn!`) |
| Issue #8 status | Open | Partially resolved (dead code removed; 4 new clippy errors found and documented) |
| Issue #9 status | Open | Still open (addressed by Phase 02 of this plan) |
| New clippy errors | Not documented | Documented with fix references |
| Deployment blockers | All "Open" | Updated based on Docker artifacts (Phase 04) |

---

## What to Do

### 1. Update `obsidian-vault/roadmap/Issues and Fixes.md`

For each resolved issue, change the status and add a resolution note:

**Issue #1** -- Change status to "Resolved (2026-03-13)". Add note:
> `CancellationToken` added to `Kernel` struct. All 6 loops in `run_loop.rs` and `task_executor.rs` use `tokio::select!` with the token. `Kernel::shutdown()` method exists. Remaining action: update integration test harness to use the token (see [[03-integration-test-harness]]).

**Issue #2** -- Change status to "Resolved (2026-03-13)". Add note:
> `ContextManager::set_partition_for_task()` implemented in `context.rs:176`. `execute_switch_partition` in `kernel_action.rs:451` calls this method instead of operating on a clone.

**Issue #3** -- Change status to "Resolved (2026-03-13)". Add note:
> All 5 production LLM adapters (openai, anthropic, gemini, ollama, custom) now call `context.active_entries()`. Only the Ollama test helper uses `as_entries()` (acceptable for test construction).

**Issue #4** -- Change status to "Resolved (2026-03-13)". Add note:
> Full escalation CLI implemented: `agentctl escalation list [--all]`, `agentctl escalation get <id>`, `agentctl escalation resolve <id> --decision <text>`. `KernelCommand::ListEscalations`, `GetEscalation`, `ResolveEscalation` wired in `run_loop.rs`. Resolution handles task requeue for approved blocking escalations and task failure for denied ones.

**Issue #5** -- Change status to "Resolved (2026-03-13)". Add note:
> `parse_uncertainty()` function implemented in `agentos-llm/src/types.rs:163`. Called in `task_executor.rs:482` after each `infer()` call. Parses `[UNCERTAINTY]...[/UNCERTAINTY]` blocks with `confidence`, `/ claim`, and `/ verify` fields. Unit tests cover parsing.

**Issue #6** -- Change status to "Resolved (2026-03-13)". Add note:
> `infer_reasoning_hints()` function in `commands/task.rs:251` auto-infers `ComplexityLevel` and `PreemptionLevel` from prompt word count. Called for both `cmd_run_task` and `cmd_delegate_task`. Background and event-triggered tasks correctly leave hints as `None` (automated tasks have no user prompt to analyze).

**Issue #7** -- Change status to "Resolved (2026-03-13)". Add note:
> All episodic memory `.record()` calls now use `if let Err(e) = ... { tracing::warn!(task_id = %task.id, error = %e, "Failed to record episodic memory"); }` pattern. Task success and failure records use `match` with explicit `Ok`/`Err` arms.

**Issue #8** -- Change status to "Partially resolved (2026-03-13)". Add note:
> `has_dependencies()` method removed. `make_engine()` test helper removed. `_input` already prefixed. 4 new clippy errors found and fixed in [[01-clippy-ci-gate-fixes]].

**Issue #9** -- Keep status "Open". Add note:
> Fix planned in [[02-event-hmac-audit-fix]].

### 2. Add new section: "Clippy Errors (2026-03-13)"

Document the 4 clippy errors found:

| Error | File | Line | Fix |
|---|---|---|---|
| `if_same_then_else` | `commands/escalation.rs` | 214 | Collapse identical `Info` branches |
| `collapsible_if` | `event_bus.rs` | 531 | Merge nested `if` |
| `unwrap_or_default` | `event_dispatch.rs` | 39 | Replace `unwrap_or_else(TraceID::new)` |
| `new_without_default` | `memory_extraction.rs` | 193 | Add `Default` impl |

### 3. Update deployment blockers table

Update the "First Deployment Blockers" table:
- "Quality gates not green" -- update to "Clippy fixed in [[01-clippy-ci-gate-fixes]]; fmt already passes"
- "Missing canonical Docker deployment artifacts" -- update to "Planned in [[04-docker-deployment-artifacts]]"

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/roadmap/Issues and Fixes.md` | Update status of 7 issues to Resolved, add clippy errors section, update deployment blockers |

---

## Prerequisites

[[01-clippy-ci-gate-fixes]] and [[02-event-hmac-audit-fix]] should be complete first so this document reflects the final state accurately.

---

## Test Plan

- No code changes -- this is a documentation update only
- Verify: all claims in the document match the actual codebase (spot-check 2-3 resolved issues by reading the referenced files)

---

## Verification

```bash
# Verify document exists and has been updated
cat obsidian-vault/roadmap/Issues\ and\ Fixes.md | grep -c "Resolved"
# Should return 7 (one per resolved issue)
```
