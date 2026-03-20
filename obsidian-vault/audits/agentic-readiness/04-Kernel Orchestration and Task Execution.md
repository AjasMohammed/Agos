---
title: "Audit #4: Kernel Orchestration & Task Execution"
tags:
  - audit
  - kernel
  - scheduler
  - task-execution
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 3h
priority: critical
---

# Audit #4: Kernel Orchestration & Task Execution

> Evaluating the kernel's core loop, scheduler, task executor, context compiler, and supporting subsystems from the perspective of an AI agent relying on this infrastructure.

---

## Scope

- `kernel.rs` — Monolithic kernel struct (1,542 LOC)
- `run_loop.rs` — Supervisor loop with 11 subsystem tasks (1,538 LOC)
- `scheduler.rs` — Priority queue + dependency graph (616 LOC)
- `task_executor.rs` — Agentic loop: LLM → tool call → context update (2,169 LOC)
- `context_compiler.rs` — Token-budgeted context assembly (636 LOC)
- `tool_call.rs` — Regex-based tool call parsing (121 LOC)
- `task_completion.rs` — Task finalization + dependency wake-up (351 LOC)
- `injection_scanner.rs` — Prompt injection detection (438 LOC)
- `intent_validator.rs` — Semantic coherence checking (427 LOC)
- `escalation.rs` — Approval workflow (594 LOC)
- `cost_tracker.rs` — Budget enforcement (780 LOC)
- `rate_limit.rs` — Per-agent rate limiting (180 LOC)

---

## Verdict: ARCHITECTURALLY SOUND — but operationally fragile for production

The kernel implements a proper agentic loop with multi-layer validation (capability + schema + semantic coherence), injection scanning with NFKC normalization, budget enforcement, and escalation workflows. However, **all state is in-memory** — a restart loses all tasks, escalations, and cost snapshots. Hardcoded limits (10 iterations, 300s escalation timeout) are too restrictive for complex autonomous tasks.

---

## Findings

### 1. Agentic Loop (task_executor.rs) — SOLID

The core loop: `dequeue task → assemble context → LLM inference → parse tool calls → validate → execute → inject result → repeat`.

**What works well for me as an agent:**
- Multi-layer tool call validation: Layer A (capability/schema) + Layer B (semantic coherence).
- Injection scanning on all tool output with NFKC Unicode normalization.
- Cost tracking with budget enforcement per iteration (warn → pause → kill).
- Model downgrade when approaching budget limits.
- Adaptive memory retrieval: semantic + episodic search injected per iteration.
- Event-triggered tasks can skip retrieval (optimization).
- Context utilization monitoring: ContextWindowNearLimit emitted at 80%.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 1 | **Max iterations hardcoded to 10** — I cannot complete complex multi-step tasks | Critical | Severely limits autonomous capability |
| 2 | **Context recompilation on every iteration** — expensive for long tasks | High | Wasted compute, increased latency |
| 3 | **No tool output size limit** — malicious tool could return megabytes, causing OOM | High | Reliability risk |
| 4 | **No explicit timeout on tool execution** — relies on sandbox timeout, not always present | Medium | Tool hangs can block the task loop |
| 5 | **Budget check only after tool call, not before LLM inference** — I could exhaust budget on the inference itself | Medium | Budget overshoot |
| 6 | **Model downgrade happens silently** — I don't know my model changed mid-task | Medium | Unexpected quality degradation |
| 7 | **Knowledge blocks refreshed every iteration if configured** — linear search overhead accumulates | Low | Latency increase over iterations |

### 2. Scheduler (scheduler.rs) — GOOD

**What works well:**
- Priority-based scheduling with FIFO tiebreaking.
- Dependency graph with DFS cycle detection.
- Preemption sensitivity multipliers: High=3x, Normal=2x timeout.
- `update_state_if_not_terminal()` prevents race conditions.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 8 | **BinaryHeap doesn't support removal** — dequeued tasks that change state waste heap entries | Medium | Memory growth over time |
| 9 | **No backoff on timeout** — immediately marks Failed, no retry opportunity | Medium | Transient failures become permanent |
| 10 | **Dependency graph is linear search O(n)** — scales poorly for large task graphs | Low | Future concern |

### 3. Run Loop (run_loop.rs) — GOOD

**What works well:**
- 11 independent subsystem tasks supervised with JoinSet restart logic.
- MAX_RESTARTS = 5 within 60 seconds — prevents infinite restart loops.
- Linear backoff: 100ms, 200ms, 300ms increments.
- Graceful shutdown via CancellationToken.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 11 | **Degraded state is informational only** — no action taken (e.g., pause new tasks) | Medium | System continues accepting work while degraded |
| 12 | **Event dispatcher may drop events** — broadcast channel size = 64 | Medium | Under heavy load, events are silently lost |
| 13 | **No circuit breaker** — cascading failures restart forever | Medium | System instability under high failure rate |

### 4. Context Compiler (context_compiler.rs) — GOOD

**What works well:**
- Category-budgeted assembly: System → Tools → Knowledge → History → Task.
- Per-category token budgets (configurable percentages).
- UTF-8 boundary-aware truncation.
- Pinned entries preserved (system prompt, safety rules).

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 14 | **4 chars ≈ 1 token heuristic** — varies by model/tokenizer; can be 30%+ off | High | Budget over/under-allocation |
| 15 | **Knowledge blocks are atomic** — oversized episodic recall is all-or-nothing | Medium | Wasted context budget if one block is too large |
| 16 | **No deduplication** — identical context entries can appear multiple times | Low | Token waste |

### 5. Tool Call Parsing (tool_call.rs) — ADEQUATE

**What works well:**
- Regex-based JSON extraction from LLM text.
- Compiled as LazyLock (efficient).
- Intent type validated against enum.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 17 | **Simple regex is fragile** — LLM might format JSON differently (extra whitespace, comments) | High | Tool calls silently missed |
| 18 | **First-match semantics** — multiple tool calls in one response not supported | High | I can only call one tool per turn |
| 19 | **No size limit on parsed payload** — could be huge | Low | Memory risk |

### 6. Injection Scanner — SOLID

**25+ patterns covering:** role override, system prompt exfiltration, delimiter injection, encoded payloads, privilege escalation, data exfiltration, context manipulation, HTML/script injection.

**Strengths:** NFKC normalization prevents homoglyph bypass; XML escaping on source; graduated threat levels.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 20 | **High false-positive rate** — "curl" in legitimate code triggers Medium alert | Medium | Legitimate tool outputs flagged |
| 21 | **No context awareness** — detects "ignore" in benign sentences | Medium | Over-blocking |
| 22 | **Patterns hardcoded** — can't be updated without recompile | Low | Slow response to new attack vectors |

### 7. Intent Validator — GOOD

**Three rules:** Intent loop detection (3+ repeats = Rejected), write-without-read (Suspicious), scope escalation (Suspicious).

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 23 | **Intent loop uses consecutive matching only** — A→B→A→B loop not detected | Medium | Subtle loops slip through |
| 24 | **Write-without-read uses tool name pattern matching** — brittle for non-standard tool names | Low | False positives/negatives |

### 8. Cost Tracker — SOLID

**Strengths:** Atomic counters per agent, CAS-based period reset, micro-USD precision, model downgrade support.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 25 | **Unregistered agents always return Ok** — budget bypass if agent not registered | Medium | Security gap |
| 26 | **Period reset is 24h elapsed, not calendar-day aligned** — confusing for operators | Low | Operational confusion |

### 9. Escalation Manager — ADEQUATE

**Strengths:** Auto-expiry with sweep, SSRF-guarded webhook, soft/hard approval tiers.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 27 | **In-memory only** — pending escalations lost on restart | Critical | Approval decisions lost |
| 28 | **No DNS rebinding check on webhook URLs** | Medium | SSRF bypass via DNS rebinding |
| 29 | **No rate limiting on escalation creation** | Medium | Agent could spam escalations |

---

## Critical Gaps for Pure Agentic Workflow

### Gap A: Hardcoded 10-Iteration Limit
Complex tasks (multi-file refactoring, research, debugging) routinely need 20-50+ tool calls. A 10-iteration cap makes me abandon tasks incomplete. This must be configurable per-task based on complexity.

### Gap B: No State Persistence
Kernel restart = all tasks, escalations, cost snapshots lost. This is unacceptable for:
- Long-running autonomous tasks (hours/days)
- Approval workflows (escalation pending → restart → decision lost)
- Cost tracking (budget resets to zero on restart)

### Gap C: Single Tool Call Per Turn
The tool call parser extracts only the first valid JSON block. Modern agentic loops (Claude, GPT) support parallel tool calls in a single turn. Without this, every tool call costs a full LLM inference round-trip.

---

## Score

| Criterion | Score (1-5) | Notes |
|-----------|------------|-------|
| Architecture | 4.0 | Clean agentic loop, proper supervision, multi-layer validation |
| Correctness | 3.5 | Race conditions guarded, but heuristic error classification |
| Agent Autonomy | 2.0 | 10-iteration cap, single tool call per turn, no configurable limits |
| Reliability | 2.5 | All in-memory, no persistence, event loss under load |
| Security | 4.0 | Injection scanning, coherence checking, capability validation |
| **Overall** | **3.2/5** | Solid architecture, needs operational hardening |
