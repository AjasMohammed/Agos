---
title: "TODO: Execute Full Codebase Review (Phases 1-2, 4-10)"
tags:
  - review
  - security
  - quality
  - next-steps
date: 2026-03-17
status: complete
effort: 5d
priority: critical
---

# Execute Full Codebase Review

> Systematically execute all 9 remaining review phases (Phase 1, 2, 4-10) across the AgentOS codebase to surface bugs, security issues, and architecture gaps before production deployment.

## Why This Phase

Only Phase 3 (Bus & Capability review) has been executed. The remaining 9 phases cover the full codebase (~32K lines, 17 crates) including the security-critical kernel (13K lines), vault, sandbox, injection scanner, and capability engine. Without this review, latent security bugs, lock-poisoning risks, and correctness issues may ship to production.

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Foundation types reviewed | Not started | All types/IDs/errors reviewed for correctness |
| Infrastructure reviewed | Not started | Vault, audit, LLM adapters, HAL, memory, pipeline reviewed |
| Tools & WASM reviewed | Not started | Built-in tools, runner, WASM executor reviewed |
| Kernel reviewed | Not started | All 49 kernel files reviewed for security/correctness |
| User interfaces reviewed | Not started | CLI, web UI reviewed |
| Cross-cutting passes | Not started | Unwrap audit, SQL injection scan, error handling patterns |
| Security deep dives | Not started | Adversarial review of vault, capability, injection scanner |
| Config & manifests reviewed | Not started | TOML config, tool manifests reviewed |
| Synthesis report | Not started | Consolidated findings by severity |

## Detailed Subtasks

For each phase, spawn an agent with the phase's file list and the review checklist. Each step produces a findings table:

```
| File | Line(s) | Severity | Category | Description | Suggested Fix |
```

### Phase 1 — Foundation Types Review
Execute: `plans/full-codebase-review/01-foundation-types-review.md`
Key files: `crates/agentos-types/src/`, `crates/agentos-sdk-macros/src/`

### Phase 2 — Infrastructure Review
Execute: `plans/full-codebase-review/02-infrastructure-review.md`
Key files: `crates/agentos-audit/`, `crates/agentos-vault/`, `crates/agentos-llm/`, `crates/agentos-hal/`, `crates/agentos-memory/`, `crates/agentos-pipeline/`, `crates/agentos-sandbox/`

### Phase 4 — Tools & WASM Review
Execute: `plans/full-codebase-review/04-tools-and-wasm-review.md`
Key files: `crates/agentos-tools/`, `crates/agentos-wasm/`, `crates/agentos-sdk/`

### Phase 5 — Kernel Review (LARGEST — 16 steps)
Execute: `plans/full-codebase-review/05-kernel-core-review.md`
Key files: All 49 files in `crates/agentos-kernel/src/`

### Phase 6 — User Interfaces Review
Execute: `plans/full-codebase-review/06-user-interfaces-review.md`
Key files: `crates/agentos-cli/`, `crates/agentos-web/`

### Phase 7 — Cross-Cutting Passes
Execute: `plans/full-codebase-review/07-cross-cutting-passes.md`
Sweep: `unwrap()` usage, SQL string interpolation, error swallowing, token security

### Phase 8 — Security Deep Dives
Execute: `plans/full-codebase-review/08-security-deep-dives.md`
Focus: vault crypto, capability token signing, injection scanner, path traversal

### Phase 9 — Config & Manifests
Execute: `plans/full-codebase-review/09-config-and-manifests-review.md`
Key files: `config/*.toml`, `tools/core/*.toml`

### Phase 10 — Synthesis
Execute: `plans/full-codebase-review/10-synthesis-and-report.md`
Consolidate all findings tables from phases 1-9, prioritize by severity, write to `obsidian-vault/reference/Codebase Review Report.md`

## Files Changed

No source files changed by the review itself. Each phase produces findings that may trigger follow-on bug fixes.

## Dependencies

Phase 1 → Phase 2 → Phase 4 → Phase 5 → Phase 6 → Phases 7 & 8 in parallel → Phase 9 (parallel) → Phase 10.
Phases within each phase can run in parallel (each step is independent).

## Test Plan

- After each phase: `cargo build --workspace && cargo test --workspace` to verify no regressions from any concurrent fixes

## Verification

```bash
# After Phase 10 synthesis:
ls obsidian-vault/reference/Codebase\ Review\ Report.md
# Findings consolidated and severity-ranked
```

## Related

- [[Full Codebase Review Plan]] — master plan with all 60 steps
- [[audit_report]] — GAP-C05
