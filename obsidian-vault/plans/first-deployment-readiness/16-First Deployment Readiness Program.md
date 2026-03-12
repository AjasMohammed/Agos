---
title: First Deployment Readiness Program
tags:
  - deployment
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 7d
priority: critical
---

# First Deployment Readiness Program

> Multi-stream execution plan to move AgentOS from buildable to first deployable release.

---

## Current State

- `panic!()` reachable in `agent_message_bus.rs:458` via normal agent messaging.
- RwLock `.write().unwrap()` in 10 sites across `agentos-capability` and `agentos-hal` — one poisoned lock crashes all permission checks.
- 6 CLI integration tests hang indefinitely (require running kernel).
- `cargo build --workspace --release` passes, but strict quality gates are not green.
- `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings` fail (10 clippy errors).
- `config/default.toml` is development-oriented (`/tmp` paths) and not durable for production.
- Hardcoded `localhost` LLM endpoints in config and fallback code break container deployment.
- No CI workflow file — gates run manually and will regress.
- Stage 1 deployment target is Docker-first, but concrete repository deployment artifacts are missing.
- Release governance is incomplete (no first tagged release baseline).

## Goal / Target State

First deployment candidate is ready with:
- no runtime crash paths (panics, lock poisoning),
- green quality gates with CI enforcement,
- production-safe config profile with configurable LLM endpoints,
- Docker deployment artifacts and operational checks,
- explicit security verification scenarios with concrete test implementations,
- release cut checklist and version tag policy.

## Minimum Viable v0.1.0

Phases 00-02 alone (code safety + quality gates + production config) produce a safe, linted, tested binary with production config. This is sufficient for a tagged single-machine release. Phases 03-05 can follow in v0.1.1+.

## Sub-tasks

| # | Task | File | Status |
|---|------|------|--------|
| 00 | [[16-00-Code Safety Hardening]] | `obsidian-vault/plans/first-deployment-readiness/subtasks/16-00-Code Safety Hardening.md` | planned |
| 01 | [[16-01-Restore Quality Gates]] | `obsidian-vault/plans/first-deployment-readiness/subtasks/16-01-Restore Quality Gates.md` | planned |
| 02 | [[16-02-Harden Production Config]] | `obsidian-vault/plans/first-deployment-readiness/subtasks/16-02-Harden Production Config.md` | planned |
| 03 | [[16-03-Add Container Deployment Artifacts]] | `obsidian-vault/plans/first-deployment-readiness/subtasks/16-03-Add Container Deployment Artifacts.md` | planned |
| 04 | [[16-04-Security Readiness Closure]] | `obsidian-vault/plans/first-deployment-readiness/subtasks/16-04-Security Readiness Closure.md` | planned |
| 05 | [[16-05-Release Versioning and Tagging]] | `obsidian-vault/plans/first-deployment-readiness/subtasks/16-05-Release Versioning and Tagging.md` | planned |
| 06 | [[16-06-Preflight and Launch Checklist]] | `obsidian-vault/plans/first-deployment-readiness/subtasks/16-06-Preflight and Launch Checklist.md` | planned |

## Step-by-Step Plan

0. Fix runtime crash paths: remove `panic!()`, add RwLock poison recovery, mark hanging tests.
1. Restore all required quality gates, add CI workflow, freeze a known-good baseline.
2. Create a production configuration baseline with persistent paths, configurable LLM endpoints, and operational assumptions.
3. Add canonical Docker deployment artifacts aligned with Stage 1 strategy.
4. Close deployment-time security validation with executable, concrete checks.
5. Define release governance, cut criteria, and first tag protocol.
6. Run preflight checklist and launch readiness review.

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/plans/first-deployment-readiness/16-First Deployment Readiness Program.md` | Parent index for first deployment program |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-00-Code Safety Hardening.md` | Subtask detail (NEW) |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-01-Restore Quality Gates.md` | Subtask detail |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-02-Harden Production Config.md` | Subtask detail |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-03-Add Container Deployment Artifacts.md` | Subtask detail |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-04-Security Readiness Closure.md` | Subtask detail |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-05-Release Versioning and Tagging.md` | Subtask detail |
| `obsidian-vault/plans/first-deployment-readiness/subtasks/16-06-Preflight and Launch Checklist.md` | Subtask detail |

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

## Related

- [[First Deployment Readiness Plan]]
- [[First Deployment Readiness Data Flow]]
- [[First Deployment Readiness Research Synthesis]]
- [[12-Production Readiness Audit]]
