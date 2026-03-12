---
title: Full Codebase Review
tags:
  - review
  - security
  - quality
  - next-steps
date: 2026-03-13
status: planned
effort: 5d
priority: critical
---

# Full Codebase Review

> Systematic review of the entire AgentOS codebase (~32K lines, 17 crates) for correctness, security, architecture, and quality — without exceeding context limits.

---

## Current State

The codebase has grown through V1-V3 without a structured code review. Security-critical subsystems (vault, capability tokens, injection scanning, sandbox) have not been adversarially scrutinized. Only 3 of 17 crates have test files.

## Goal / Target State

All 173 source files reviewed. Findings consolidated into a prioritized report. Critical/high issues identified for immediate remediation.

---

## Sub-tasks

| # | Task | Status |
|---|------|--------|
| 1 | [[01-foundation-types-review\|Phase 1: Foundation Types]] (5 steps, ~2,431 lines) | planned |
| 2 | [[02-infrastructure-review\|Phase 2: Infrastructure]] (10 steps, ~9,944 lines) | planned |
| 3 | [[03-bus-and-capability-review\|Phase 3: Bus & Capability]] (2 steps, ~1,470 lines) | planned |
| 4 | [[04-tools-and-wasm-review\|Phase 4: Tools & WASM]] (7 steps, ~3,252 lines) | planned |
| 5 | [[05-kernel-core-review\|Phase 5: Kernel]] (16 steps, ~13,483 lines) | planned |
| 6 | [[06-user-interfaces-review\|Phase 6: User Interfaces]] (8 steps, ~5,276 lines) | planned |
| 7 | [[07-cross-cutting-passes\|Phase 7: Cross-Cutting Passes]] (6 steps) | planned |
| 8 | [[08-security-deep-dives\|Phase 8: Security Deep Dives]] (4 steps) | planned |
| 9 | [[09-config-and-manifests-review\|Phase 9: Config & Manifests]] (1 step) | planned |
| 10 | [[10-synthesis-and-report\|Phase 10: Synthesis]] (1 step) | planned |

**Total: 60 steps across 10 phases.**

---

## Verification

```bash
# After any fixes applied from review findings:
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Related

- [[Full Codebase Review Plan]] — master plan with design decisions and dependency graph
- [[Full Codebase Review Data Flow]] — how findings flow through phases to final report
