---
title: "Phase 03: Task Management Enhancement"
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

# Phase 03: Task Management Enhancement

> Add task filtering, search, status badges, cancel action, and improved task detail layout to the Task Inspector.

---

## Why This Phase

The Task Inspector is the primary interface for monitoring agent work. Currently it is a bare table with clickable rows and 3-second polling. Operators need:

- **Filtering by status** -- quickly find failed or running tasks without scanning
- **Search by prompt text** -- locate tasks by what was asked
- **Cancel action** -- stop runaway tasks from the UI instead of the CLI
- **Visual status progression** -- clear badges and progress indicators
- **Improved task detail** -- better layout for context window, cancel button, metadata

The task detail page already has a good SSE log terminal. This phase focuses on the list page and task detail metadata sections.

---

## Current to Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Task list filter | None | Filter bar with status dropdown and text search input |
| Task list columns | ID, Agent, Status, Prompt, Created, Tool Calls, Tokens | Same + Priority column, sortable by created date |
| Empty state | None (shows empty table) | "No tasks found" message with context |
| Task cancel | Not available in UI | Delete/cancel button on task rows and task detail |
| Task detail metadata | Simple `<dl>` in a card | Enhanced card with duration, token count, tool call count |
| Task detail cancel | Not available | Cancel button (hx-delete) with confirmation |
| Task detail context window | Expandable `<details>` entries | Same but with message count badge and role color indicators |
| Partial swap | `?partial=list` returns task rows | Same pattern with filter/search params |

---

## Detailed Subtasks

### 1. Add task filter bar to `tasks.html`

Open `crates/agentos-web/src/templates/tasks.html`. Replace the content:

```html
{% extends "base.html" %}
{% block content %}
<div class="page-header">
    <h1>Task Inspector</h1>
</div>

<div class="filter-bar">
    <input type="text" name="search" placeholder="Search by prompt..."
           hx-get="/tasks" hx-target="#task-list" hx-swap="innerHTML"
           hx-trigger="keyup changed delay:300ms"
           hx-include="[name='search'],[name='status']"
           hx-vals='{"partial": "list"}'>
    <select name="status"
            hx-get="/tasks" hx-target="#task-list" hx-swap="innerHTML"
            hx-trigger="change"
            hx-include="[name='search'],[name='status']"
            hx-vals='{"partial": "list"}'>
        <option value="">All Statuses</option>
        <option value="pending">Pending</option>
        <option value="queued">Queued</option>
        <option value="running">Running</option>
        <option value="complete">Complete</option>
        <option value="failed">Failed</option>
        <option value="cancelled">Cancelled</option>
    </select>
    <button class="outline secondary btn-sm"
            hx-get="/tasks?partial=list"
            hx-target="#task-list"
            hx-swap="innerHTML">Refresh</button>
</div>

<figure>
<table>
    <thead>
        <tr>
            <th>ID</th>
            <th>Agent</th>
            <th>Status</th>
            <th>Priority</th>
            <th>Prompt</th>
            <th>Created</th>
            <th>Tool Calls</th>
            <th>Tokens</th>
            <th>Actions</th>
        </tr>
    </thead>
    <tbody id="task-list" hx-get="/tasks?partial=list" hx-trigger="every 3s" hx-swap="innerHTML">
        {% include "partials/task_row.html" %}
    </tbody>
</table>
</figure>

{% if tasks|length == 0 %}
<div class="empty-state" role="status" id="task-empty">
    <p class="empty-state-icon" aria-hidden="true">&#9654;</p>
    <p class="empty-state-text">No tasks found</p>
    <p class="muted">Tasks appear here when agents begin work.</p>
</div>
{% endif %}
{% endblock %}
```

### 2. Update `partials/task_row.html` with cancel button

Open `crates/agentos-web/src/templates/partials/task_row.html`:

```html
{% for task in tasks %}
<tr class="clickable" onclick="if(!event.target.closest('button'))window.location='/tasks/{{ task.id }}'">
    <td><code class="id-short" title="{{ task.id }}">{{ task.id[:8] }}</code></td>
    <td><code class="id-short" title="{{ task.agent_id }}">{{ task.agent_id[:8] }}</code></td>
    <td><span class="badge badge-{{ task.state|lower }}">{{ task.state }}</span></td>
    <td>{{ task.priority }}</td>
    <td class="prompt-cell" title="{{ task.prompt_preview }}">{{ task.prompt_preview[:60] }}{% if task.prompt_preview|length > 60 %}...{% endif %}</td>
    <td><code class="ts">{{ task.created_at }}</code></td>
    <td>{{ task.tool_calls }}</td>
    <td>{{ task.tokens_used }}</td>
    <td>
        {% if task.state == "Running" or task.state == "Pending" or task.state == "Queued" %}
        <button class="outline secondary btn-sm"
                hx-post="/tasks/{{ task.id }}/cancel"
                hx-confirm="Cancel this task?"
                hx-target="closest tr"
                hx-swap="outerHTML"
                onclick="event.stopPropagation()">Cancel</button>
        {% else %}
        <span class="muted">--</span>
        {% endif %}
    </td>
</tr>
{% endfor %}
```

### 3. Update `handlers/tasks.rs` list handler with filter params

Open `crates/agentos-web/src/handlers/tasks.rs`. Update `ListQuery` and the `list()` handler:

```rust
#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
    pub search: Option<String>,
    pub status: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
) -> Response {
    let tasks = state.kernel.scheduler.list_tasks().await;
    let task_rows: Vec<_> = tasks
        .iter()
        .filter(|t| {
            // Filter by status if provided
            if let Some(ref status) = query.status {
                if !status.is_empty() {
                    let state_str = format!("{:?}", t.state).to_lowercase();
                    if !state_str.contains(&status.to_lowercase()) {
                        return false;
                    }
                }
            }
            // Filter by search term in prompt
            if let Some(ref search) = query.search {
                if !search.is_empty() {
                    if !t.prompt_preview.to_lowercase().contains(&search.to_lowercase()) {
                        return false;
                    }
                }
            }
            true
        })
        .map(|t| {
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
        })
        .collect();

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { tasks => task_rows };
        return super::render(&state.templates, "partials/task_row.html", ctx);
    }

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Tasks",
        breadcrumbs => vec![context! { label => "Tasks" }],
        tasks => task_rows,
        csrf_token,
    };
    super::render(&state.templates, "tasks.html", ctx)
}
```

### 4. Add task cancel handler

Add to `crates/agentos-web/src/handlers/tasks.rs`:

```rust
pub async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => {
            return (axum::http::StatusCode::BAD_REQUEST, "Invalid task ID").into_response();
        }
    };

    match state.kernel.api_cancel_task(task_id).await {
        Ok(()) => {
            // Return the updated task row partial for HTMX swap
            if let Some(task) = state.kernel.scheduler.get_task(&task_id).await {
                let task_row = context! {
                    tasks => vec![context! {
                        id => task.id.to_string(),
                        state => format!("{:?}", task.state),
                        agent_id => task.agent_id.to_string(),
                        prompt_preview => task.prompt_preview.clone(),
                        created_at => task.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                        tool_calls => task.tool_calls,
                        tokens_used => task.tokens_used,
                        priority => task.priority,
                    }],
                };
                return super::render(&state.templates, "partials/task_row.html", task_row);
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(msg) => {
            tracing::error!(task = %id, error = %msg, "Failed to cancel task");
            (StatusCode::BAD_REQUEST, "Failed to cancel task").into_response()
        }
    }
}
```

### 5. Add cancel route to `router.rs`

Open `crates/agentos-web/src/router.rs`. Add:

```rust
.route("/tasks/{id}/cancel", axum::routing::post(tasks::cancel))
```

### 6. Enhance `task_detail.html` with cancel button and better metadata

Open `crates/agentos-web/src/templates/task_detail.html`.

Add a cancel button in the page header for active tasks:

```html
<div class="page-header">
    <div>
        <h1>Task <code class="id-long">{{ task_id[:8] }}</code></h1>
        <small class="muted">{{ task_id }}</small>
    </div>
    <div class="page-header-actions">
        <span class="badge badge-{{ state|lower }} badge-lg">{{ state }}</span>
        {% if state == "Running" or state == "Pending" or state == "Queued" %}
        <button class="outline secondary btn-sm"
                hx-post="/tasks/{{ task_id }}/cancel"
                hx-confirm="Cancel this task?"
                hx-swap="none">Cancel Task</button>
        {% endif %}
    </div>
</div>
```

### 7. Add task-specific CSS

Add to `crates/agentos-web/static/css/app.css`:

```css
/* ── Task list ──────────────────────────────────────────── */
.prompt-cell {
    max-width: 300px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.page-header-actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/tasks.html` | Add filter bar with search + status dropdown |
| `crates/agentos-web/src/templates/partials/task_row.html` | Add cancel button, priority column, truncated prompt |
| `crates/agentos-web/src/templates/task_detail.html` | Add cancel button in header for active tasks |
| `crates/agentos-web/src/handlers/tasks.rs` | Add `search`/`status` filter params to `ListQuery` and `list()`; add `cancel()` handler |
| `crates/agentos-web/src/router.rs` | Add `/tasks/{id}/cancel` POST route |
| `crates/agentos-web/static/css/app.css` | Add prompt-cell, page-header-actions styles |

---

## Dependencies

[[01-layout-navigation]] must be complete first (new base.html shell).

---

## Test Plan

- `cargo build -p agentos-web` must compile
- `cargo test -p agentos-web` must pass
- `cargo clippy -p agentos-web -- -D warnings` must pass
- Manual verification:
  - Task list shows filter bar with search input and status dropdown
  - Typing in search box filters tasks by prompt text after 300ms debounce
  - Selecting a status in the dropdown filters to only tasks of that status
  - Cancel button appears only for Running/Pending/Queued tasks
  - Clicking Cancel shows confirmation dialog, then updates the row to show Cancelled status
  - Task detail page shows cancel button in the header for active tasks
  - Priority column renders correctly for all tasks
  - With no tasks, the empty state message appears
  - Clicking a task row (but not the cancel button) navigates to task detail

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
cargo clippy -p agentos-web -- -D warnings
cargo fmt -p agentos-web -- --check
```
