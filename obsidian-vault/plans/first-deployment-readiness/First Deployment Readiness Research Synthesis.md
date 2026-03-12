---
title: First Deployment Readiness Research Synthesis
tags:
  - deployment
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 4h
priority: high
---

# First Deployment Readiness Research Synthesis

> Consolidated evidence for first deployment blockers and recommended closure sequence.

---

## Problem framing

The codebase is functionally advanced (~150 passing tests, 40+ command handlers, 5 LLM adapters), but first deployment depends on runtime safety, release reliability signals, and operational reproducibility — not implementation breadth alone.

## Evidence summary

| Evidence | Observation | Implication |
|---|---|---|
| Runtime safety | `panic!()` in `agent_message_bus.rs:458`; RwLock `.unwrap()` in 10 sites across capability/HAL | Kernel crash from normal operations |
| Lock poisoning | `agentos-capability` engine and profiles use `.write().unwrap()` | One thread panic poisons all subsequent permission checks |
| Quality checks | `cargo fmt --all -- --check` fails | Formatting gate blocks release |
| Static analysis | `cargo clippy --workspace -- -D warnings` fails (10 errors across 4 crates) | Strict quality policy not met |
| Integration tests | 6 CLI integration tests hang indefinitely | No end-to-end test confidence |
| Release compile | `cargo build --workspace --release` passes | Buildability is strong but insufficient |
| Unit tests | ~150 workspace unit tests pass | Functional baseline exists |
| Packaging | no committed `Dockerfile` / `docker-compose` | Stage 1 target not operationalized |
| Runtime defaults | `/tmp` paths in default config | Non-durable storage in production context |
| LLM endpoints | Hardcoded `localhost` in config and fallback code | Breaks containerized/remote deployment |
| CI automation | No workflow file or Makefile for gate chain | Gates will regress without enforcement |
| Release baseline | no git tags | No immutable first release marker |

## Deployment risks and closure strategy

### 0. Runtime crash paths (NEW — highest priority)
- **Risk:** `panic!()` in message signing and RwLock poisoning in capability engine can crash the kernel during normal agent operations. A single thread panic cascades into kernel-wide failure.
- **Closure:** replace all `panic!()` in non-test code with `Result` returns; add poison-recovery on all `RwLock` access in `agentos-capability` and `agentos-hal`.

### 1. Quality gate drift
- **Risk:** release candidates vary by local tooling and unchecked warnings.
- **Closure:** enforce deterministic gate chain, add CI workflow, freeze candidate only after full pass.

### 2. Runtime persistence mismatch
- **Risk:** secrets/audit/data on temporary paths can be lost after restart. Hardcoded `localhost` LLM endpoints break any non-local deployment.
- **Closure:** introduce production profile with stable paths, ownership checks, and environment-variable-driven LLM endpoints.

### 3. Packaging mismatch
- **Risk:** docs claim Docker-first but operators have no canonical artifacts. Seccomp sandbox and static linking may cause container build issues.
- **Closure:** commit reference container assets, test multi-stage Rust build, wire health/readiness checks.

### 4. Security confidence gap
- **Risk:** feature presence does not guarantee deploy-time behavior. Security acceptance tests are not concrete enough to execute.
- **Closure:** require executable security acceptance scenarios with specific test files, assertion patterns, and audit event verification.

### 5. Governance gap
- **Risk:** no reliable rollback anchor without release tag policy.
- **Closure:** define first tag criteria tied to preflight evidence.

## Recommended order

0. Code safety hardening (panic removal, lock-poisoning fixes).
1. Quality gates stabilization (fmt, clippy, CI workflow).
2. Production configuration baseline (persistent paths, LLM endpoint config).
3. Container artifact publication.
4. Security closure validation.
5. Release cut and tag protocol.

## Minimum viable release (v0.1.0)

If time is constrained, Phases 0-2 alone produce a safe, linted, tested binary with production config. This is sufficient for a tagged single-machine release. Phases 3-5 can follow in v0.1.1+.

## Success metrics

| Metric | Target |
|---|---|
| Runtime panics in non-test code | 0 |
| RwLock `.unwrap()` in production paths | 0 (all poison-recovered) |
| Formatting gate | 100 percent pass |
| Clippy strict gate | 100 percent pass |
| Test gate | 100 percent pass (ignored tests documented) |
| Release build gate | 100 percent pass |
| CI workflow | Committed and passing |
| Container smoke | healthy start, stable restart |
| Security scenarios | all mandatory checks pass with specific assertions |
| Release governance | first validated semver tag ready |

## Related

- [[First Deployment Readiness Plan]]
- [[First Deployment Readiness Data Flow]]
- [[16-First Deployment Readiness Program]]
