---
title: Release Process and Cutover
tags:
  - release
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 4h
priority: high
---

# Release Process and Cutover

> Define first-release cut criteria, tagging protocol, and rollback behavior.

## Why this phase

First deployment must produce an immutable baseline that can be audited and rolled back safely.

## Current -> Target state

- **Current:** no first-release tag policy and no formalized launch sign-off.
- **Target:** approved cut checklist with explicit semver tagging and rollback steps.

## Detailed subtasks

1. Define first release policy:
   - target tag `v0.1.0`
   - required evidence before tag creation.
2. Define launch sign-off template:
   - quality gate evidence
   - container smoke evidence
   - security closure evidence
3. Define rollback procedure:
   - previous tag redeploy
   - data compatibility checks
4. Update docs and references:
   - `README.md`
   - `agentic-os-deployment.md`
   - `obsidian-vault/reference/First Deployment Runbook.md`
5. Record release checklist completion path.

## Files changed

| File | Change |
|------|--------|
| `README.md` | Release policy and tag guidance |
| `agentic-os-deployment.md` | Cutover and rollback steps |
| `obsidian-vault/reference/First Deployment Runbook.md` | Launch sign-off template |

## Dependencies

- **Requires:** [[01-quality-gates-stabilization]], [[04-security-gate-closure]].
- **Blocks:** first public deployment.

## Test plan

- Dry-run release checklist on a candidate commit.
- Validate rollback procedure in a staging-like environment.

## Verification

```bash
git tag --list
git log --oneline -n 20
```
