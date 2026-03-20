---
title: "Audit #5: LLM Integration & Agent Communication"
tags:
  - audit
  - llm
  - agent-communication
  - events
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 2h
priority: critical
---

# Audit #5: LLM Integration & Agent Communication

> Evaluating the LLM adapter layer, agent registry, inter-agent messaging, and event system — the infrastructure that connects my brain to the world and to other agents.

---

## Scope

- `crates/agentos-llm/` — LLMCore trait + 6 adapters (Anthropic, OpenAI, Ollama, Gemini, Custom, Mock)
- `crates/agentos-kernel/src/agent_registry.rs` — Agent registration + persistence
- `crates/agentos-kernel/src/agent_message_bus.rs` — Inter-agent messaging with Ed25519 signatures
- `crates/agentos-kernel/src/event_bus.rs` — Event subscription, filter evaluation, throttling
- `crates/agentos-kernel/src/event_dispatch.rs` — Event emission, HMAC signing, task triggering

---

## Verdict: MIXED — LLM layer has critical gaps; event system is well-designed

The event system is sophisticated (filter expressions, throttling, chain-depth loop detection). However, the LLM adapters have critical issues: OpenAI tool calling is not implemented, uncertainty parsing is never invoked, and max_tokens is hardcoded. The agent message bus has a **pubkey registration trust vulnerability**.

---

## Findings

### 1. LLM Adapters — INCOMPLETE

**LLMCore Trait:**
- Clean 5-method trait: `infer()`, `infer_stream()`, `capabilities()`, `health_check()`, `provider_name()/model_name()`.
- Default `infer_stream` falls back to `infer()` — good for adapters without streaming.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 1 | **OpenAI adapter has NO tool call extraction** — `supports_tool_calling: true` but tool_calls array never parsed from response | Critical | OpenAI-backed agents cannot use any tools |
| 2 | **Anthropic max_tokens hardcoded to 4096** — can't request longer outputs | High | Long-form generation truncated silently |
| 3 | **Uncertainty parsing never invoked** — `UncertaintyDeclaration` type exists, `parse_uncertainty()` function exists, but no adapter calls it | High | Feature promised but completely non-functional |
| 4 | **Ollama context window hardcoded at 8,192** — many models support 32K+ | Medium | Under-utilization of model capabilities |
| 5 | **No adapter-level retry logic** — single timeout kills the request | Medium | Flaky API connections cause task failures |
| 6 | **ToolResult mapped to "user" role** in Anthropic adapter — workaround since Anthropic API doesn't have a tool_result role; may confuse the model | Low | Potential quality degradation |

**Pricing Table:**
- Current as of March 2026 — covers Anthropic, OpenAI, Google Gemini, Ollama (free).
- `calculate_inference_cost()` is correct with per-token pricing.

### 2. Agent Registry — SOLID

**What works well:**
- Dual indexing (by ID and name) for O(1) lookups.
- Persistent storage with atomic write (temp file + rename).
- Role-based permission composition (`compute_effective_permissions()`).
- Base role auto-created with fs.user_data permission.
- Agents loaded from disk marked Offline (correct).

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 7 | **JSON parse failure silently returns empty registry** — no warning logged, all agents lost | Medium | Silent data loss on corrupted registry file |
| 8 | **No backup/recovery mechanism** — single file, no versioning | Low | Corrupted file = total agent loss |

### 3. Agent Message Bus — SECURITY VULNERABILITY

**What works well:**
- Ed25519 signature verification on all messages.
- Bounded inbox (256 messages) prevents unbounded memory growth.
- History capped at 10,000 entries.
- Events emitted: DirectMessageReceived, MessageDeliveryFailed, AgentImpersonationAttempt, AgentUnreachable.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 9 | **Pubkey registration has no authentication** — any component can call `register_pubkey(agent_id, attacker_key)` and then forge messages as that agent | Critical | Authentication bypass vulnerability |
| 10 | **Message expiry checked AFTER signature verification** — expired messages still pay crypto cost | Medium | DoS via expired message flooding |
| 11 | **Keys stored in-memory only** — lost on restart, must re-register | Medium | Agent identity not persistent |

**Attack Vector (Issue #9):**
1. Malicious component calls `bus.register_pubkey(alice_id, attacker_pubkey)`
2. Attacker crafts message signed with attacker_privkey
3. Signature verifies — message accepted as from Alice
4. Audit log shows Alice as sender

**Recommendation:** Move pubkey registration to kernel boot only, store keys in encrypted vault, make agent_id → pubkey immutable after registration.

### 4. Event Bus — WELL-DESIGNED

**What works well:**
- Subscription CRUD with enable/disable.
- Event type filter: Exact, Category, All.
- Sophisticated payload filter expression parser (field == value, AND, IN, CONTAINS, comparisons).
- Three throttle policies: None, MaxOncePerDuration, MaxCountPerDuration.
- Fail-open filter policy (intentional — invalid filters match everything).
- Chain-depth loop detection in event dispatch.
- HMAC-signed events for tamper detection.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 12 | **O(n) subscription evaluation on every event** — full scan of all subscriptions | Medium | Becomes bottleneck at 10K+ events/sec with 1K subscriptions |
| 13 | **Async throttle check within subscription loop** — serializes evaluation | Medium | Increased latency under load |
| 14 | **Subscription cloning on every match** — unnecessary allocation | Low | Memory churn |

### 5. Event Dispatch — SOLID

**What works well:**
- Single emission point (`emit_signed_event()`) prevents signature skew.
- Canonical format: `event_id|event_type|timestamp|chain_depth`.
- Audit log integration on every event.
- Triggered task creation with proper permission recomputation.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 15 | **Event loss possible** — broadcast channel capacity = 64; if subscribers can't keep up, events dropped silently | Medium | Agents may miss events they subscribed to |

---

## Critical Gaps for Pure Agentic Workflow

### Gap A: OpenAI Tool Calling Broken
If I'm backed by an OpenAI model, I literally cannot use any tools. The adapter parses only `choices[0].message.content` as plain text — it never extracts `choices[0].message.tool_calls`. This makes OpenAI agents non-functional for agentic workflows.

### Gap B: No Parallel Tool Calls
The LLM response parsing extracts only one tool call per turn. Modern LLMs (Claude, GPT-4) can emit multiple tool calls in a single response. Without parallel tool call support, every action costs a full inference round-trip.

### Gap C: Agent Identity Not Persistent
Ed25519 keys are in-memory only. On kernel restart, all agent identities are lost. Messages signed before restart can't be verified after restart. This makes long-lived multi-agent systems unreliable.

---

## Score

| Criterion | Score (1-5) | Notes |
|-----------|------------|-------|
| LLM Adapter Quality | 2.5 | Anthropic works, OpenAI broken for tools, uncertainty dead code |
| Agent Communication | 3.0 | Ed25519 signing is good, but pubkey trust model is broken |
| Event System | 4.0 | Sophisticated filter expressions, throttling, loop detection |
| Multi-Agent Coordination | 3.0 | Message bus works but bounded, keys not persistent |
| Production Readiness | 2.5 | Critical issues in OpenAI + pubkey registration |
| **Overall** | **3.0/5** | Event system excellent; LLM + messaging need critical fixes |
