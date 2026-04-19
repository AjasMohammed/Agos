---
title: "TODO: Update Stale Unwired Features Plan Statuses"
tags:
  - documentation
  - next-steps
  - event-system
date: 2026-03-17
status: planned
effort: 10m
priority: medium
---

# Update Stale Unwired Features Plan Statuses

> Update frontmatter `status:` in the Unwired Features master plan and Phase 01 and Phase 03 files — both are fully implemented but docs still say `planned`.

## Why This Phase

A plan audit (2026-03-17) confirmed:
- Phase 01 (Emit Missing Events): All 27 non-external event types now have emission points in the codebase.
- Phase 03 (Web UI Integration): `agentctl web serve` command is implemented in `crates/agentos-cli/src/commands/web.rs`; CLI dispatches to `WebServer::new()`.
- The master `Unwired Features Plan.md` still says `status: planned`.

## Current → Target State

| File | Current | Target |
|------|---------|--------|
| `Unwired Features Plan.md` | `planned` | `complete` |
| `01-emit-missing-event-types.md` | `planned` | `complete` |
| `03-web-ui-integration.md` | `planned` | `complete` |

## Detailed Subtasks

1. Open `obsidian-vault/plans/unwired-features/Unwired Features Plan.md` — change `status: planned` to `status: complete`.

2. Open `obsidian-vault/plans/unwired-features/01-emit-missing-event-types.md` — change `status: planned` to `status: complete`.

3. Open `obsidian-vault/plans/unwired-features/03-web-ui-integration.md` — change `status: planned` to `status: complete`.

## Files Changed

| File | Change |
|------|--------|
| `Unwired Features Plan.md` | `status: planned` → `status: complete` |
| `01-emit-missing-event-types.md` | `status: planned` → `status: complete` |
| `03-web-ui-integration.md` | `status: planned` → `status: complete` |

## Dependencies

None — documentation-only change.

## Verification

```bash
grep "^status:" obsidian-vault/plans/unwired-features/*.md
# Expected: complete for Plan.md, 01, 02, 03, 04 files
```

## Related

- [[Unwired Features Plan]] — master plan
- [[audit_report]] — plan audit that identified this gap
