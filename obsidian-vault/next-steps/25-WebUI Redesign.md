---
title: WebUI Redesign
tags:
  - webui
  - htmx
  - frontend
  - next-steps
date: 2026-03-18
status: planned
effort: 12d
priority: high
---

# WebUI Redesign

> Transform the AgentOS web dashboard from a functional prototype into a polished, interactive management console with sidebar navigation, real-time SSE updates, toast notifications, and comprehensive accessibility.

---

## Current State

The web UI (`crates/agentos-web/`) is functional: 7 pages (Dashboard, Agents, Tasks, Tools, Secrets, Pipelines, Audit) render data via MiniJinja templates, support CRUD operations via HTMX partial swaps, and poll for updates. However, there is no sidebar layout, no empty states, no loading indicators, no toast feedback, no keyboard shortcuts, and only the task detail page uses SSE for live updates.

## Goal / Target State

A production-quality dashboard with:
- Responsive sidebar + topbar shell layout with theme toggle and breadcrumbs
- Rich dashboard with agent status cards, task breakdown bar, and stat widgets
- Task filtering/search, cancel actions, and improved detail layout
- Audit log with severity filtering and clear-filters button
- SSE-based live updates replacing polling on Dashboard, Agents, Tasks pages
- Consistent empty states, skeleton loaders, toast notifications, and ARIA labels across all pages

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[01-layout-navigation]] | `base.html`, `app.css`, `app.js`, all handlers | planned |
| 02 | [[02-agent-dashboard]] | `dashboard.html`, `dashboard.rs`, `router.rs`, `templates.rs`, 3 new partials | planned |
| 03 | [[03-task-management]] | `tasks.html`, `task_row.html`, `task_detail.html`, `tasks.rs`, `router.rs` | planned |
| 04 | [[04-audit-log-viewer]] | `audit.html`, `log_line.html`, `audit.rs`, `app.css` | planned |
| 05 | [[05-real-time-updates]] | `events.rs` (new), `router.rs`, `mod.rs`, dashboard/agents/tasks templates, `sse.js` | planned |
| 06 | [[06-ux-polish]] | 3 new partials, `base.html`, 5 page templates, `app.css`, `app.js`, all CRUD handlers | planned |

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
cargo clippy -p agentos-web -- -D warnings
cargo fmt -p agentos-web -- --check
```

## Related

- [[WebUI Redesign Plan]]
- [[WebUI Redesign Research]]
- [[WebUI Redesign Data Flow]]
- [[23-WebUI Security Fixes]]
