---
title: WebUI Redesign Data Flow
tags:
  - webui
  - htmx
  - frontend
  - flow
date: 2026-03-18
status: planned
effort: 0d
priority: high
---

# WebUI Redesign Data Flow

> How data flows from the Axum server through HTMX partials and SSE streams to update the browser UI.

---

## Full Page Load Flow

```mermaid
sequenceDiagram
    participant Browser
    participant Axum as Axum Server
    participant MJ as MiniJinja
    participant Kernel

    Browser->>Axum: GET /agents
    Axum->>Axum: Auth middleware (cookie check)
    Axum->>Axum: CSRF middleware (generate token)
    Axum->>Kernel: agent_registry.read()
    Kernel-->>Axum: Vec<AgentInfo>
    Axum->>MJ: render("agents.html", {agents, csrf_token})
    MJ->>MJ: Extend base.html, include partials
    MJ-->>Axum: Full HTML document
    Axum-->>Browser: 200 OK + HTML
    Browser->>Browser: HTMX initializes, Alpine.js initializes
```

## HTMX Partial Swap Flow

```mermaid
sequenceDiagram
    participant Browser
    participant HTMX
    participant Axum
    participant MJ as MiniJinja
    participant Kernel

    Note over HTMX: hx-trigger fires (timer, user action)
    HTMX->>Axum: GET /agents?partial=list
    Note over HTMX: Adds X-CSRF-Token header via csrf.js
    Axum->>Kernel: agent_registry.read()
    Kernel-->>Axum: Vec<AgentInfo>
    Axum->>MJ: render("partials/agent_card.html", {agents})
    MJ-->>Axum: HTML fragment (no base.html wrapper)
    Axum-->>HTMX: 200 OK + HTML fragment
    HTMX->>Browser: Replace innerHTML of target element
    Note over Browser: Alpine.js re-scans for x-data on new DOM
```

## SSE Live Update Flow (new)

```mermaid
sequenceDiagram
    participant Browser
    participant SSEExt as HTMX SSE Extension
    participant Axum
    participant MJ as MiniJinja
    participant Kernel

    Browser->>SSEExt: Connect to /events/agents
    SSEExt->>Axum: GET /events/agents (EventSource)
    Axum-->>SSEExt: 200 OK (text/event-stream)

    loop Every 3 seconds
        Axum->>Kernel: agent_registry.read()
        Kernel-->>Axum: Vec<AgentInfo>
        Axum->>MJ: render partial
        MJ-->>Axum: HTML fragment
        Axum-->>SSEExt: event: agent-update\ndata: <html>
        SSEExt->>Browser: Swap target element with received HTML
    end

    Note over Browser: Tab hidden (visibilitychange)
    Browser->>SSEExt: Close EventSource
```

## Form Submission with Toast Flow (new)

```mermaid
sequenceDiagram
    participant User
    participant Alpine as Alpine.js
    participant HTMX
    participant Axum
    participant Kernel

    User->>HTMX: Submit form (hx-post="/agents")
    HTMX->>Axum: POST /agents (with X-CSRF-Token)
    Axum->>Axum: CSRF middleware (validate token)
    Axum->>Kernel: api_connect_agent(...)
    Kernel-->>Axum: Ok(())

    alt Success
        Axum-->>HTMX: 200 OK + partial HTML
        Note over Axum: HX-Trigger: {"showToast": {"message": "Agent connected", "type": "success"}}
        HTMX->>HTMX: Swap target with new content
        HTMX->>Alpine: Dispatch showToast event
        Alpine->>User: Show success toast (auto-dismiss 5s)
    else Error
        Axum-->>HTMX: 400 Bad Request
        Note over Axum: HX-Trigger: {"showToast": {"message": "Failed to connect", "type": "error"}}
        HTMX->>Alpine: Dispatch showToast event
        Alpine->>User: Show error toast (auto-dismiss 8s)
    end
```

## Template Rendering Architecture

```
base.html
  +-- Topbar partial (partials/topbar.html)
  +-- Sidebar partial (partials/sidebar.html)
  +-- Toast container (partials/toast_container.html)
  +-- {% block content %}
       |
       +-- dashboard.html
       |     +-- partials/dashboard_stats.html (HTMX swap target)
       |     +-- partials/dashboard_recent_audit.html (HTMX swap target)
       |
       +-- agents.html
       |     +-- partials/agent_card.html (HTMX swap target)
       |     +-- partials/empty_state.html (when no agents)
       |
       +-- tasks.html
       |     +-- partials/task_row.html (HTMX swap target)
       |     +-- partials/empty_state.html (when no tasks)
       |
       +-- task_detail.html
       |     +-- (inline SSE log terminal, already exists)
       |
       +-- tools.html
       |     +-- partials/tool_card.html (HTMX swap target)
       |     +-- partials/empty_state.html (when no tools)
       |
       +-- secrets.html
       |     +-- partials/secret_row.html (HTMX swap target)
       |     +-- partials/empty_state.html (when no secrets)
       |
       +-- pipelines.html
       |     +-- partials/pipeline_row.html (HTMX swap target)
       |     +-- partials/empty_state.html (when no pipelines)
       |
       +-- audit.html
             +-- partials/log_line.html (HTMX swap target)
             +-- partials/audit_filters.html (filter bar)
```

---

## Data Sources by Page

| Page | Kernel Data Source | Refresh Mechanism | Current | Target |
|------|-------------------|-------------------|---------|--------|
| Dashboard | `agent_registry`, `scheduler`, `tool_registry`, `audit`, `background_pool` | Full page load | Polling (5s via HTMX on audit table only) | SSE for stats + recent audit |
| Agents | `agent_registry.list_online()` | `?partial=list` | Polling 5s | SSE `agent-update` events |
| Tasks | `scheduler.list_tasks()` | `?partial=list` | Polling 3s | SSE `task-update` events |
| Task Detail | `scheduler.get_task()`, `audit.query_since_for_task()` | SSE (already) | SSE (done) | Keep as-is |
| Tools | `tool_registry.list_all()` | `?partial=list` | Polling 10s | Keep polling (tools change rarely) |
| Secrets | `vault.list()` | `?partial=list` | Polling 10s | Keep polling (secrets change rarely) |
| Pipelines | `pipeline_engine.store_arc().list_pipelines()` | `?partial=list` | Polling 10s | Keep polling (pipelines change rarely) |
| Audit | `audit.query_recent()`, `audit.count()` | `?partial=list` | Polling 10s | SSE `audit-entry` events |

---

## Related

- [[WebUI Redesign Plan]]
- [[WebUI Redesign Research]]
