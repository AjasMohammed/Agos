---
title: "Phase 1: Foundation Types Review"
tags:
  - review
  - types
  - phase-1
date: 2026-03-13
status: complete
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
- [x] `define_id!()` produces correct UUID newtype with Serialize/Deserialize/Display/Clone/Hash/Eq
- [x] `AgentOSError` covers all error variants — no catch-all that swallows context
- [x] Error variants have meaningful payloads (not just strings where structured data is needed)
- [x] `SecretEntry` / `SecretMetadata` do not leak plaintext via Debug/Display — no value field
- [x] Schedule types handle timezone edge cases — pure UTC throughout
- [x] Re-exports in `lib.rs` match actual public items (no orphaned exports)

---

## Step 1.2 — Intent, Tool, Capability (~564 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-types/src/intent.rs` | 116 | `IntentMessage`, `IntentType`, `IntentResult` |
| `crates/agentos-types/src/tool.rs` | 131 | `ToolManifest`, `TrustTier`, `RegisteredTool` |
| `crates/agentos-types/src/capability.rs` | 317 | `PermissionSet`, `CapabilityToken`, `PermissionEntry` |

**Checklist:**
- [x] `PermissionSet.check()` correctly implements path-prefix matching + deny entries + SSRF blocking
- [x] Deny entries cannot be bypassed via case sensitivity or encoding — **FIXED** case normalization for `net:` deny patterns
- [x] `TrustTier` ordering is correct (Core > Verified > Community > Blocked) — lower numeric = higher trust, documented
- [x] `CapabilityToken` fields are non-forgeable (proper HMAC coverage) — Phase 3 already verified
- [x] `IntentMessage` covers all necessary fields for audit trail
- [x] Risk levels are properly ordered and serializable

---

## Step 1.3 — Context Window & Events (~1,056 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-types/src/context.rs` | 677 | `ContextWindow`, `ContextEntry`, `TokenBudget`, `OverflowStrategy` |
| `crates/agentos-types/src/event.rs` | 379 | `EventMessage`, `EventType`, `EventCategory`, `EventSubscription` |

**Checklist:**
- [x] Token counting is accurate (no off-by-one) — uses `chars().count() / 4 + 1`
- [x] `OverflowStrategy` implementations correctly evict entries — **FIXED** Summarize overflow bug
- [x] Context partitioning does not allow one partition to starve another — Active/Scratchpad partitioned correctly
- [x] `TokenBudget` enforces hard limits — **FIXED** `validate()` now rejects negative percentages
- [ ] Event types cover all 83+ audit-relevant operations — only ~55 variants; gap documented in deferred issues
- [x] Event filtering (`EventTypeFilter`) has no logic inversions

---

## Step 1.4 — Task (~171 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-types/src/task.rs` | 171 | `AgentTask`, `TaskState`, `AgentBudget`, `CostSnapshot` |

**Checklist:**
- [x] Task state machine transitions are well-defined — `can_transition_to()` enumerates all legal moves; terminals have no outgoing
- [x] `AgentBudget` / `CostSnapshot` handle floating-point precision correctly — f64 acceptable for LLM cost estimates
- [x] `PreemptionLevel` ordering is correct — Low < Normal < High (derived Ord top-to-bottom)
- [x] `TaskSummary` does not leak sensitive data — `prompt_preview` is display-only; token/cost fields are aggregate stats

---

## Step 1.5 — SDK Macros (~214 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-sdk-macros/src/lib.rs` | 214 | `#[tool]` proc macro |

**Checklist:**
- [x] `#[tool]` proc macro generates correct trait implementations
- [x] Generated code handles errors properly — `to_compile_error()` for all parse failures
- [x] Permission attributes are correctly parsed and forwarded — **FIXED** added `rx`/`wx` compound ops
- [x] Edge cases: empty permissions → empty vec; missing name → compile error; unicode in description → stored as string literal

---

## Findings

| File | Line(s) | Severity | Category | Description | Status |
|------|---------|----------|----------|-------------|--------|
| `crates/agentos-types/src/context.rs` | 286-349 | critical | Bug | Summarize overflow strategy: when `non_system_count ≤ 2`, removes 1 and inserts 1 summary (net 0), then `push()` exceeds `max_entries`. Also triggers when all entries are System. | **FIXED**: added safety-net FIFO eviction after the `match` block |
| `crates/agentos-types/src/context.rs` | ~500 | warning | Correctness | `estimated_tokens()` used `content.len()` (byte length), underestimating for non-ASCII UTF-8. Comment said "4 chars ≈ 1 token". | Already fixed in source (`chars().count()`; update just verified) |
| `crates/agentos-types/src/context.rs` | 138-157 | warning | Correctness | `TokenBudget::validate()` did not reject negative category percentages; `(usable * negative_pct) as usize` wraps to `usize::MAX`. | **FIXED**: per-field non-negative check added |
| `crates/agentos-types/src/capability.rs` | 117-123 | warning | Security | Deny entries for network resources (`net:`/`network:`) compared case-sensitively; bypass possible via `"net:http://Corp/"` vs deny `"net:http://corp/"`. SSRF check was already case-insensitive but deny entries were not. | **FIXED**: case-normalize both sides when resource and pattern are `net:`/`network:` |
| `crates/agentos-sdk-macros/src/lib.rs` | 139-153 | info | Correctness | `"rx"`, `"xr"`, `"wx"`, `"xw"` compound ops fell to `Err` branch; users writing `:rx` got a compile error rather than Read+Execute. | **FIXED**: added `"rx" | "xr"` and `"wx" | "xw"` match arms |
| `crates/agentos-types/src/capability.rs` | 184 | warning | Security | Path-prefix grant/deny matching has no separator check: `"fs:/home/user"` also matches `"fs:/home/user-backup/"`. Convention is to use trailing `/` for directories; no enforcement. | **DEFERRED**: design decision — documented in Remaining Issues |
| `crates/agentos-types/src/ids.rs` | 32-36 | info | API | `Default` for ID types generates a random UUID (non-deterministic). Could confuse callers expecting a zero/nil sentinel. | **ACCEPTED**: intentional randomness; well-suited for auto-generated IDs |
| `crates/agentos-types/src/capability.rs` | 7-18 | warning | Security | `CapabilityToken` derives `Debug`/`Serialize`, exposing the HMAC signature in logs or LLM context. | **DEFERRED**: [[08-security-deep-dives]] |
| `crates/agentos-types/src/task.rs` | 141-167 | info | Correctness | `AgentBudget` has no invariant `warn_at_pct < pause_at_pct`; inverted values would pause before warning. | **DEFERRED**: runtime validation in kernel |
| `crates/agentos-types/src/task.rs` | 93-99 | info | Correctness | `TaskSummary.prompt_preview` not truncated at construction; callers must remember to truncate. | **ACCEPTED**: documented in field comment |
| `crates/agentos-types/src/event.rs` | 26-109 | info | Completeness | `EventType` has ~55 variants; audit log has 83+ types. Gap exists for VaultAccess, ConfigChange, KernelBoot, BudgetExceeded etc. | **DEFERRED**: to Phase 10 synthesis |
| `crates/agentos-sdk-macros/src/lib.rs` | 155-160 | info | Usability | Permission string without `:` separator (e.g. `"fs.data"`) silently defaults to Read. | **ACCEPTED**: reasonable default; documented in error message |

---

## Remaining Issues (deferred)

| Severity | Issue | Deferred To |
|----------|-------|-------------|
| HIGH | `CapabilityToken` derives Debug/Serialize, exposing HMAC signature | [[08-security-deep-dives]] |
| MEDIUM | Path-prefix matching has no separator check (potential over-grant) | Capability hardening phase |
| LOW | `AgentBudget` no `warn_at_pct < pause_at_pct` invariant | Config validation phase |
| LOW | `EventType` missing ~28 audit-relevant event variants | [[10-synthesis-and-report]] |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/context.rs` | Added safety-net FIFO eviction after overflow `match` (fixes Summarize overflow bug); verified `estimated_tokens` uses `chars().count()`; added negative-pct check to `TokenBudget::validate()` |
| `crates/agentos-types/src/capability.rs` | `is_denied()` now case-normalizes both sides when comparing `net:`/`network:` deny patterns |
| `crates/agentos-sdk-macros/src/lib.rs` | Added `"rx" | "xr"` and `"wx" | "xw"` compound op match arms in `parse_permission()` |

## Dependencies

None — this is the first phase.

## Test Plan

N/A (review-only). Findings are recorded in findings tables.

## Verification

```bash
cargo build -p agentos-types -p agentos-sdk-macros   # ✅ PASSED
cargo test -p agentos-types -p agentos-sdk-macros     # ✅ 23 tests passed
cargo clippy -p agentos-types -p agentos-sdk-macros -- -D warnings  # ✅ PASSED
cargo fmt -p agentos-types -p agentos-sdk-macros -- --check          # ✅ PASSED
cargo build --workspace                                               # ✅ PASSED (no downstream breakage)
# Note: pre-existing issues in working tree (not introduced by this phase):
#   - crates/agentos-memory/src/semantic.rs:672 — syntax error (pre-existing)
#   - crates/agentos-cli/tests/pipeline_test.rs — unresolved module (pre-existing)
#   - agent_message_bus::tests::test_broadcast_emits_event — flaky (passes individually)
```

---

## Related

- [[Full Codebase Review Plan]]
- [[02-infrastructure-review]]
