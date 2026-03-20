---
title: "Audit #1: Type System & Intent Protocol"
tags:
  - audit
  - types
  - intent
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 2h
priority: critical
---

# Audit #1: Type System & Intent Protocol

> Evaluating `agentos-types` as the foundational language an AI agent must speak to operate within AgentOS.

---

## Scope

Crate: `crates/agentos-types/src/` — all 15 modules.

As an AI agent, this is the **first thing I interact with**. Every tool call, every memory access, every message I send is encoded using these types. If the type system is awkward, ambiguous, or incomplete, I cannot work effectively.

---

## Verdict: GOOD — with important gaps

The type system is well-designed for its purpose. Types are clean, well-documented, use proper Rust idioms (`thiserror`, `serde`, newtype IDs). However, several gaps would limit my effectiveness in a pure agentic workflow.

---

## Findings

### 1. Intent Protocol — SOLID

**What works well:**
- `IntentMessage` is a clean envelope: ID, capability token, target, payload, priority, timeout, trace ID — everything I need to make a request.
- `IntentType` covers the full spectrum: Read, Write, Execute, Query, Observe, Delegate, Message, Broadcast, Escalate, Subscribe, Unsubscribe.
- `IntentTarget` correctly separates Tool, Kernel, Agent, Hardware, and Broadcast targets.
- `SemanticPayload` with schema name + JSON data is LLM-friendly — I can construct payloads without knowing Rust structs.
- `IntentResult` gives me structured feedback: status, payload, error, execution time.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 1 | No `IntentType::Create` or `IntentType::Delete` — I must use `Write` for both creation and deletion | Low | Minor ambiguity in intent semantics |
| 2 | `IntentCoherenceResult::Suspicious` has `confidence: f32` but no threshold documentation — I don't know what score triggers blocking | Medium | Unpredictable behavior when my actions are flagged |
| 3 | `ActionRiskLevel` has 5 levels but no mapping to IntentTypes — which risk level does `Delegate` get? | Medium | I can't predict when I'll be blocked |
| 4 | `HardwareResource` only has 4 variants (System, Process, Network, LogReader) — but HAL has 7 drivers (GPU, Storage, Sensor missing) | High | I cannot target GPU/Storage/Sensor via intents |

### 2. ID System — EXCELLENT

**What works well:**
- Newtype pattern via `define_id!()` macro — type-safe, prevents mixing up TaskID with AgentID.
- Derives: Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord — complete set.
- `FromStr` and `Display` — I can parse IDs from strings and format them.
- 12 distinct ID types covering all domains.

**No issues found.** This is production-ready.

### 3. Task System — GOOD

**What works well:**
- `TaskState` has a proper state machine with `can_transition_to()` and `transition()` — prevents invalid state changes.
- `TaskReasoningHints` with `ComplexityLevel` and `PreemptionLevel` — I can signal to the scheduler how to handle my work.
- `AgentBudget` with `ModelDowngradeTier` — graceful degradation when approaching limits.
- `TriggerSource` tracks event provenance — I can understand why a task was created.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 5 | `AgentTask.history: Vec<IntentMessage>` stores full intent history in the task — this will grow unbounded for long-running tasks | Medium | Memory bloat, serialization overhead |
| 6 | No `TaskState::Suspended` — `BudgetAction::Suspend` exists but there's no matching task state | High | Budget suspension has no representation in the state machine |
| 7 | `TaskSummary.prompt_preview` is "first 100 chars" — no method to generate it, relies on caller truncation | Low | Inconsistent preview generation |
| 8 | `timeout: Duration` on AgentTask but `max_wall_time_seconds: u64` on AgentBudget — two different timeout mechanisms with no documented precedence | Medium | Confusion about which timeout applies |

### 4. Capability & Permission System — SOLID

**What works well:**
- `PermissionSet` with deny-takes-precedence is correct security design.
- Path-prefix matching with segment boundary checking (`/home/user` doesn't match `/home/username`) — excellent edge case handling.
- SSRF protection built into `is_denied()` — blocks private ranges, IPv6 mapped, ULA, case variations.
- Permission expiry support with `expires_at`.
- Comprehensive test suite (14 tests covering edge cases).

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 9 | `CapabilityToken.allowed_tools: BTreeSet<ToolID>` — tools must be pre-registered by ID, but I discover tools by name. There's no `BTreeSet<String>` for name-based access | Medium | I can't request access to tools I haven't seen yet |
| 10 | No wildcard permission support — I can't request `fs:/home/user/**` or `net:*.anthropic.com` | Low | Must enumerate every path individually |
| 11 | `PermissionOp` only has Read/Write/Execute — no Query or Observe, but `IntentType` has both. Mismatch | Medium | Intent types that don't map to permission ops |

### 5. Context Window — GOOD

**What works well:**
- Four overflow strategies: FIFO, Summarize, SlidingWindow, SemanticEviction — different agents can choose their strategy.
- `SemanticEviction` uses composite scoring (importance * 0.4 + recency * 0.3 + reference_count * 0.3) — smart eviction.
- `ContextPartition` (Active vs Scratchpad) — I can maintain working notes without polluting the LLM context.
- `ContextCategory` (System, Tools, Knowledge, History, Task) — structured context compilation.
- `TokenBudget` with per-category allocation and validation.
- `compress_oldest()` creates readable summaries, not just drops.
- UTF-8 safe truncation in summaries.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 12 | `estimated_tokens()` uses `chars().count() / 4 + 1` — this is a rough heuristic. For languages like Chinese/Japanese this over-counts significantly (1 char ≈ 1-2 tokens, not 0.25) | Medium | Inaccurate token budgeting for non-Latin text |
| 13 | `max_entries: usize` is entry-count-based, not token-based — a 5-token entry and a 5000-token entry count the same | High | Token budget can be exceeded or underused |
| 14 | No method to query remaining budget per category — I can't ask "how many tokens do I have left for Knowledge?" | Medium | Blind context management |
| 15 | `Summarize` strategy creates summary entries with `role: System` — these accumulate and are never evicted (system role check in FIFO) | Medium | Summary entries pile up over long conversations |
| 16 | `set_partition()` only affects the most recent non-system entry — no way to partition by index or ID | Low | Limited scratchpad control |

### 6. Event System — SOLID

**What works well:**
- 50+ event types organized into 10 categories — comprehensive coverage.
- `EventMessage` with HMAC signature and chain_depth for loop detection — security-conscious.
- `EventSubscription` with filter, priority, throttle, and enable/disable — flexible.
- `ThrottlePolicy` with rate-limiting — prevents event floods.
- `#[non_exhaustive]` on `EventType` — forward-compatible.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 17 | No `EventType::BudgetWarning` or `EventType::BudgetExhausted` — cost events aren't in the event system | Medium | I can't subscribe to budget alerts |
| 18 | `EventMessage.payload: serde_json::Value` is untyped — I must guess the schema per event type | Medium | Error-prone event handling |
| 19 | No `EventType::ToolCallStarted` / `ToolCallCompleted` — only `ToolExecutionFailed` exists | Low | Can't observe successful tool execution |

### 7. Agent & Message System — GOOD

**What works well:**
- `AgentProfile` with Ed25519 public key — cryptographic identity.
- `AgentMessage` with TTL, expiry, Ed25519 signature, canonical signing payload — secure messaging.
- `MessageContent` with Text, Structured, TaskDelegation, TaskResult — covers multi-agent coordination.
- `MessageTarget` supports Direct (by ID), DirectByName, Group, Broadcast.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 20 | `AgentMessage.signing_payload()` uses `timestamp.timestamp()` (Unix seconds) — loses sub-second precision, two messages in the same second would have the same signing input | Low | Theoretical replay vulnerability |
| 21 | No `MessageContent::Error` variant — if I need to report failure to another agent I must use `Text` or `Structured` | Low | No standard error protocol between agents |

### 8. Error System — GOOD

**What works well:**
- `AgentOSError` with `thiserror` — proper error hierarchy.
- Specific variants for every domain: tool, kernel, capability, vault, sandbox, event.
- `Clone` implemented (using `Arc<io::Error>` for IO) — errors can be sent across channels.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 22 | No `AgentOSError::BudgetExceeded` variant — budget enforcement has no standard error | Medium | Budget violations use generic `KernelError` |
| 23 | No `AgentOSError::RateLimited` variant — rate limiting has no standard error | Low | Rate limit hits are indistinguishable from other errors |
| 24 | `AgentOSError::AgentNotFound(String)` takes String, but `TaskNotFound(TaskID)` takes ID — inconsistent | Low | Different lookup patterns |

### 9. Registry Query Traits — EXCELLENT

**What works well:**
- `AgentRegistryQuery` and `TaskQuery` as traits in types crate — breaks circular dependency.
- `AgentRegistrySnapshot` and `TaskSnapshot` — immutable point-in-time views.
- `TaskIntrospectionSummary` gives me just enough to understand task state without full `AgentTask`.

**No significant issues.** Well-designed abstraction boundary.

---

## Critical Gaps for Pure Agentic Workflow

### Gap A: No Standard Tool Call Format

The type system defines `IntentMessage` → `SemanticPayload` but there's **no standard type for how an LLM emits a tool call**. The gap between "LLM says call tool X with args Y" and "construct an IntentMessage with CapabilityToken, target ToolID, etc." is bridged entirely in kernel code. As an agent, I need a simpler intermediate format:

```rust
struct ToolCallRequest {
    tool_name: String,        // I know tools by name
    arguments: serde_json::Value,
    // kernel fills in: capability token, tool ID, trace ID, etc.
}
```

### Gap B: No Agent Self-Introspection Types

There's no type representing "what I know about myself." As an agent, I need:
- My current permissions (already in `CapabilityToken` but not easily queryable)
- My budget status (exists as `CostSnapshot` but no tool to query it directly)
- My registered tools (exists in kernel but no snapshot type for agent-side access)
- My active subscriptions (no query type)

### Gap C: No Streaming/Partial Result Types

`IntentResult` is a single response. For long-running operations (web fetch, large file read, pipeline execution), there's no type for streaming partial results back to me. This forces synchronous, all-or-nothing tool execution.

---

## Recommendations (Priority Order)

1. **[Critical]** Add `TaskState::Suspended` to match `BudgetAction::Suspend`
2. **[Critical]** Add missing `HardwareResource` variants (GPU, Storage, Sensor)
3. **[High]** Add `AgentOSError::BudgetExceeded` and `RateLimited` variants
4. **[High]** Add `EventType::BudgetWarning`, `BudgetExhausted`, `ToolCallStarted`, `ToolCallCompleted`
5. **[High]** Add a `ToolCallRequest` intermediate type for LLM → kernel tool calls
6. **[Medium]** Map `PermissionOp` to cover all `IntentType` variants (add Query, Observe)
7. **[Medium]** Add `estimated_tokens_remaining(category)` method to ContextWindow
8. **[Medium]** Document `IntentCoherenceResult::Suspicious` threshold behavior
9. **[Low]** Make `estimated_tokens()` configurable (chars-per-token ratio)
10. **[Low]** Add `MessageContent::Error` variant for inter-agent error reporting

---

## Test Coverage Assessment

| Module | Unit Tests | Coverage Quality |
|--------|-----------|-----------------|
| capability.rs | 14 tests | Excellent — SSRF, deny, expiry, prefix matching |
| context.rs | 7 tests | Good — overflow strategies, pinning, eviction |
| event.rs | 4 tests | Adequate — category mapping, serialization |
| task.rs | 0 tests | **Missing** — state machine transitions untested |
| intent.rs | 0 tests | **Missing** — serialization untested |
| ids.rs | 0 tests | Missing — but macro-generated, low risk |
| agent_message.rs | 0 tests | **Missing** — signing payload, expiry untested |

**Recommendation:** Add tests for `TaskState::transition()`, `IntentMessage` serialization roundtrip, and `AgentMessage::is_expired()`.

---

## Score

| Criterion | Score (1-5) | Notes |
|-----------|------------|-------|
| Completeness | 3.5 | Missing budget errors, suspended state, HW resources |
| Correctness | 4.5 | Well-tested where tests exist; proper edge case handling |
| Agent Ergonomics | 3.0 | No self-introspection types, no streaming, name-vs-ID friction |
| Security | 4.5 | SSRF, deny-first, expiry, HMAC — all solid |
| Documentation | 3.5 | Good doc comments, but missing threshold/precedence docs |
| **Overall** | **3.8/5** | Solid foundation with addressable gaps |
