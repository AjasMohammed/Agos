---
title: "Phase 05: Real-Time Updates via SSE"
tags:
  - webui
  - htmx
  - frontend
  - plan
date: 2026-03-18
status: planned
effort: 2d
priority: high
---

# Phase 05: Real-Time Updates via SSE

> Replace polling-based partial swaps with Server-Sent Events (SSE) for the Dashboard, Agents, Tasks, and Audit Log pages, providing instant updates without repeated polling.

---

## Why This Phase

Currently, every page uses HTMX `hx-trigger="every Ns"` to poll for updates. This approach:

- Creates unnecessary server load (requests even when nothing changed)
- Has a latency floor equal to the poll interval (3-10 seconds)
- Does not scale well with many concurrent browser sessions

SSE provides push-based updates: the server only sends data when something actually changes (or on a reasonable heartbeat). The task detail page (`task_detail.html`) already uses SSE successfully via `EventSource` -- this phase extends that pattern to the remaining pages using the HTMX SSE extension.

---

## Current to Target State

| Page | Current Mechanism | Target Mechanism |
|------|-------------------|------------------|
| Dashboard stats | `hx-trigger="every 5s"` polling | SSE via `/events/dashboard` with `sse-swap` |
| Dashboard agents | `hx-trigger="every 5s"` polling | SSE via `/events/dashboard` (same stream) |
| Dashboard recent audit | `hx-trigger="every 5s"` polling | SSE via `/events/dashboard` (same stream) |
| Agent list | `hx-trigger="every 5s"` polling | SSE via `/events/agents` |
| Task list | `hx-trigger="every 3s"` polling | SSE via `/events/tasks` |
| Audit log | `hx-trigger="every 10s"` polling | SSE via `/events/audit` |
| Task detail log | SSE (EventSource, custom JS) | Keep as-is (already optimal) |
| Tools | `hx-trigger="every 10s"` polling | Keep polling (tools change rarely) |
| Secrets | `hx-trigger="every 10s"` polling | Keep polling (secrets change rarely) |
| Pipelines | `hx-trigger="every 10s"` polling | Keep polling (pipelines change rarely) |

---

## Detailed Subtasks

### 1. Add HTMX SSE extension

Download the HTMX SSE extension and place it in the static directory.

Create `crates/agentos-web/static/js/sse.js` by downloading from the HTMX extensions repository. The SSE extension enables `hx-ext="sse"` with `sse-connect` and `sse-swap` attributes.

Add the script tag to `base.html` (after `htmx.min.js`):

```html
<script src="/static/js/sse.js" defer></script>
```

### 2. Create SSE handler module

Create `crates/agentos-web/src/handlers/events.rs`:

```rust
use crate::state::AppState;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use futures::stream::{self, StreamExt};
use minijinja::context;
use std::convert::Infallible;
use std::time::Duration;

/// SSE endpoint for the dashboard page.
/// Sends named events: `dashboard-stats`, `dashboard-agents`, `dashboard-tasks`, `dashboard-audit`.
pub async fn dashboard_stream(
    State(state): State<AppState>,
) -> Sse<KeepAliveStream<futures::stream::BoxStream<'static, Result<Event, Infallible>>>> {
    let kernel = state.kernel.clone();
    let templates = state.templates.clone();

    let stream = stream::unfold(0u64, move |tick| {
        let kernel = kernel.clone();
        let templates = templates.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(3)).await;

            // Rotate which partial we render each tick to spread the load
            let event_name = match tick % 3 {
                0 => "dashboard-stats",
                1 => "dashboard-agents",
                _ => "dashboard-audit",
            };

            let html = match event_name {
                "dashboard-stats" => {
                    let agent_count = kernel.agent_registry.read().await.list_online().len();
                    let tool_count = kernel.tool_registry.read().await.list_all().len();
                    let tasks = kernel.scheduler.list_tasks().await;
                    let active = kernel.scheduler.running_count().await;
                    let bg = kernel.background_pool.list_running().await.len();
                    let uptime = chrono::Utc::now()
                        .signed_duration_since(kernel.started_at)
                        .num_seconds();
                    let uptime_display = super::dashboard::format_uptime(uptime);

                    let ctx = context! {
                        agent_count, tool_count,
                        active_task_count => active,
                        total_task_count => tasks.len(),
                        bg_running => bg,
                        uptime_secs => uptime,
                        uptime_display,
                    };
                    render_partial(&templates, "partials/dashboard_stats.html", ctx)
                }
                "dashboard-agents" => {
                    let registry = kernel.agent_registry.read().await;
                    let agents: Vec<_> = registry.list_online().iter().map(|a| {
                        context! {
                            name => a.name.clone(),
                            provider => format!("{:?}", a.provider),
                            model => a.model.clone(),
                            status => format!("{:?}", a.status),
                            current_task => a.current_task.as_ref().map(|t| t.to_string()),
                        }
                    }).collect();
                    let ctx = context! { agents };
                    render_partial(&templates, "partials/dashboard_agents.html", ctx)
                }
                _ => {
                    let audit = kernel.audit.clone();
                    let entries = match tokio::task::spawn_blocking(move || {
                        audit.query_recent(10)
                    }).await {
                        Ok(Ok(e)) => e,
                        _ => vec![],
                    };
                    let rows: Vec<_> = entries.iter().map(|e| context! {
                        timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                        event_type => format!("{:?}", e.event_type),
                        severity => format!("{:?}", e.severity),
                        agent_id => e.agent_id.as_ref().map(|id| id.to_string()),
                    }).collect();
                    // Return raw table rows as data
                    let html = rows.iter().map(|r| {
                        format!("<tr><td><code class=\"ts\">{}</code></td><td><code class=\"event-type\">{}</code></td><td><span class=\"badge badge-{}\">{}</span></td><td>{}</td></tr>",
                            r.get_attr("timestamp").unwrap(),
                            r.get_attr("event_type").unwrap(),
                            r.get_attr("severity").unwrap().to_string().to_lowercase(),
                            r.get_attr("severity").unwrap(),
                            r.get_attr("agent_id").map(|v| v.to_string()).unwrap_or_else(|_| "--".to_string()),
                        )
                    }).collect::<Vec<_>>().join("\n");
                    html
                }
            };

            Some((
                Ok(Event::default().event(event_name).data(html)),
                tick + 1,
            ))
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

/// SSE endpoint for the agents page.
pub async fn agents_stream(
    State(state): State<AppState>,
) -> Sse<KeepAliveStream<futures::stream::BoxStream<'static, Result<Event, Infallible>>>> {
    let kernel = state.kernel.clone();
    let templates = state.templates.clone();

    let stream = stream::unfold((), move |()| {
        let kernel = kernel.clone();
        let templates = templates.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(3)).await;

            let registry = kernel.agent_registry.read().await;
            let agents: Vec<_> = registry.list_online().iter().map(|a| {
                context! {
                    id => a.id.to_string(),
                    name => a.name.clone(),
                    provider => format!("{:?}", a.provider),
                    model => a.model.clone(),
                    status => format!("{:?}", a.status),
                    description => a.description.clone(),
                    roles => a.roles.clone(),
                    current_task => a.current_task.as_ref().map(|t| t.to_string()),
                    created_at => a.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    last_active => a.last_active.format("%Y-%m-%d %H:%M:%S").to_string(),
                }
            }).collect();
            drop(registry);

            let ctx = context! { agents };
            let html = render_partial(&templates, "partials/agent_card.html", ctx);

            Some((
                Ok(Event::default().event("agent-update").data(html)),
                (),
            ))
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

/// SSE endpoint for the tasks page.
pub async fn tasks_stream(
    State(state): State<AppState>,
) -> Sse<KeepAliveStream<futures::stream::BoxStream<'static, Result<Event, Infallible>>>> {
    let kernel = state.kernel.clone();
    let templates = state.templates.clone();

    let stream = stream::unfold((), move |()| {
        let kernel = kernel.clone();
        let templates = templates.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(2)).await;

            let tasks = kernel.scheduler.list_tasks().await;
            let task_rows: Vec<_> = tasks.iter().map(|t| {
                context! {
                    id => t.id.to_string(),
                    state => format!("{:?}", t.state),
                    agent_id => t.agent_id.to_string(),
                    prompt_preview => t.prompt_preview.clone(),
                    created_at => t.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    tool_calls => t.tool_calls,
                    tokens_used => t.tokens_used,
                    priority => t.priority,
                }
            }).collect();

            let ctx = context! { tasks => task_rows };
            let html = render_partial(&templates, "partials/task_row.html", ctx);

            Some((
                Ok(Event::default().event("task-update").data(html)),
                (),
            ))
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

/// SSE endpoint for the audit log page.
pub async fn audit_stream(
    State(state): State<AppState>,
) -> Sse<KeepAliveStream<futures::stream::BoxStream<'static, Result<Event, Infallible>>>> {
    let kernel = state.kernel.clone();
    let templates = state.templates.clone();

    let stream = stream::unfold((), move |()| {
        let kernel = kernel.clone();
        let templates = templates.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(5)).await;

            let audit = kernel.audit.clone();
            let (entries, _count) = match tokio::task::spawn_blocking(move || {
                let entries = audit.query_recent(50).unwrap_or_default();
                let count = audit.count().unwrap_or(0);
                (entries, count)
            }).await {
                Ok(r) => r,
                Err(_) => (vec![], 0),
            };

            let rows: Vec<_> = entries.iter().map(|e| {
                context! {
                    timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                    event_type => format!("{:?}", e.event_type),
                    severity => format!("{:?}", e.severity),
                    agent_id => e.agent_id.as_ref().map(|id| id.to_string()),
                    task_id => e.task_id.as_ref().map(|id| id.to_string()),
                    tool_id => e.tool_id.as_ref().map(|id| id.to_string()),
                    details => e.details.to_string(),
                }
            }).collect();

            let ctx = context! { entries => rows };
            let html = render_partial(&templates, "partials/log_line.html", ctx);

            Some((
                Ok(Event::default().event("audit-update").data(html)),
                (),
            ))
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

/// Helper: render a MiniJinja template to a string, returning empty string on error.
fn render_partial(
    templates: &minijinja::Environment<'_>,
    name: &str,
    ctx: minijinja::value::Value,
) -> String {
    templates
        .get_template(name)
        .and_then(|t| t.render(ctx))
        .unwrap_or_default()
}
```

### 3. Register SSE routes in `router.rs`

Open `crates/agentos-web/src/router.rs`. Add event stream routes:

```rust
// SSE event streams
.route("/events/dashboard", axum::routing::get(events::dashboard_stream))
.route("/events/agents", axum::routing::get(events::agents_stream))
.route("/events/tasks", axum::routing::get(events::tasks_stream))
.route("/events/audit", axum::routing::get(events::audit_stream))
```

### 4. Register the events module

Open `crates/agentos-web/src/handlers/mod.rs`. Add:

```rust
pub mod events;
```

Import in `router.rs`:

```rust
use crate::handlers::{agents, audit, dashboard, events, pipelines, secrets, tasks, tools};
```

### 5. Update templates to use SSE instead of polling

Update `dashboard.html` -- replace `hx-trigger="every 5s"` with SSE:

```html
<div hx-ext="sse" sse-connect="/events/dashboard">
    <section id="dashboard-stats" sse-swap="dashboard-stats">
        {% include "partials/dashboard_stats.html" %}
    </section>
    <!-- ... agents and audit sections similarly -->
</div>
```

Update `agents.html` -- replace polling on agent grid:

```html
<div id="agent-grid" class="grid"
     hx-ext="sse" sse-connect="/events/agents" sse-swap="agent-update">
    {% include "partials/agent_card.html" %}
</div>
```

Update `tasks.html` -- replace polling on task list:

```html
<tbody id="task-list"
       hx-ext="sse" sse-connect="/events/tasks" sse-swap="task-update">
    {% include "partials/task_row.html" %}
</tbody>
```

Update `audit.html` -- the audit log keeps polling for now because SSE would conflict with filter parameters. Instead, add SSE as an optional "live mode":

```html
<tbody id="audit-body" hx-get="/audit?partial=list" hx-trigger="every 10s" hx-swap="innerHTML">
    {% include "partials/log_line.html" %}
</tbody>
```

The audit page keeps polling because filters are applied server-side per request. SSE would require sending filter state in the SSE connection URL, which adds complexity. The audit SSE endpoint is available for future "live tail" mode.

### 6. Add SSE connection lifecycle management to `app.js`

Open `crates/agentos-web/static/js/app.js`. Add visibility-based SSE connection management:

```javascript
// Close SSE connections when tab is hidden to save resources
document.addEventListener('visibilitychange', function() {
    if (document.hidden) {
        // HTMX SSE extension handles this automatically when elements are removed,
        // but we ensure cleanup on tab hide
        document.querySelectorAll('[sse-connect]').forEach(function(el) {
            if (el._sseEventSource) {
                el._sseEventSource.close();
            }
        });
    }
});
```

### 7. Update CSP header for SSE connections

Open `crates/agentos-web/src/router.rs`. The current CSP already allows `connect-src 'self'` which covers SSE connections to the same origin. No changes needed.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/handlers/events.rs` | **New** -- SSE handlers for dashboard, agents, tasks, audit streams |
| `crates/agentos-web/src/handlers/mod.rs` | Add `pub mod events;` |
| `crates/agentos-web/src/router.rs` | Add 4 SSE routes under `/events/*`, import events module |
| `crates/agentos-web/src/templates/dashboard.html` | Replace `hx-trigger` polling with `hx-ext="sse"` |
| `crates/agentos-web/src/templates/agents.html` | Replace `hx-trigger` polling with `hx-ext="sse"` |
| `crates/agentos-web/src/templates/tasks.html` | Replace `hx-trigger` polling with `hx-ext="sse"` |
| `crates/agentos-web/static/js/sse.js` | **New** -- HTMX SSE extension (vendor file) |
| `crates/agentos-web/static/js/app.js` | Add visibility-based SSE lifecycle management |
| `crates/agentos-web/src/templates/base.html` | Add `<script>` tag for `sse.js` |

---

## Dependencies

- [[01-layout-navigation]] must be complete (base.html changes)
- [[02-agent-dashboard]] must be complete (dashboard partials exist)
- [[03-task-management]] must be complete (task row partial has new columns)
- [[04-audit-log-viewer]] should be complete (audit partial has severity filter)

---

## Test Plan

- `cargo build -p agentos-web` must compile
- `cargo test -p agentos-web` must pass
- `cargo clippy -p agentos-web -- -D warnings` must pass
- Manual verification:
  - Dashboard opens an SSE connection to `/events/dashboard` (visible in browser DevTools Network tab as `EventSource`)
  - Dashboard stats update in real-time when agents connect/disconnect
  - Agent page updates immediately when an agent connects (no 5s delay)
  - Task page updates immediately when a task state changes
  - SSE connections close when the browser tab is hidden
  - SSE connections reconnect when the tab becomes visible again
  - No JavaScript errors in the browser console
  - Pages still render correctly on initial load (SSE enhances, not replaces)
  - The task detail page SSE log stream (existing) continues to work correctly
  - Tools, Secrets, and Pipelines pages still use polling (unchanged)

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
cargo clippy -p agentos-web -- -D warnings
cargo fmt -p agentos-web -- --check
```
