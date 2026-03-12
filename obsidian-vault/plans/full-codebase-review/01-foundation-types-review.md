---
title: "Phase 1: Foundation Types Review"
tags:
  - review
  - types
  - phase-1
date: 2026-03-13
status: planned
effort: 2h
priority: high
---

# Phase 1: Foundation Types Review

> Review the `agentos-types` and `agentos-sdk-macros` crates — the foundation layer with zero internal dependencies.

---

## Why This Phase

Every other crate depends on `agentos-types`. Bugs here propagate everywhere: incorrect ID semantics, missing error variants, leaky Debug impls on secrets, or broken permission logic would affect the entire system. Review this first to establish a sound foundation.

---

## Current State

- `agentos-types`: 14 files, 2,217 lines — IDs, errors, context window, events, capabilities, tasks, tools, intents
- `agentos-sdk-macros`: 1 file, 214 lines — `#[tool]` proc macro

## Target State

All types reviewed for: correctness, security (no secret leaks), completeness (no missing error variants), and API consistency.

---

## Step 1.1 — IDs, Errors, Small Types (~380 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-types/src/ids.rs` | 59 | `define_id!()` macro, UUID newtypes |
| `crates/agentos-types/src/error.rs` | 100 | `AgentOSError` enum |
| `crates/agentos-types/src/lib.rs` | 45 | Re-exports |
| `crates/agentos-types/src/role.rs` | 25 | `Role` struct |
| `crates/agentos-types/src/secret.rs` | 38 | `SecretEntry`, `SecretMetadata` |
| `crates/agentos-types/src/schedule.rs` | 40 | Schedule types |
| `crates/agentos-types/src/agent.rs` | 43 | `AgentProfile`, `AgentStatus` |
| `crates/agentos-types/src/agent_message.rs` | 76 | Inter-agent message types |

**Checklist:**
- [ ] `define_id!()` produces correct UUID newtype with Serialize/Deserialize/Display/Clone/Hash/Eq
- [ ] `AgentOSError` covers all error variants — no catch-all that swallows context
- [ ] Error variants have meaningful payloads (not just strings where structured data is needed)
- [ ] `SecretEntry` / `SecretMetadata` do not leak plaintext via Debug/Display
- [ ] Schedule types handle timezone edge cases
- [ ] Re-exports in `lib.rs` match actual public items (no orphaned exports)

---

## Step 1.2 — Intent, Tool, Capability (~564 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-types/src/intent.rs` | 116 | `IntentMessage`, `IntentType`, `IntentResult` |
| `crates/agentos-types/src/tool.rs` | 131 | `ToolManifest`, `TrustTier`, `RegisteredTool` |
| `crates/agentos-types/src/capability.rs` | 317 | `PermissionSet`, `CapabilityToken`, `PermissionEntry` |

**Checklist:**
- [ ] `PermissionSet.check()` correctly implements path-prefix matching + deny entries + SSRF blocking
- [ ] Deny entries cannot be bypassed via case sensitivity or encoding
- [ ] `TrustTier` ordering is correct (Core > Verified > Community > Blocked)
- [ ] `CapabilityToken` fields are non-forgeable (proper HMAC coverage)
- [ ] `IntentMessage` covers all necessary fields for audit trail
- [ ] Risk levels are properly ordered and serializable

---

## Step 1.3 — Context Window & Events (~1,056 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-types/src/context.rs` | 677 | `ContextWindow`, `ContextEntry`, `TokenBudget`, `OverflowStrategy` |
| `crates/agentos-types/src/event.rs` | 379 | `EventMessage`, `EventType`, `EventCategory`, `EventSubscription` |

**Checklist:**
- [ ] Token counting is accurate (no off-by-one)
- [ ] `OverflowStrategy` implementations correctly evict entries
- [ ] Context partitioning does not allow one partition to starve another
- [ ] `TokenBudget` enforces hard limits, not just soft limits
- [ ] Event types cover all 83+ audit-relevant operations
- [ ] Event filtering (`EventTypeFilter`) has no logic inversions

---

## Step 1.4 — Task (~171 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-types/src/task.rs` | 171 | `AgentTask`, `TaskState`, `AgentBudget`, `CostSnapshot` |

**Checklist:**
- [ ] Task state machine transitions are well-defined (no invalid transitions possible)
- [ ] `AgentBudget` / `CostSnapshot` handle floating-point precision correctly
- [ ] `PreemptionLevel` ordering is correct
- [ ] `TaskSummary` does not leak sensitive data from task payloads

---

## Step 1.5 — SDK Macros (~214 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-sdk-macros/src/lib.rs` | 214 | `#[tool]` proc macro |

**Checklist:**
- [ ] `#[tool]` proc macro generates correct trait implementations
- [ ] Generated code handles errors properly (does not panic)
- [ ] Permission attributes are correctly parsed and forwarded
- [ ] Edge cases: empty permissions, missing name, unicode in description

---

## Files Changed

No files changed — this is a read-only review phase.

## Dependencies

None — this is the first phase.

## Test Plan

N/A (review-only). Findings are recorded in findings tables.

## Verification

```bash
# Confirm types crate compiles clean
cargo build -p agentos-types
cargo test -p agentos-types
cargo clippy -p agentos-types -- -D warnings
```

---

## Related

- [[Full Codebase Review Plan]]
- [[02-infrastructure-review]]
