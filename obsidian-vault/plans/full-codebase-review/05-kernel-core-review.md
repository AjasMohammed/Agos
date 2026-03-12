---
title: "Phase 5: Kernel Core Review"
tags:
  - review
  - kernel
  - security
  - phase-5
date: 2026-03-13
status: planned
effort: 8h
priority: critical
---

# Phase 5: Kernel Core Review

> Review the largest crate: `agentos-kernel` — 49 files, 13,483 lines covering the central orchestrator, all command handlers, and every subsystem.

---

## Why This Phase

The kernel is where **everything converges**: task execution, tool dispatch, capability validation, injection scanning, cost tracking, escalation, and event handling all run here. It holds the most complex state machines and the most concurrency-sensitive code. The largest single file (`task_executor.rs`, 1,412 lines) is the highest bug-density risk.

---

## Current → Target State

- **Current:** 49 files, 13,483 lines, no dedicated test directory (tested indirectly via CLI integration tests)
- **Target:** All 32 modules and 14 command handlers reviewed for state machine correctness, concurrency safety, and security enforcement

---

## Step 5.1 — Config & Core Manifests (~155 lines)

**Files:**
- `crates/agentos-kernel/src/config.rs` (113) — Configuration loading
- `crates/agentos-kernel/src/core_manifests.rs` (42) — Embedded tool manifests

**Checklist:**
- [ ] Config defaults are secure (not overly permissive)
- [ ] Config parsing handles missing fields with sane defaults
- [ ] Core manifest definitions match actual tool implementations

---

## Step 5.2 — Kernel Boot (~354 lines)

**Files:**
- `crates/agentos-kernel/src/kernel.rs` (354) — `Kernel` struct, initialization, lifecycle

**Checklist:**
- [ ] `Kernel::new()` initializes all subsystems in correct order
- [ ] `Arc<RwLock<>>` used correctly for shared state
- [ ] `CancellationToken` propagated to all background tasks
- [ ] Startup failure is clean (no partially initialized state)
- [ ] Shutdown sequence releases all resources

---

## Step 5.3 — Run Loop & Router (~898 lines)

**Files:**
- `crates/agentos-kernel/src/run_loop.rs` (661) — Main event loop, command dispatch
- `crates/agentos-kernel/src/router.rs` (237) — Intent routing

**Checklist:**
- [ ] Run loop dispatches all KernelCommand variants (no missing arms)
- [ ] Router correctly maps intents to tools
- [ ] Error in one command does not crash the run loop
- [ ] Cancellation token checked in loop (graceful shutdown)
- [ ] No blocking operations inside the async run loop
- [ ] Restart logic (MAX_RESTARTS=5 per 60s) is sound

---

## Step 5.4 — Context & Context Compiler (~765 lines)

**Files:**
- `crates/agentos-kernel/src/context.rs` (211) — `ContextManager`
- `crates/agentos-kernel/src/context_compiler.rs` (554) — Context window compilation

**Checklist:**
- [ ] Context assembly respects token budget
- [ ] Context compiler prioritizes entries correctly (system > tool results > history)
- [ ] Context truncation does not corrupt structured data
- [ ] Concurrent access to context is safe (locking strategy)
- [ ] Memory pressure: context does not grow unbounded

---

## Step 5.5 — Task Executor (~1,412 lines) `CRITICAL`

**Files:**
- `crates/agentos-kernel/src/task_executor.rs` (1,412) — **Largest file in the codebase**

**Checklist:**
- [ ] Task lifecycle state machine: all transitions valid
- [ ] Concurrent task execution: no data races between tasks
- [ ] Task timeout enforcement works correctly
- [ ] Tool call within task: capability token validated
- [ ] LLM inference errors handled gracefully (retry? fail task?)
- [ ] Cost tracking per task is accurate
- [ ] Preemption logic: higher-priority task can preempt lower
- [ ] Resource cleanup on task failure/cancellation
- [ ] `unwrap()` calls: search for panics in production paths

---

## Step 5.6 — Scheduler & Schedule Manager (~549 lines)

**Files:**
- `crates/agentos-kernel/src/scheduler.rs` (362) — Priority queue, dependency graph
- `crates/agentos-kernel/src/schedule_manager.rs` (187) — Cron job management

**Checklist:**
- [ ] Priority queue ordering is correct (higher priority + older = first)
- [ ] Scheduler handles empty queue without spinning
- [ ] Cron schedule parsing and next-fire-time calculation
- [ ] Concurrent schedule creation/deletion is safe

---

## Step 5.7 — Security Modules (~1,143 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-kernel/src/injection_scanner.rs` (381) — Prompt injection detection
- `crates/agentos-kernel/src/intent_validator.rs` (421) — Intent schema validation
- `crates/agentos-kernel/src/risk_classifier.rs` (269) — Risk level classification
- `crates/agentos-kernel/src/rate_limit.rs` (72) — Rate limiting

**Checklist:**
- [ ] Injection scanner covers: prompt injection, jailbreak, data exfiltration patterns
- [ ] Scanner patterns not trivially bypassable (case-insensitive, unicode normalization)
- [ ] Intent validator validates all required fields, rejects unknown types
- [ ] Risk classifier: risk levels correctly mapped to required approval levels
- [ ] Rate limiter: resistant to burst abuse, per-agent isolation

---

## Step 5.8 — Escalation & Identity (~636 lines) `SECURITY`

**Files:**
- `crates/agentos-kernel/src/escalation.rs` (449) — Escalation workflow
- `crates/agentos-kernel/src/identity.rs` (187) — Ed25519 identity management

**Checklist:**
- [ ] `expires_at` correctly auto-denies after 5min timeout
- [ ] `sweep_expired()` runs regularly and does not miss entries
- [ ] Escalation approval cannot be forged
- [ ] Identity key generation uses secure RNG
- [ ] Private keys stored in vault (not plaintext on disk)

---

## Step 5.9 — Cost Tracker & Resource Arbiter (~1,414 lines)

**Files:**
- `crates/agentos-kernel/src/cost_tracker.rs` (733) — Cost attribution
- `crates/agentos-kernel/src/resource_arbiter.rs` (681) — Resource lock management

**Checklist:**
- [ ] Float accumulation errors over many requests
- [ ] Budget enforcement: hard stop when exceeded (not just warning)
- [ ] Resource arbiter: deadlock prevention (no circular lock acquisition)
- [ ] Resource lock expiry: stale locks cleaned up
- [ ] Concurrent resource requests handled fairly (FIFO)
- [ ] Cost attribution matches actual LLM usage (no double-counting)

---

## Step 5.10 — Event System (~1,170 lines)

**Files:**
- `crates/agentos-kernel/src/event_bus.rs` (418) — Subscription registry
- `crates/agentos-kernel/src/event_dispatch.rs` (236) — Event emission
- `crates/agentos-kernel/src/commands/event.rs` (516) — Event commands

**Checklist:**
- [ ] Subscribers receive all matching events (no dropped events under load)
- [ ] Throttle policy prevents event storms
- [ ] Event bus does not leak memory (subscribers cleaned up on agent deregister)
- [ ] HMAC event signing correct
- [ ] Event command handlers validate input

---

## Step 5.11 — Snapshot, Trigger Prompt, Kernel Action (~1,212 lines)

**Files:**
- `crates/agentos-kernel/src/snapshot.rs` (424) — Task snapshots
- `crates/agentos-kernel/src/trigger_prompt.rs` (348) — Event-triggered prompts
- `crates/agentos-kernel/src/kernel_action.rs` (440) — Privileged tool actions

**Checklist:**
- [ ] Snapshot: `sweep_expired_snapshots(max_age)` correctly calculates age
- [ ] Snapshot: serialization/deserialization round-trips perfectly
- [ ] Trigger prompt: injection-safe system prompt construction
- [ ] Kernel action: all action types handled, no missing match arms
- [ ] Kernel action does not bypass permission checks

---

## Step 5.12 — Registries & Helpers (~670 lines)

**Files:**
- `crates/agentos-kernel/src/agent_registry.rs` (285), `tool_registry.rs` (124), `schema_registry.rs` (122), `tool_call.rs` (77), `background_pool.rs` (62)

**Checklist:**
- [ ] Agent registry: duplicate agent IDs handled
- [ ] Tool registry: `register()` returns `Result` (not panic)
- [ ] Schema registry: JSON schema validation correct
- [ ] Tool call parsing handles malformed input
- [ ] Background pool: tasks bounded (no unbounded spawning)

---

## Step 5.13 — Agent Message Bus, Metrics, Health (~632 lines)

**Files:**
- `crates/agentos-kernel/src/agent_message_bus.rs` (461), `metrics.rs` (57), `health.rs` (114)

**Checklist:**
- [ ] Message delivery guarantees (at-least-once? at-most-once?)
- [ ] Bounded queue (no OOM from message flood)
- [ ] No sensitive data in metric labels
- [ ] Health check does not block on slow subsystems

---

## Step 5.14 — Command Handlers A (~1,256 lines)

**Files:**
- `crates/agentos-kernel/src/commands/agent.rs` (393), `task.rs` (237), `pipeline.rs` (462), `background.rs` (164)

**Checklist:**
- [ ] Each handler validates input before processing
- [ ] Error responses descriptive but do not leak internals
- [ ] Pipeline: handles missing pipeline ID gracefully

---

## Step 5.15 — Command Handlers B (~869 lines)

**Files:**
- `crates/agentos-kernel/src/commands/permission.rs` (310), `role.rs` (232), `schedule.rs` (130), `secret.rs` (97), `tool.rs` (50), `audit.rs` (20), `system.rs` (30)

**Checklist:**
- [ ] Permission command: cannot escalate beyond caller's permissions
- [ ] Secret command: vault operations properly authenticated
- [ ] Role assignment validates role exists

---

## Step 5.16 — Command Handlers C (~290 lines)

**Files:**
- `crates/agentos-kernel/src/commands/escalation.rs` (109), `cost.rs` (32), `resource.rs` (79), `identity.rs` (70)

**Checklist:**
- [ ] Escalation commands properly gate on authority
- [ ] Cost commands do not allow budget manipulation by unauthorized agents
- [ ] Resource commands validate resource existence

---

## Files Changed

No files changed — read-only review phase.

## Dependencies

Phases 1-4 (all lower layers understood).

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel
cargo clippy -p agentos-kernel -- -D warnings
```

---

## Related

- [[Full Codebase Review Plan]]
- [[04-tools-and-wasm-review]]
- [[06-user-interfaces-review]]
- [[08-security-deep-dives]]
