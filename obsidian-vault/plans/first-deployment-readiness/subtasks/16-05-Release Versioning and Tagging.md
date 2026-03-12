---
title: Release Versioning and Tagging
tags:
  - release
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 4h
priority: high
---

# Release Versioning and Tagging

> Define immutable first-release governance and cut criteria.

## Why this sub-task

Without a versioned baseline and cut protocol, deployment state is not reproducible and rollback targets are ambiguous.

## Current -> Target State

- **Current:** no release tags and no formal release cut policy.
- **Target:** first semver tag policy, cut checklist, and rollback reference strategy.

## What to Do

1. Define versioning strategy:
   - first release target `v0.1.0`
   - patch/minor increment criteria
2. Define hard release criteria:
   - fmt/clippy/tests/release build all green
   - security smoke scenarios pass
   - deployment smoke pass on Docker profile
3. Define tagging process and release notes schema.
4. Define rollback process to previous known-good tag.

## Files Changed

| File | Change |
|------|--------|
| `README.md` | Release policy section |
| `agentic-os-deployment.md` | Cutover and rollback procedures |
| `obsidian-vault/reference/Release Process.md` | Operator release runbook |

## Expected Inputs and Outputs

- **Input:** deployment-ready candidate commit.
- **Output:** approved and tagged first deployment baseline.

## Prerequisites

- [[16-01-Restore Quality Gates]]
- [[16-04-Security Readiness Closure]]

## Verification

```bash
git tag --list
git log --oneline -n 20
```

Pass criteria:
- Tagging policy is documented and accepted.
- First release tag points to a fully validated commit.
