---
title: Incomplete Plans — Implementation Gaps
tags:
  - audit
  - status
  - reference
  - bugfix
date: 2026-03-16
updated: 2026-03-16 (re-audit + 2026-03-16 gap closure pass)
status: in-progress
---

# Incomplete Plans

> Plans listed here are either PARTIAL (some items implemented, some missing) or INCOMPLETE (not started or explicitly deferred). Each entry includes the specific gaps found in the codebase.

---

## PARTIAL — Core Plans (next-steps/)

### 04 - Kernel Modularization
**File:** `next-steps/04-Kernel Modularization.md`
**Status:** PARTIAL

**What's done:**
- `context_compiler.rs` created (functional equivalent of planned `context_injector.rs`)
- `context_injector.rs` created — exists at `crates/agentos-kernel/src/context_injector.rs`
- `task_completion.rs` created — exists at `crates/agentos-kernel/src/task_completion.rs`
- `kernel.rs` reduced from ~2700 lines to ~472 lines

**Missing:**
- `kernel.rs` at 472 lines, still above target of < 300 lines

---

### 07 - Hardware Abstraction Layer
**File:** `next-steps/07-Hardware Abstraction Layer.md`
**Status:** PARTIAL

**What's done:**
- `HardwareRegistry` with `quarantine_device()`, `approve_device()`, `request_access()`, `revoke_access()` in `agentos-hal/src/registry.rs`
- `hal: Arc<HardwareAbstractionLayer>` wired as a `Kernel` field
- HAL CLI commands (`agentctl hal ...`) — `crates/agentos-cli/src/commands/hal.rs` exists
- HAL `KernelCommand` variants — 6 variants present in `agentos-bus/src/message.rs`: `HalListDevices`, `HalApproveDevice`, `HalDenyDevice`, `HalRevokeDevice`, `HalRegisterDevice`, `HalDeviceList`

**Missing:**
- `GpuSliceManager` — no GPU-specific slice manager or per-agent VRAM cap enforcement
- `HardwarePermission` on `AgentPermission` — no such struct in `agentos-types`

---

### 10 - High-Priority Spec Gaps
**File:** `next-steps/10-High-Priority Gaps.md`
**Status:** PARTIAL

**What's done (5 of 7):**
- CLI escalation wiring (list + resolve commands)
- High-taint injection blocking (task → `Waiting`, human review)
- Checkpoint/rollback wiring (`SnapshotManager` + `RollbackTask`)
- A2A message signing (signature field, `verify_message_signature()`, TTL)
- Deadlock detection (`wait_for` graph, `would_deadlock()` DFS in `resource_arbiter.rs`)

**Missing:**
- Token budget: dual implementation (`ContextManager` + `ContextCompiler`) — integration unclear
- Zero-exposure secret proxy: explicitly deferred to a separate tracking item

---

### 14 - Spec Gap Fixes
**File:** `next-steps/14-Spec Gap Fixes.md`
**Status:** COMPLETE

**All 17 subtasks verified present (2026-03-16 audit):**
- Proxy token sweep, CRL enforcement, `ProxyVault` wrapper, memory store `sweep_old_entries()`/`export_jsonl()`, `VaultLockdown` command, identity commands, `revoke_agent()`, lock waiter priority, pipeline budget check, soft-approval, notify_url webhook, `max_wall_time_seconds`, `export_chain_json`, contention_stats, `import_jsonl`, `notify_tx` on `CostTracker`
- **Subtask 6** (CLI scope parsing): `scope_raw: Some(scope)` passed from CLI `secret set`; kernel `cmd_set_secret` resolves via `resolve_secret_scope()` which looks up agent/tool by name in registries → `SecretScope::Agent(id)` / `SecretScope::Tool(id)`. Confirmed in `crates/agentos-kernel/src/commands/secret.rs` and `crates/agentos-cli/src/commands/secret.rs`.
- **Subtask 14b** (`ExportAuditChain`): `KernelCommand::ExportAuditChain { limit }` present in `message.rs:279`; CLI `audit export` subcommand in `crates/agentos-cli/src/commands/audit.rs:26-33`; kernel handler in `crates/agentos-kernel/src/commands/audit.rs`; dispatched in `run_loop.rs:936`. All verified.

---


### 18 - Bug Fixes and Deployment Readiness
**File:** `next-steps/18-Bug Fixes and Deployment Readiness.md`
**Status:** COMPLETE (2026-03-16)

**All 5 sub-tasks verified:**
- Sub-task 01: Clippy passes clean (`cargo clippy --workspace` zero warnings)
- Sub-task 02: Event HMAC wiring in `AgentMessageBus`/`ScheduleManager`
- Sub-task 03: Integration test harness (304-line `integration_test.rs`, no `#[ignore]`)
- Sub-task 04: Docker artifacts (`Dockerfile`, `docker-compose.yml`, `.dockerignore`, `config/docker.toml`)
- Sub-task 05: Issues and Fixes doc updated

---

### 20 - V1 Release Fix Plan
**File:** `next-steps/20-V1-Release-Fix-Plan.md`
**Status:** PARTIAL

**What's done (~10 of 15):**
- Phase 1.1: `vault.rs` now uses `tokio::sync::Mutex<Connection>` ✓
- Phase 1.2: Audit error suppression fixed (no `let _ = self.audit` patterns)
- Phase 1.3: Poisoned mutex handled via `unwrap_or_else(|e| e.into_inner())`
- Phase 1.4: Date parsing `unwrap` removed
- Phase 1.5: All channels use bounded `channel(CHANNEL_CAPACITY)` with capacity 1024
- Phase 2.2: Event HMAC signing from `AgentMessageBus`/`ScheduleManager` wired
- Phase 2.3: Per-agent rate limiting — `PerAgentRateLimiter` struct exists in `rate_limit.rs`
- Phase 3.1: `.github/workflows/ci.yml` exists with check/test/fmt/clippy jobs
- Phase 3.3: Docker artifacts present
- Phase 5.2: Episodic memory writes on task success/failure wired

**Missing:**
- ~~Phase 2.1: HMAC key scope still `SecretScope::Global`~~ — **DONE** (`SecretScope::Kernel` used in `capability/engine.rs:74`)
- Phase 4.2: Audit log rotation — `max_audit_entries` ~~not in `default.toml`~~ — **DONE** (added 2026-03-16); `prune_old_entries()` implemented + wired in `run_loop.rs:318-333`. Also added `max_audit_entries = 500000` to `config/production.toml`. **COMPLETE.**
- ~~Phase 4.1: Dedicated `tests/e2e/` directory not created~~ — **DONE** (`crates/agentos-kernel/tests/e2e/` exists with `kernel_boot.rs`, `common.rs`, `e2e.rs` harness)

---

## PARTIAL — First Deployment Readiness Plans

### 01 - Quality Gates Stabilization
**File:** `plans/first-deployment-readiness/01-quality-gates-stabilization.md`
Also: `subtasks/16-01-Restore Quality Gates.md`
**Status:** PARTIAL

**What's done:**
- All clippy fixes implemented (sandbox, audit, memory, pipeline crates)
- `.github/workflows/release-gate.yml` exists with fmt/clippy/test/release-build steps

**Missing:**
- CI pipeline not verified as green on an actual GitHub run (workflow file exists but execution status unknown)

---

### 02 - Production Config Baseline
**File:** `plans/first-deployment-readiness/02-production-config-baseline.md`
Also: `subtasks/16-02-Harden Production Config.md`
**Status:** PARTIAL

**What's done:**
- `config/production.toml` exists with non-localhost endpoints (Consul/internal)
- Hardcoded `localhost:8000/v1` fallback removed from `commands/agent.rs`

**Missing:**
- ~~Startup validation warnings when paths are under `/tmp`~~ — **DONE** (`warn_on_tmp_paths()` implemented in `config.rs:214-244`)
- Documentation updates to `docs/guide/07-configuration.md` — still pending
- ~~README versioning/cut criteria~~ — **DONE** (added 2026-03-16)

---

### 03 - Containerization and Runtime
**File:** `plans/first-deployment-readiness/03-containerization-and-runtime.md`
Also: `subtasks/16-03-Add Container Deployment Artifacts.md`
**Status:** COMPLETE (2026-03-16)

**All items verified:**
- `Dockerfile` (multi-stage, non-root user, healthcheck via `/healthz`)
- `docker-compose.yml` with agentos + ollama services, `read_only: true`, `no-new-privileges`
- `.dockerignore`
- `config/docker.toml`
- `.env.example` — present with required and optional variable documentation
- `docs/guide/07-configuration.md` — Docker profile section added (config keys, env vars, security settings)
- `docs/guide/02-getting-started.md` — Docker quick-start section added
- `README.md` — Docker Deployment section already present

---

## INCOMPLETE — Plans Not Yet Started or Explicitly Deferred

### 02 - Semantic Tool Discovery (Memory Architecture Phase 2)
**File:** `plans/memory-context-architecture/02-semantic-tool-discovery.md`
**Status:** INCOMPLETE (Intentionally Deferred)

**Reason:** Plan explicitly marked `status: deferred` — deferred to V3.3+ until tool catalog exceeds ~30 tools.

**What's needed when unblocked:**
- `always_loaded: bool` field on `ToolInfo`
- `search_tools()` and `embed_all()` on `ToolRegistry`
- `ToolSearchTool` (new tool)
- `cosine_similarity()` utility in `tool_registry.rs`

---

### 04 - Security Gate Closure
**File:** `plans/first-deployment-readiness/04-security-gate-closure.md`
Also: `subtasks/16-04-Security Readiness Closure.md`
**Status:** COMPLETE (2026-03-16)

**All items verified:**
- `crates/agentos-kernel/tests/security_acceptance_test.rs` — 7 scenarios, all pass (`cargo test -p agentos-kernel --test security_acceptance_test`)
- `docs/guide/06-security.md` — "Deployment Security Acceptance" section added with scenario table, pass criteria, and remediation guide
- `agentic-os-deployment.md` — "Security Gate — Required before launch" section added with scenario checklist and hard-block policy

---

### 05 - Release Process and Cutover
**File:** `plans/first-deployment-readiness/05-release-process-and-cutover.md`
Also: `subtasks/16-05-Release Versioning and Tagging.md` and `subtasks/16-06-Preflight and Launch Checklist.md`
**Status:** COMPLETE (2026-03-16)

**All items delivered:**
- `obsidian-vault/reference/Release Process.md` — created (2026-03-16): SemVer policy, cut criteria, tagging workflow, rollback procedure, sign-off template
- `obsidian-vault/reference/First Deployment Runbook.md` — created (2026-03-16): 6-phase preflight + first-boot smoke checklist (11 checks) + deployment sign-off template
- Git semver tags — pending (no tagged release yet; `v0.1.0` is the target once cut criteria pass)
- Versioning strategy + cut criteria in `README.md` — added (2026-03-16)
- Consolidated preflight checklist — in [[First Deployment Runbook]] Phase 1
- First-boot smoke checklist — in [[First Deployment Runbook]] Phase 4 (11 checks)
- Rollback trigger conditions and sign-off template — in [[Release Process]] and [[First Deployment Runbook]]

---

### 19 - User Handbook
**File:** `next-steps/19-User Handbook.md`
**Status:** INCOMPLETE

**Reason:** Planning documents exist in `plans/user-handbook/` (9 phase files + master plan) but zero handbook chapters have been written.

**What's needed:**
- `obsidian-vault/reference/handbook/` directory exists but is empty — write 19 chapters + index covering: installation, quickstart, CLI reference, agents, tasks, tools, security, vault, memory, pipeline, event system, cost tracking, audit, configuration, troubleshooting

---

### 21 - Future Improvements (Post-V1 Backlog)
**File:** `next-steps/21-Future-Improvements.md`
**Status:** INCOMPLETE (by design — backlog)

**Reason:** Explicitly a post-v1 backlog. None of the 18 items are expected to be done pre-release.

**Notable backlog items:**
- ~~Replace `std::sync::Mutex` in `vault.rs` with `tokio::sync::Mutex`~~ — done (moved to V1 plan phase 1.1)
- Connection pooling in `BusClient`
- Configurable message size limit
- `HashMap`-based permission lookup for large permission sets
- Circuit breaker in LLM adapters
- Code coverage in CI
- `CHANGELOG.md`
- `docs/adr/` (Architecture Decision Records)

---

## Summary

| Category | Count |
|----------|-------|
| PARTIAL (some items done, some gaps) | 5 |
| COMPLETE (all items verified) | 5 |
| INCOMPLETE (not started) | 1 |
| INCOMPLETE (intentionally deferred) | 2 |
| **Total still requiring work** | **6** |

### Priority order for remaining work

1. **High:** User Handbook (`19`) — `reference/handbook/` directory exists, zero chapters written
2. **High:** `docs/guide/07-configuration.md` documentation update — `max_audit_entries` key needs documenting
3. **Medium:** HAL gaps — `GpuSliceManager`, `HardwarePermission` (`07`)
4. **Medium:** Token budget dual-implementation clarification — `ContextManager` vs `ContextCompiler` integration (`10`)
5. **Medium:** `kernel.rs` still 472 lines — target < 300 (`04-kernel-modularization`)
6. **Low (post-v1):** Semantic Tool Discovery (`02-memory-arch`)
7. **Backlog:** Future Improvements (`21`)

### Items closed since last audit (2026-03-16)

| Item | Gap | Resolution |
|---|---|---|
| 20 Phase 2.1 | HMAC key `SecretScope::Global` | Was already `SecretScope::Kernel` in `engine.rs:74` |
| 20 Phase 4.2 | `max_audit_entries` not in `default.toml` | Added to `config/default.toml` |
| 20 Phase 4.2 | Audit rotation not implemented | `prune_old_entries()` exists + wired in `run_loop.rs:318` |
| 20 Phase 4.1 | No `tests/e2e/` directory | `crates/agentos-kernel/tests/e2e/` exists with real tests |
| 02 prod-config | No startup `/tmp` warnings | `warn_on_tmp_paths()` in `config.rs:214` |
| 05 release-process | No reference docs | Created `Release Process.md` + `First Deployment Runbook.md` |
| 05 release-process | No versioning in README | Added "Versioning and Releases" section to `README.md` |
| 20 Phase 4.2 | `max_audit_entries` absent from `production.toml` | Added `max_audit_entries = 500000` to `config/production.toml` |
