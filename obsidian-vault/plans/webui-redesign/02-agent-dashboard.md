---
title: "Phase 02: Agent Dashboard Enhancement"
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

# Phase 02: Agent Dashboard Enhancement

> Upgrade the dashboard from a bare stats page to an informative command center with agent status cards, task queue summary, resource usage indicators, and enhanced stat widgets.

---

## Why This Phase

The dashboard is the landing page -- the first thing operators see. Currently it shows four number cards (uptime, agent count, task count, tool count) and a raw audit table. This provides no actionable insight. Operators need to see:

- Which agents are online/idle/busy at a glance
- How many tasks are queued vs running vs completed
- Recent activity with meaningful formatting
- Quick actions (connect agent, submit task) without navigating away

---

## Current to Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Stat cards | 4 plain text cards (uptime, agent count, task count, tool count) | 6 styled stat widgets with icons, trend context, and links |
| Agent overview | Just a count: "3 connected" | Agent status breakdown showing online/idle/busy agents in a compact card grid |
| Task summary | Just counts: "2 active / 5 total" | Task status breakdown with colored indicators (queued, running, completed, failed) |
| Recent audit | Raw table polling every 5s | Styled recent events list with severity badges and relative timestamps |
| Quick actions | "Manage" / "View" link buttons | Quick action bar: Connect Agent, Submit Task |
| Layout | Single `.grid` with 4 equal cards | Two-row layout: stat bar on top, detailed panels below |
| Template | `dashboard.html` (51 lines) | `dashboard.html` (refactored) + `partials/dashboard_stats.html` + `partials/dashboard_agents.html` + `partials/dashboard_tasks.html` |

---

## Detailed Subtasks

### 1. Create `partials/dashboard_stats.html`

Create `crates/agentos-web/src/templates/partials/dashboard_stats.html`:

```html
<div class="stat-grid">
    <div class="stat-card">
        <div class="stat-value">{{ agent_count }}</div>
        <div class="stat-label">Agents Online</div>
    </div>
    <div class="stat-card">
        <div class="stat-value">{{ active_task_count }}</div>
        <div class="stat-label">Active Tasks</div>
    </div>
    <div class="stat-card">
        <div class="stat-value">{{ total_task_count }}</div>
        <div class="stat-label">Total Tasks</div>
    </div>
    <div class="stat-card">
        <div class="stat-value">{{ tool_count }}</div>
        <div class="stat-label">Tools Installed</div>
    </div>
    <div class="stat-card">
        <div class="stat-value">{{ bg_running }}</div>
        <div class="stat-label">Background Jobs</div>
    </div>
    <div class="stat-card">
        <div class="stat-value">{{ uptime_display }}</div>
        <div class="stat-label">Kernel Uptime</div>
    </div>
</div>
```

### 2. Create `partials/dashboard_agents.html`

Create `crates/agentos-web/src/templates/partials/dashboard_agents.html`:

```html
{% if agents %}
<div class="agent-status-grid">
    {% for agent in agents %}
    <div class="agent-status-card">
        <div class="agent-status-header">
            <strong>{{ agent.name }}</strong>
            <span class="badge badge-{{ agent.status|lower }}">{{ agent.status }}</span>
        </div>
        <div class="agent-status-meta">
            <small class="muted">{{ agent.provider }} / {{ agent.model }}</small>
        </div>
        {% if agent.current_task %}
        <div class="agent-status-task">
            <small>Working on <a href="/tasks/{{ agent.current_task }}"><code>{{ agent.current_task[:8] }}</code></a></small>
        </div>
        {% endif %}
    </div>
    {% endfor %}
</div>
{% else %}
<div class="empty-state" role="status">
    <p class="empty-state-icon" aria-hidden="true">&#9679;</p>
    <p class="empty-state-text">No agents connected</p>
    <a href="/agents" role="button" class="outline">Connect an Agent</a>
</div>
{% endif %}
```

### 3. Create `partials/dashboard_tasks.html`

Create `crates/agentos-web/src/templates/partials/dashboard_tasks.html`:

```html
{% if task_summary %}
<div class="task-summary-bar">
    {% if task_summary.queued > 0 %}
    <div class="task-summary-segment task-queued" title="{{ task_summary.queued }} queued"
         style="flex: {{ task_summary.queued }}">{{ task_summary.queued }} queued</div>
    {% endif %}
    {% if task_summary.running > 0 %}
    <div class="task-summary-segment task-running" title="{{ task_summary.running }} running"
         style="flex: {{ task_summary.running }}">{{ task_summary.running }} running</div>
    {% endif %}
    {% if task_summary.completed > 0 %}
    <div class="task-summary-segment task-completed" title="{{ task_summary.completed }} completed"
         style="flex: {{ task_summary.completed }}">{{ task_summary.completed }} completed</div>
    {% endif %}
    {% if task_summary.failed > 0 %}
    <div class="task-summary-segment task-failed" title="{{ task_summary.failed }} failed"
         style="flex: {{ task_summary.failed }}">{{ task_summary.failed }} failed</div>
    {% endif %}
</div>
{% else %}
<div class="empty-state" role="status">
    <p class="empty-state-icon" aria-hidden="true">&#9654;</p>
    <p class="empty-state-text">No tasks yet</p>
    <a href="/tasks" role="button" class="outline">View Tasks</a>
</div>
{% endif %}
```

### 4. Rewrite `dashboard.html`

Replace `crates/agentos-web/src/templates/dashboard.html`:

```html
{% extends "base.html" %}
{% block content %}
<div class="page-header">
    <h1>Dashboard</h1>
    <div class="page-actions">
        <a href="/agents" role="button" class="outline btn-sm">Connect Agent</a>
    </div>
</div>

<section id="dashboard-stats"
         hx-get="/dashboard-stats" hx-trigger="every 5s" hx-swap="innerHTML">
    {% include "partials/dashboard_stats.html" %}
</section>

<div class="dashboard-panels">
    <article>
        <header>
            <strong>Agents</strong>
            <a href="/agents" class="muted" style="margin-left: auto; font-size: 0.85rem;">View all</a>
        </header>
        <div id="dashboard-agents"
             hx-get="/dashboard-agents" hx-trigger="every 5s" hx-swap="innerHTML">
            {% include "partials/dashboard_agents.html" %}
        </div>
    </article>

    <article>
        <header>
            <strong>Task Status</strong>
            <a href="/tasks" class="muted" style="margin-left: auto; font-size: 0.85rem;">View all</a>
        </header>
        <div id="dashboard-tasks"
             hx-get="/dashboard-tasks" hx-trigger="every 5s" hx-swap="innerHTML">
            {% include "partials/dashboard_tasks.html" %}
        </div>
    </article>
</div>

<article>
    <header>
        <strong>Recent Activity</strong>
        <a href="/audit" class="muted" style="margin-left: auto; font-size: 0.85rem;">View all</a>
    </header>
    <div id="dashboard-recent-audit"
         hx-get="/audit?partial=list&limit=10" hx-trigger="every 5s" hx-swap="innerHTML">
        <table class="audit-table">
            <thead>
                <tr>
                    <th>Timestamp</th>
                    <th>Event</th>
                    <th>Severity</th>
                    <th>Agent</th>
                </tr>
            </thead>
            <tbody>
                {% for entry in recent_audit %}
                <tr>
                    <td><code class="ts">{{ entry.timestamp }}</code></td>
                    <td><code class="event-type">{{ entry.event_type }}</code></td>
                    <td><span class="badge badge-{{ entry.severity|lower }}">{{ entry.severity }}</span></td>
                    <td>{% if entry.agent_id %}<code class="id-short">{{ entry.agent_id[:8] }}</code>{% else %}<span class="muted">--</span>{% endif %}</td>
                </tr>
                {% endfor %}
            </tbody>
        </table>
    </div>
</article>
{% endblock %}
```

### 5. Update `handlers/dashboard.rs` with new data and partial endpoints

Open `crates/agentos-web/src/handlers/dashboard.rs`.

Expand the `index()` handler to compute task summary breakdown and agent list. Add two new handlers for the dashboard partials:

```rust
use agentos_types::TaskState;

pub async fn index(State(state): State<AppState>, jar: CookieJar) -> Response {
    let registry = state.kernel.agent_registry.read().await;
    let agents: Vec<_> = registry.list_online().iter().map(|a| {
        context! {
            name => a.name.clone(),
            provider => format!("{:?}", a.provider),
            model => a.model.clone(),
            status => format!("{:?}", a.status),
            current_task => a.current_task.as_ref().map(|t| t.to_string()),
        }
    }).collect();
    let agent_count = agents.len();
    drop(registry);

    let tool_count = state.kernel.tool_registry.read().await.list_all().len();
    let tasks = state.kernel.scheduler.list_tasks().await;

    // Task summary breakdown
    let mut queued = 0u32;
    let mut running = 0u32;
    let mut completed = 0u32;
    let mut failed = 0u32;
    for t in &tasks {
        match t.state {
            TaskState::Pending | TaskState::Queued => queued += 1,
            TaskState::Running => running += 1,
            TaskState::Complete => completed += 1,
            TaskState::Failed | TaskState::Cancelled => failed += 1,
        }
    }

    let uptime_secs = chrono::Utc::now()
        .signed_duration_since(state.kernel.started_at)
        .num_seconds();
    let uptime_display = format_uptime(uptime_secs);
    let bg_running = state.kernel.background_pool.list_running().await.len();

    // Recent audit (same as before)
    let audit = state.kernel.audit.clone();
    let recent_audit = match tokio::task::spawn_blocking(move || audit.query_recent(10)).await {
        Ok(result) => result.unwrap_or_default(),
        Err(e) => {
            tracing::error!("dashboard audit query panicked: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Dashboard",
        breadcrumbs => vec![context! { label => "Dashboard" }],
        csrf_token,
        agent_count,
        agents,
        tool_count,
        active_task_count => running as usize,
        total_task_count => tasks.len(),
        task_summary => context! { queued, running, completed, failed },
        recent_audit => recent_audit.iter().map(|e| context! {
            timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            event_type => format!("{:?}", e.event_type),
            severity => format!("{:?}", e.severity),
            agent_id => e.agent_id.as_ref().map(|id| id.to_string()),
        }).collect::<Vec<_>>(),
        uptime_secs,
        uptime_display,
        bg_running,
    };

    super::render(&state.templates, "dashboard.html", ctx)
}

fn format_uptime(secs: i64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{}d {}h {}m", days, hours, mins)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}
```

Add partial endpoints `dashboard_stats` and `dashboard_agents` and `dashboard_tasks` for HTMX swap targets. These are simpler versions of the `index()` handler that return only the partial:

```rust
pub async fn stats_partial(State(state): State<AppState>) -> Response {
    // Compute same stats as index, render partials/dashboard_stats.html
}

pub async fn agents_partial(State(state): State<AppState>) -> Response {
    // Compute agent list, render partials/dashboard_agents.html
}

pub async fn tasks_partial(State(state): State<AppState>) -> Response {
    // Compute task summary, render partials/dashboard_tasks.html
}
```

### 6. Register new routes in `router.rs`

Open `crates/agentos-web/src/router.rs`.

Add routes for the new dashboard partials:

```rust
.route("/dashboard-stats", axum::routing::get(dashboard::stats_partial))
.route("/dashboard-agents", axum::routing::get(dashboard::agents_partial))
.route("/dashboard-tasks", axum::routing::get(dashboard::tasks_partial))
```

### 7. Add dashboard-specific CSS to `app.css`

```css
/* ── Dashboard Stat Grid ────────────────────────────────── */
.stat-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 1rem;
    margin-bottom: 1.5rem;
}
.stat-card {
    background: var(--pico-card-background-color);
    border: 1px solid var(--pico-muted-border-color);
    border-radius: var(--pico-border-radius);
    padding: 1rem;
    text-align: center;
}
.stat-value { font-size: 1.8rem; font-weight: 700; color: var(--pico-primary); }
.stat-label { font-size: 0.8rem; color: var(--pico-muted-color); text-transform: uppercase; letter-spacing: 0.05em; margin-top: 0.25rem; }

/* ── Dashboard Panels ───────────────────────────────────── */
.dashboard-panels {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
    gap: 1rem;
    margin-bottom: 1.5rem;
}

/* ── Agent Status Grid ──────────────────────────────────── */
.agent-status-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 0.75rem;
}
.agent-status-card {
    border: 1px solid var(--pico-muted-border-color);
    border-radius: var(--pico-border-radius);
    padding: 0.75rem;
}
.agent-status-header { display: flex; align-items: center; justify-content: space-between; }
.agent-status-meta { margin-top: 0.35rem; }
.agent-status-task { margin-top: 0.35rem; }

/* ── Task Summary Bar ───────────────────────────────────── */
.task-summary-bar {
    display: flex;
    border-radius: var(--pico-border-radius);
    overflow: hidden;
    min-height: 2rem;
    font-size: 0.75rem;
    font-weight: 600;
    color: #fff;
}
.task-summary-segment {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0.25rem 0.5rem;
    min-width: 2rem;
}
.task-queued { background: #ffc107; color: #000; }
.task-running { background: #17a2b8; }
.task-completed { background: #28a745; }
.task-failed { background: #dc3545; }
```

### 8. Register new templates in `templates.rs`

Open `crates/agentos-web/src/templates.rs`.

Add:

```rust
env.add_template(
    "partials/dashboard_stats.html",
    include_str!("templates/partials/dashboard_stats.html"),
)?;
env.add_template(
    "partials/dashboard_agents.html",
    include_str!("templates/partials/dashboard_agents.html"),
)?;
env.add_template(
    "partials/dashboard_tasks.html",
    include_str!("templates/partials/dashboard_tasks.html"),
)?;
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/dashboard.html` | Rewrite with stat grid, agent panel, task panel, recent audit |
| `crates/agentos-web/src/templates/partials/dashboard_stats.html` | **New** -- stat card grid partial |
| `crates/agentos-web/src/templates/partials/dashboard_agents.html` | **New** -- agent status card grid partial |
| `crates/agentos-web/src/templates/partials/dashboard_tasks.html` | **New** -- task summary bar partial |
| `crates/agentos-web/src/handlers/dashboard.rs` | Expand `index()` with task breakdown + agent list; add `stats_partial()`, `agents_partial()`, `tasks_partial()` |
| `crates/agentos-web/src/router.rs` | Add `/dashboard-stats`, `/dashboard-agents`, `/dashboard-tasks` routes |
| `crates/agentos-web/src/templates.rs` | Register 3 new partials |
| `crates/agentos-web/static/css/app.css` | Add stat-grid, dashboard-panels, agent-status-grid, task-summary-bar styles |

---

## Dependencies

[[01-layout-navigation]] must be complete first (the dashboard extends the new `base.html` shell).

---

## Test Plan

- `cargo build -p agentos-web` must compile
- `cargo test -p agentos-web` must pass
- `cargo clippy -p agentos-web -- -D warnings` must pass
- Manual verification:
  - Dashboard shows 6 stat cards with correct values
  - Agent status grid shows each connected agent with name, provider/model, status badge
  - Task summary bar renders colored segments proportional to queued/running/completed/failed counts
  - With zero agents, the agent panel shows the "No agents connected" empty state
  - With zero tasks, the task panel shows the "No tasks yet" empty state
  - HTMX partial endpoints (`/dashboard-stats`, `/dashboard-agents`, `/dashboard-tasks`) return valid HTML fragments
  - Partial swaps update every 5 seconds without full page reload
  - Uptime displays in human-readable format ("1d 2h 15m")

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
cargo clippy -p agentos-web -- -D warnings
cargo fmt -p agentos-web -- --check
```
