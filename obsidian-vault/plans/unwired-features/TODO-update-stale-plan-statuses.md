---
title: "TODO: Update Stale Plan Doc Statuses"
tags:
  - docs
  - next-steps
date: 2026-03-17
status: planned
effort: 30m
priority: medium
---

# Update Stale Plan Doc Statuses

> Update `status:` frontmatter in 20+ plan files whose code is already implemented but docs still say `planned` or `partial`.

## Why This Phase

A code audit (2026-03-17) found that the majority of `planned` status tags in plan files are stale — the code has been implemented but the docs were not updated. This misleads agents and contributors about what work remains.

## Detailed Subtasks

For each file below, change only the `status:` YAML frontmatter field.

### Event Trigger Completion phases (partial → complete)

| File | Current | Target |
|------|---------|--------|
| `plans/event-trigger-completion/01-task-lifecycle-emission.md` | `partial` | `complete` |
| `plans/event-trigger-completion/02-security-event-emission.md` | `partial` | `complete` |
| `plans/event-trigger-completion/05-memory-event-emission-and-prompt.md` | `partial` | `complete` |
| `plans/event-trigger-completion/06-communication-and-schedule-emission.md` | `partial` | `complete` |
| `plans/event-trigger-completion/10-system-health-and-hardware-emission.md` | `partial` | `complete` |

### Memory Context Architecture phases (planned → complete/partial)

| File | Current | Target |
|------|---------|--------|
| `plans/memory-context-architecture/03-context-assembly-engine.md` | `planned` | `complete` |
| `plans/memory-context-architecture/04-procedural-memory-tier.md` | `planned` | `complete` |
| `plans/memory-context-architecture/05-adaptive-retrieval-gate.md` | `planned` | `complete` |
| `plans/memory-context-architecture/06-structured-memory-extraction.md` | `planned` | `complete` |
| `plans/memory-context-architecture/07-consolidation-pathways.md` | `planned` | `partial` (engine built, background loop missing) |
| `plans/memory-context-architecture/08-agent-memory-self-management.md` | `planned` | `complete` |
| `plans/memory-context-architecture/Memory Context Architecture Plan.md` | `planned` | `partial` |

### WebUI Security Fixes phases (planned → complete)

| File | Current | Target |
|------|---------|--------|
| `plans/webui-security-fixes/02-cli-and-startup.md` | `planned` | `complete` |
| `plans/webui-security-fixes/03-cors-auth-csp-ratelimit.md` | `planned` | `complete` |
| `plans/webui-security-fixes/04-csrf-protection.md` | `planned` | `complete` |
| `plans/webui-security-fixes/05-xss-and-secrets.md` | `planned` | `complete` |
| `plans/webui-security-fixes/06-tool-install-path-security.md` | `planned` | `complete` |
| `plans/webui-security-fixes/07-sse-and-pipeline-execution.md` | `planned` | `complete` |
| `plans/webui-security-fixes/08-kernel-dispatch-integration.md` | `planned` | `complete` |
| `plans/webui-security-fixes/WebUI Security Fixes Plan.md` | `planned` | `complete` (template dedup TODO remains separately) |
| `plans/webui-security-fixes/01-quick-wins.md` | `planned` | `partial` (template dedup remaining) |

### First Deployment Readiness phases (planned → partial/complete)

| File | Current | Target |
|------|---------|--------|
| `plans/first-deployment-readiness/First Deployment Readiness Plan.md` | `planned` | `partial` |
| `plans/first-deployment-readiness/subtasks/16-00-Code Safety Hardening.md` | `planned` | `complete` |
| `plans/first-deployment-readiness/subtasks/16-01-Restore Quality Gates.md` | `planned` | `complete` |
| `plans/first-deployment-readiness/subtasks/16-02-Harden Production Config.md` | `planned` | `complete` |
| `plans/first-deployment-readiness/subtasks/16-03-Add Container Deployment Artifacts.md` | `planned` | `complete` |
| `plans/first-deployment-readiness/subtasks/16-04-Security Readiness Closure.md` | `planned` | `complete` |
| `plans/first-deployment-readiness/subtasks/16-05-Release Versioning and Tagging.md` | `planned` | `planned` (keep — not done) |
| `plans/first-deployment-readiness/subtasks/16-06-Preflight and Launch Checklist.md` | `planned` | `planned` (keep — not done) |

### Unwired Features phases

| File | Current | Target |
|------|---------|--------|
| `plans/unwired-features/01-emit-missing-event-types.md` | `planned` | `complete` (all 27 non-external events now emitted) |

## Files Changed

All files listed above — only `status:` field in YAML frontmatter changes.

## Dependencies

None — this is documentation-only.

## Verification

```bash
# Confirm key files updated
grep "status:" obsidian-vault/plans/memory-context-architecture/03-context-assembly-engine.md
# Expected: status: complete

grep "status:" obsidian-vault/plans/webui-security-fixes/02-cli-and-startup.md
# Expected: status: complete

grep "status:" obsidian-vault/plans/event-trigger-completion/01-task-lifecycle-emission.md
# Expected: status: complete
```

## Related

- [[audit_report]] — GAP-M01
- [[Unwired Features Plan]] — Phase 04 (stale docs cleanup)
