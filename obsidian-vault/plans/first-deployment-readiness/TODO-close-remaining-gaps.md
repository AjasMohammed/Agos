---
title: Close First Deployment Readiness Remaining Gaps
tags: [deployment, ci, release, next-steps]
date: 2026-03-17
status: planned
effort: 4h
priority: high
---

# Close First Deployment Readiness Remaining Gaps

> Fix the cargo fmt ordering diff, verify security gate closure, and cut the v0.1.0 release tag so the First Deployment Readiness plan reaches 100%.

## Why This Phase

A plan audit (2026-03-17) confirmed that Phases 00-03 of the First Deployment Readiness plan are complete but their subtask status fields still say `planned`. Phase 04 (security gate closure verification) has not been signed off. Phase 05 (release cutover) is blocked by one outstanding `cargo fmt` issue and the Phase 04 sign-off. No `v0.1.0` git tag exists.

## Current State

| Aspect | Current | Gap |
|--------|---------|-----|
| `cargo fmt` | FAIL — ordering diff in `commands/mod.rs` | `healthz` declaration mispositioned |
| CI workflow | Both `.github/workflows/ci.yml` and `release-gate.yml` exist | `TODO-ci-automation.md` incorrectly says `planned` |
| Subtask files 16-00 to 16-03 | All say `status: planned` | Code is done; docs are stale |
| Phase 04 security gate closure | `status: planned` | Checklist not signed off |
| Phase 05 release cutover | `status: planned` | No v0.1.0 tag exists |
| Launch checklist | Does not exist | Needed before tagging |

## Target State

- `cargo fmt --all -- --check` passes clean
- All subtask files 16-00 through 16-03 updated to `complete`
- `TODO-ci-automation.md` updated to `complete`
- Security gate checklist from `16-04-Security Readiness Closure.md` verified and signed off
- `LAUNCH-CHECKLIST.md` created in `obsidian-vault/plans/first-deployment-readiness/`
- `v0.1.0` git tag created on the validated commit
- Master plan status updated to `complete`

## Detailed Subtasks

### 1. Fix cargo fmt ordering diff

Open `crates/agentos-cli/src/commands/mod.rs`.

The `pub mod healthz;` declaration is currently on line 4, before `pub mod audit;`. It needs to be placed after `pub mod hal;` to maintain alphabetical order.

Current order (lines 4-23):
```
pub mod agent;
pub mod healthz;   ← WRONG POSITION
pub mod audit;
pub mod bg;
pub mod cost;
pub mod escalation;
pub mod event;
pub mod hal;
pub mod identity;
...
```

Target order:
```
pub mod agent;
pub mod audit;
pub mod bg;
pub mod cost;
pub mod escalation;
pub mod event;
pub mod hal;
pub mod healthz;   ← CORRECT POSITION (after hal, before identity)
pub mod identity;
...
```

Verify: `cargo fmt --all -- --check` must return exit code 0.

### 2. Update stale subtask status fields

For each file below, change only the `status:` YAML frontmatter field from `planned` to `complete`:

| File | Change |
|------|--------|
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-00-Code Safety Hardening.md` | `planned` → `complete` |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-01-Restore Quality Gates.md` | `planned` → `complete` |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-02-Harden Production Config.md` | `planned` → `complete` |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-03-Add Container Deployment Artifacts.md` | `planned` → `complete` |
| `obsidian-vault/plans/first-deployment-readiness/TODO-ci-automation.md` | `planned` → `complete` |

### 3. Execute Phase 04 security gate closure checklist

Open `obsidian-vault/plans/first-deployment-readiness/subtasks/16-04-Security Readiness Closure.md`.

Run each verification command from the checklist in that file. For each item, confirm the expected behavior is present in source code. Items expected to be present (verified by audit):
- Vault AES-256-GCM encryption: `crates/agentos-vault/src/`
- Capability token HMAC: `crates/agentos-capability/src/`
- Injection scanner (23 patterns): `crates/agentos-kernel/src/injection_scanner.rs`
- Seccomp sandbox (Linux): `crates/agentos-sandbox/src/`
- HMAC event signing on all emission paths: `crates/agentos-kernel/src/event_dispatch.rs`
- No plaintext secrets in logs: search for `tracing::info!.*secret` — must be absent

Once verified, update `16-04-Security Readiness Closure.md` status to `complete`.

### 4. Create LAUNCH-CHECKLIST.md

Create `obsidian-vault/plans/first-deployment-readiness/LAUNCH-CHECKLIST.md` with the following structure:

```markdown
---
title: v0.1.0 Launch Checklist
date: 2026-03-17
status: complete
---

# v0.1.0 Launch Checklist

- [ ] cargo build --workspace — PASS
- [ ] cargo test --workspace — PASS (0 failures)
- [ ] cargo clippy --workspace -- -D warnings — PASS
- [ ] cargo fmt --all -- --check — PASS
- [ ] Dockerfile builds successfully
- [ ] docker-compose up boots without errors
- [ ] Security gate closure verified (Phase 04)
- [ ] No plaintext secrets in logs
- [ ] v0.1.0 tag created
```

Sign off each item with the date and result.

### 5. Cut v0.1.0 tag

After all above steps pass:

```bash
git tag -a v0.1.0 -m "AgentOS v0.1.0 — first production baseline

- 17 crates, 487 passing tests
- Full event system (43 of 47 EventType variants emitted)
- WebUI with auth, CSRF, CORS, rate limiting
- Memory architecture: episodic, semantic, procedural, context compiler
- Pipeline executor with security enforcement
- Hardware Abstraction Layer
- First deployment readiness verified"
```

Then update `TODO-release-cutover.md` status to `complete` and the master `First Deployment Readiness Plan.md` status to `complete`.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-cli/src/commands/mod.rs` | Reorder `healthz` declaration alphabetically |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-00-Code Safety Hardening.md` | Update status |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-01-Restore Quality Gates.md` | Update status |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-02-Harden Production Config.md` | Update status |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-03-Add Container Deployment Artifacts.md` | Update status |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-04-Security Readiness Closure.md` | Update status |
| `obsidian-vault/plans/first-deployment-readiness/TODO-ci-automation.md` | Update status |
| `obsidian-vault/plans/first-deployment-readiness/TODO-release-cutover.md` | Update status |
| `obsidian-vault/plans/first-deployment-readiness/First Deployment Readiness Plan.md` | Update status to `complete` |
| `obsidian-vault/plans/first-deployment-readiness/LAUNCH-CHECKLIST.md` | Create new file |

## Dependencies

Subtask 1 (fmt fix) must be done before subtask 5 (tag). All other steps are independent.

## Test Plan

- `cargo fmt --all -- --check` — exit code 0
- `cargo clippy --workspace -- -D warnings` — exit code 0, no warnings
- `cargo test --workspace` — all tests pass, 0 failures
- `git tag -l v0.1.0` — returns `v0.1.0`

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
git tag -l "v*"
```
