---
title: Full Codebase Review Data Flow
tags:
  - review
  - flow
date: 2026-03-13
status: planned
effort: 30m
priority: critical
---

# Full Codebase Review Data Flow

> How source files flow through review steps and how findings are consolidated.

---

## Diagram

```mermaid
flowchart TD
    subgraph Input
        SRC[173 .rs source files]
        TOML[7 tool manifests + 1 config]
        TESTS[8 test files]
    end

    subgraph "Phase 1-6: Per-Crate Review"
        S1[Step reads 500-1500 lines]
        S2[Agent applies checklist]
        S3[Produces findings table]
    end

    subgraph "Phase 7: Cross-Cutting"
        CC1[unwrap audit across all crates]
        CC2[Concurrency & async safety]
        CC3[SQL injection audit - 5 files]
        CC4[Secret hygiene - 7 files]
        CC5[Test coverage gap analysis]
        CC6[API surface consistency]
    end

    subgraph "Phase 8: Security Deep Dives"
        SD1[Capability token lifecycle]
        SD2[Tool execution boundary]
        SD3[Injection & prompt safety]
        SD4[Secrets at rest & transit]
    end

    subgraph "Phase 10: Synthesis"
        MERGE[Merge all findings]
        DEDUP[Deduplicate]
        RANK[Rank by severity]
        REPORT[Final report]
    end

    SRC --> S1
    TOML --> S1
    TESTS --> S1
    S1 --> S2 --> S3

    S3 --> CC1 & CC2 & CC3 & CC4 & CC5 & CC6
    S3 --> SD1 & SD2 & SD3 & SD4

    CC1 & CC2 & CC3 & CC4 & CC5 & CC6 --> MERGE
    SD1 & SD2 & SD3 & SD4 --> MERGE
    S3 --> MERGE

    MERGE --> DEDUP --> RANK --> REPORT
```

---

## Steps

1. **Input Selection** — Each step receives a curated list of 1-6 source files (500-1,500 lines total)
2. **Agent Review** — Agent reads files, applies the step-specific checklist, and records findings
3. **Findings Output** — Each step outputs a markdown table with: File, Line(s), Severity, Category, Description, Suggested Fix
4. **Cross-Cutting Passes** — Sweep across all crates for specific concerns (unwrap, SQL, concurrency, secrets)
5. **Security Deep Dives** — Re-read critical files with adversarial questions
6. **Synthesis** — Merge all findings, deduplicate, rank by severity, produce final report

---

## Parallelization Strategy

```
Phase 1 (5 steps)     ─── all in parallel ───>  findings
Phase 2 (10 steps)    ─── all in parallel ───>  findings
Phase 3 (2 steps)     ─── all in parallel ───>  findings
Phase 4 (7 steps)     ─── all in parallel ───>  findings
Phase 5 (16 steps)    ─── all in parallel ───>  findings
Phase 6 (8 steps)     ─── all in parallel ───>  findings
Phase 7 (6 steps)     ─── all in parallel ───>  findings
Phase 8 (4 steps)     ─── all in parallel ───>  findings
Phase 9 (1 step)      ─── single agent ──────>  findings
Phase 10 (1 step)     ─── single agent ──────>  FINAL REPORT
```

Maximum parallelism within each phase. Phases execute sequentially (1 → 2 → ... → 10), except phases 7-8 can start after phase 6.

---

## Related

- [[Full Codebase Review Plan]]
- [[10-synthesis-and-report]]
