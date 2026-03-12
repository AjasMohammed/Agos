---
title: Preflight and Launch Checklist
tags:
  - launch
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 4h
priority: critical
---

# Preflight and Launch Checklist

> Final go/no-go checklist for first deployment launch.

## Why this sub-task

A single operator checklist prevents skipped validation and ensures deployment readiness is measured consistently.

## Current -> Target State

- **Current:** checks exist across docs but not consolidated into one launch gate.
- **Target:** one canonical checklist with explicit go/no-go criteria and evidence capture.

## What to Do

1. Assemble preflight checklist sections:
   - build quality gates
   - deployment artifact validation
   - security smoke tests
   - runtime config validation
2. Add first-boot smoke checklist:
   - kernel health
   - agent registration
   - task execution
   - audit visibility
3. Add rollback trigger conditions and emergency steps.
4. Define sign-off format for launch owner.

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/First Deployment Runbook.md` | Canonical preflight and launch runbook |
| `agentic-os-deployment.md` | Align checklist and launch criteria |
| `README.md` | Link to launch checklist |

## Expected Inputs and Outputs

- **Input:** candidate release commit and deployment environment.
- **Output:** signed go/no-go decision with reproducible evidence.

## Prerequisites

- [[16-01-Restore Quality Gates]]
- [[16-02-Harden Production Config]]
- [[16-03-Add Container Deployment Artifacts]]
- [[16-04-Security Readiness Closure]]
- [[16-05-Release Versioning and Tagging]]

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --workspace --release
docker compose up -d
agentctl status
```

Pass criteria:
- All required checks pass and evidence is recorded in launch runbook.
