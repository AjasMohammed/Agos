---
title: Production Readiness Audit — All 12 Specs
tags:
  - kernel
  - security
  - audit
  - phase-v3
  - next-steps
date: 2026-03-11
status: complete
effort: 4-6 weeks to production
priority: critical
---

# Production Readiness Audit — All 12 Specs

> Deep audit of all 12 implementation spec items against the actual codebase. Identifies what is implemented, what is missing, and what blocks production deployment.

---

## Current State

- **Build:** `cargo build --workspace` passes (1 dead-code warning in vault.rs)
- **Tests:** 218 tests pass, 0 failures, 2 ignored
- **Clippy:** 10 warnings (all minor: print_literal, needless_borrows)

All 12 specs have *some* implementation but **none are fully production-ready**. The Index.md dashboard marks everything "Done" but this audit reveals significant gaps between "code exists" and "production-ready enforcement."

---

## Overall Scorecard

| Spec | Title | Impl % | Enforced? | Prod-Ready? |
|------|-------|--------|-----------|-------------|
| #1 | Capability-Signed Skill Registry | 60% | Partial | NO |
| #2 | Kernel-Owned Permission Matrix | 55% | Partial | NO |
| #3 | Encrypted Secrets Vault | 75% | Partial | NO |
| #4 | Cost-Aware Task Scheduler | 80% | Yes | CLOSE |
| #5 | Immutable Merkle Audit Trail | 70% | Yes | CLOSE |
| #6 | Prompt Injection Scanner | 75% | Partial | NO |
| #7 | Agent Identity & IAM | 60% | NO | NO |
| #8 | Concurrent Resource Arbitration | 75% | Partial | NO |
| #9 | HAL with Per-Agent Gating | 40% | NO | NO |
| #10 | Multi-Agent Coordination | 60% | NO | NO |
| #11 | Context/Memory Architecture | 70% | Partial | NO |
| #12 | Approval Gates | 65% | Partial | NO |

**Overall: ~65% implemented, NOT production-ready**

---

## Goal / Target State

All 12 specs fully enforced end-to-end, with integration tests covering multi-spec failure scenarios. Safe for deployment with untrusted community tools and multiple concurrent agents.

---

## Spec-by-Spec Gap Analysis

### Spec #1: Capability-Signed Skill Registry (60%)

**Working:**
- Ed25519 signing verification (signing.rs)
- TrustTier enum (Core/Verified/Community/Blocked)
- Kernel refuses Blocked tools
- Tests for valid/invalid/tampered manifests

**Missing (critical):**
- No revocation list (CRL) — compromised tools cannot be globally revoked
- No foundation key pinning — Core tools accepted unconditionally without embedded pubkey
- No 48-hour automated sandboxed scan for community tools
- Comment in signing.rs: "In production hardened build this would verify..." — not done

**Files:** `crates/agentos-tools/src/signing.rs`, `crates/agentos-types/src/tool.rs`, `crates/agentos-kernel/src/tool_registry.rs`

---

### Spec #2: Kernel-Owned Permission Matrix (55%)

**Working:**
- PermissionSet with rwx + expiry + deny entries
- Path-prefix matching for filesystem scopes
- SSRF blocking (private IP ranges)
- CLI: grant/revoke/show/profile commands
- CapabilityToken HMAC validation before tool execution

**Missing (critical):**
- No hardware permission gating — matrix claims camera/GPU control but HAL not wired
- No inter-agent permission enforcement — agents message freely without `can_message` whitelist
- No model allowlist — agents can call expensive models despite budget intent
- Permission matrix not persistent across restarts — rebuilt from scratch
- Seccomp sandbox exists but not integrated with permission matrix rules

**Files:** `crates/agentos-types/src/capability.rs`, `crates/agentos-capability/src/engine.rs`

---

### Spec #3: Encrypted Secrets Vault (75%)

**Working:**
- AES-256-GCM encryption at rest
- Argon2id key derivation
- ZeroizingString (secrets wiped on drop)
- Proxy tokens (secrets not exposed to tools)
- Per-secret access logging (agent_id, task_id, timestamp)
- Secret rotation without downtime
- Single-use proxy tokens with TTL expiry

**Missing (critical):**
- No scope/owner enforcement — any agent can request ANY secret by name
- No emergency lockdown (`agentsecret lockdown` command missing)
- No Shamir's Secret Sharing for multi-party key recovery
- No TPM-backed key derivation
- Proxy tokens in-memory only — kernel restart invalidates all active tokens

**Files:** `crates/agentos-vault/src/vault.rs`

---

### Spec #4: Cost-Aware Task Scheduler (80%)

**Working:**
- Real-time token + cost metering per inference
- Provider rate tables (Anthropic, OpenAI, Ollama)
- Multi-tiered enforcement: warn (80%) -> pause (95%) -> hard-limit (100%)
- Model downgrade path (switch to cheaper model)
- Cost attribution audit log
- Tool call budgeting
- Pre-inference budget check
- CLI `cost show` with table output

**Missing:**
- No parent budget hierarchy — 5 agents x $5/day = $25, no org-level cap
- No wall-time limits (`max_wall_time_seconds` not in AgentBudget)
- No external notifications (Slack/Telegram) for budget warnings
- Pricing hardcoded at compile-time — no runtime updates
- Per-task cost attribution is agent-level only

**Files:** `crates/agentos-kernel/src/cost_tracker.rs`, `crates/agentos-types/src/task.rs`

---

### Spec #5: Immutable Merkle Audit Trail + Rollback (70%)

**Working:**
- Merkle hash chain with SHA-256 per-entry + prev_hash linking
- 83 audit event types
- `verify_chain()` DFS tamper detection
- Snapshot system (file-based JSON, 72-hour expiry)
- Auto-snapshot before write ops + budget exhaustion
- Manual rollback via CLI (`audit rollback --task=<id> --snapshot=<ref>`)
- `sweep_expired_snapshots()` every 10 minutes

**Missing:**
- No auto-resumption from snapshot on task recovery
- Snapshots stored as plaintext JSON (not encrypted)
- No integrity binding between snapshot and audit entry (no HMAC)
- Rollback does not validate context state consistency

**Files:** `crates/agentos-audit/src/log.rs`, `crates/agentos-kernel/src/snapshot.rs`

---

### Spec #6: Prompt Injection Scanner (75%)

**Working:**
- 22 regex patterns across 8 categories (role override, exfil, delimiters, encoded payloads, privilege escalation, context manipulation, HTML/markdown)
- Threat levels: High/Medium/Low
- Taint wrapping with `<user_data taint="..." source="..." patterns="...">` tags
- Standing instruction in system prompt about `<user_data>` tags
- High-threat auto-escalation (blocks task, creates PendingEscalation)
- 14 unit tests

**Missing (critical):**
- Stage 2 (Semantic Classifier) — no LLM-based analysis, regex only
- Stage 3 (Taint Propagation) — taint applied at boundary only, no flow tracking
- Scanner NOT called on user-provided prompts — only tool outputs
- Pattern database hardcoded — no external updates
- No false positive reporting or retraining pipeline

**Files:** `crates/agentos-kernel/src/injection_scanner.rs`

---

### Spec #7: Agent Identity & IAM (60%)

**Working:**
- Ed25519 keypair generation and vault-backed persistence
- Message signing and verification (sign_message/verify_signature)
- Identity revocation (removes signing key from vault)
- 7 unit tests

**Missing (critical):**
- NO agent authentication on bus — CLI can impersonate any agent without challenge-response
- Cross-agent message signing infrastructure exists but NOT enforced in message bus
- No key rotation mechanism
- No online/offline status tracking in Agent Registry
- No delegation tokens for inter-agent operations

**Files:** `crates/agentos-kernel/src/identity.rs`

---

### Spec #8: Concurrent Resource Arbitration (75%)

**Working:**
- Shared/Exclusive lock modes
- FIFO waiter queue
- Deadlock detection via DFS wait-for graph
- TTL auto-release with periodic sweep
- Lock release + waiter waking
- CLI: resource list, release, release-all
- 8 unit tests

**Missing:**
- No priority preemption — FIFO only
- Resource arbiter NOT called during tool execution — enforcement gap
- No device resource integration with HAL
- No contention metrics (avg wait time, peak queue depth)
- Deadlock DFS runs in std::sync::Mutex — may block async executor

**Files:** `crates/agentos-kernel/src/resource_arbiter.rs`

---

### Spec #9: HAL with Per-Agent Gating (40%)

**Working:**
- HardwareRegistry with DeviceEntry (id, type, status, granted_agents)
- Device lifecycle: quarantine -> approve -> deny
- Per-agent access checks
- 7 unit tests

**Missing (critical):**
- NOT integrated with resource arbiter or task executor
- No device discovery (no /sys/ scanning, no hot-plug detection)
- No GPU Resource Manager (no time-slicing, no VRAM quotas)
- No CLI commands for device management
- No approval workflow UI
- No audit events for device operations

**Files:** `crates/agentos-hal/src/registry.rs`, `crates/agentos-hal/src/lib.rs`

---

### Spec #10: Multi-Agent Coordination (60%)

**Working:**
- Pipeline engine with topological sort, dependency detection, retry logic
- Agent message bus with Direct/Broadcast/Group delivery
- Message TTL enforcement with expired message rejection
- Ed25519 signature field + signing payload generation
- 80+ pipeline tests, 5 message bus tests

**Missing (critical):**
- Message signature verification NOT enforced — message forgery possible
- No inter-agent permission enforcement (`can_message` whitelist missing)
- No capability-scoped messages
- No message persistence — lost on kernel restart
- No stage output validation/checksumming between pipeline steps

**Files:** `crates/agentos-pipeline/src/engine.rs`, `crates/agentos-kernel/src/agent_message_bus.rs`

---

### Spec #11: Context Window Management & Memory (70%)

**Working:**
- Tier 1 (Working): Rolling context window, 4 overflow strategies (FIFO, Summarize, SlidingWindow, SemanticEviction)
- Token budget enforcement (80% compress, 95% checkpoint flag)
- Partitions (Active + Scratchpad)
- Tier 2 (Episodic): SQLite per-agent with FTS5 search
- Tier 3 (Semantic): Vector embeddings with hybrid search (similarity + BM25)
- 12 context tests

**Missing:**
- T2/T3 not auto-populated — task completion doesn't write to episodic memory (Phase 5.1)
- No semantic memory query API — agents cannot recall from Tier 3
- No cross-tier promotion/demotion (T1 entries evicted without archiving)
- No memory export/import for portability

**Files:** `crates/agentos-kernel/src/context.rs`, `crates/agentos-types/src/context.rs`, `crates/agentos-memory/`

---

### Spec #12: Approval Gates (65%)

**Working:**
- 5-level risk classification (Autonomous -> Notify -> SoftApproval -> HardApproval -> Forbidden)
- EscalationManager with create/resolve/list/sweep_expired
- Auto-denial after 5-minute timeout
- Task pauses on blocking escalation, resumes on resolution
- CLI: escalation list/get/resolve
- Audit integration (RiskEscalation, ActionForbidden events)
- 14 risk classifier tests

**Missing (critical):**
- No automatic risk -> escalation trigger — risk classifier exists but not wired to auto-escalate
- No notification channels (Slack/Telegram/Web UI) — users must poll CLI
- No Web UI for escalation review
- Soft-approval path doesn't enforce "proceed only if approved"
- Fixed 5-minute timeout — no per-action or per-agent customization

**Files:** `crates/agentos-kernel/src/risk_classifier.rs`, `crates/agentos-kernel/src/escalation.rs`

---

## Critical Security Vulnerabilities

| # | Vulnerability | Impact | Exploitability | Spec |
|---|-------------|--------|----------------|------|
| 1 | Any agent can request ANY secret by name (no scope enforcement) | Secret exfiltration | HIGH | #3 |
| 2 | CLI can impersonate any agent (no bus authentication) | Full identity spoofing | HIGH | #7 |
| 3 | Messages accepted without signature verification | Message forgery | HIGH | #10 |
| 4 | No CRL — compromised tools cannot be globally revoked | Persistent compromise | HIGH | #1 |
| 5 | Permission matrix not wired to syscall filter | Permission bypass via raw syscalls | HIGH | #2 |
| 6 | No model allowlist — agents call expensive models freely | Budget exhaustion | HIGH | #2,4 |
| 7 | Injection scanner skips user prompts | Direct prompt injection | MEDIUM | #6 |
| 8 | Risk classifier not auto-triggering escalations | Unapproved high-risk actions | MEDIUM | #12 |
| 9 | HAL not integrated — hardware access uncontrolled | Unauthorized device access | MEDIUM | #9 |
| 10 | No inter-agent permission check | Unauthorized agent communication | MEDIUM | #10 |

---

## Step-by-Step Remediation Plan

### Phase 1: Critical Security Fixes (2-3 weeks)

1. **Secret scope enforcement** — Validate agent_id vs secret owner in `issue_proxy_token()` (vault.rs)
2. **Bus authentication** — Add Ed25519 challenge-response in BusConnection handshake
3. **Message signature verification** — Enforce in `send_direct()`, `broadcast()`, `send_to_group()` (agent_message_bus.rs)
4. **CRL checking** — Add revocation list loader + check in `tool_registry.rs::register()`
5. **Model allowlist** — Add `allowed_models: Vec<String>` to AgentBudget, validate before inference
6. **Scan user input** — Call injection scanner on task prompts in task_executor.rs

### Phase 2: Enforcement Wiring (2-3 weeks)

7. **Risk -> Escalation auto-trigger** — Wire risk_classifier output to escalation_manager in task_executor
8. **Inter-agent permission enforcement** — Check `can_message` whitelist in message bus
9. **HAL integration** — Check HardwareRegistry permissions in task_executor before tool execution
10. **Permission matrix -> Seccomp** — Generate seccomp rules from permission matrix
11. **Resource arbiter enforcement** — Call arbiter during tool execution for file/network resources
12. **Escalation notifications** — Add webhook/Slack/Telegram notification trait

### Phase 3: Completeness (2-3 weeks)

13. **Episodic memory auto-write** — Hook task completion to T2 recording (Phase 5.1)
14. **Semantic memory query API** — Expose `recall()` in context manager for agents
15. **Parent budget hierarchy** — Aggregate cost tracking across agent groups
16. **Wall-time limits** — Add `max_wall_time_seconds` to AgentBudget
17. **Device discovery** — Linux /sys/ scanning + hot-plug detection for HAL
18. **GPU Resource Manager** — Time-slicing + VRAM quotas
19. **Emergency lockdown** — `agentsecret lockdown` atomic revocation command
20. **Snapshot encryption** — Encrypt snapshot JSON files at rest

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-vault/src/vault.rs` | Add scope validation in `issue_proxy_token()`, add lockdown command |
| `crates/agentos-kernel/src/tool_registry.rs` | Add CRL checking in `register()` |
| `crates/agentos-kernel/src/agent_message_bus.rs` | Add signature verification + `can_message` check |
| `crates/agentos-kernel/src/task_executor.rs` | Wire risk classifier, scan user prompts, check HAL, use arbiter |
| `crates/agentos-types/src/task.rs` | Add `allowed_models`, `max_wall_time_seconds` to AgentBudget |
| `crates/agentos-bus/src/message.rs` | Add authentication handshake |
| `crates/agentos-kernel/src/identity.rs` | Add challenge-response authentication |
| `crates/agentos-kernel/src/risk_classifier.rs` | Wire to escalation manager |
| `crates/agentos-hal/src/registry.rs` | Add device discovery, GPU manager |
| `crates/agentos-kernel/src/cost_tracker.rs` | Add parent budget hierarchy |

---

## Verification

```bash
# Build and test
cargo build --workspace && cargo test --workspace

# Clippy clean
cargo clippy --workspace -- -D warnings

# Security-specific tests to add:
cargo test -p agentos-vault -- scope_enforcement
cargo test -p agentos-kernel -- message_signature_verification
cargo test -p agentos-kernel -- risk_auto_escalation
cargo test -p agentos-kernel -- model_allowlist
cargo test -p agentos-kernel -- inter_agent_permission
```

---

## Related

- [[agos-implementation-spec]]
- [[11-Spec Enforcement Hardening]]
- [[Feedback Implementation Plan]]
- [[Issues and Fixes]]
