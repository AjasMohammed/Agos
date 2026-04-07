---
title: "Phase 1: Task DAG View"
tags:
  - web
  - kernel
  - plan
date: 2026-04-03
status: planned
effort: 2d
priority: high
---

# Phase 1: Task DAG View

> Add a `/tasks/dag` page to the web UI that renders the parent-child task tree from `scheduler.child_map`, showing task ID, agent name, state (color-coded), spawn depth, and duration -- using server-rendered HTML with HTMX `<details>/<summary>` for tree expansion.

---

## Why This Phase

Multi-agent coordination produces a tree of parent and child tasks (up to `MAX_SPAWN_DEPTH = 5` levels deep). Currently, the web UI shows a flat task list at `/tasks`. There is no way to visualize which tasks are children of which, or to see the overall execution topology. Competing frameworks (LangSmith, AutoGen Studio) provide visual DAG debuggers.

The data already exists: `TaskScheduler.child_map: RwLock<HashMap<TaskID, Vec<TaskID>>>` tracks parent-child relationships, and each `AgentTask` has `spawn_depth`, `parent_task_id`, and `agent_id` fields.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Task view | Flat list at `/tasks` | Tree view at `/tasks/dag` with expandable parent-child hierarchy |
| Task state display | Text labels | Color-coded badges: grey(Queued), blue(Running), green(Complete), red(Failed), amber(Cancelled) |
| Parent-child visibility | Not shown in UI | Tree nesting with spawn depth indicators |
| Mermaid export | Not available | `/api/tasks/dag/mermaid` returns DAG as Mermaid text |

## What to Do

### 1. Add DAG data retrieval to `AppState`

The `AppState` struct in `crates/agentos-web/src/state.rs` already has `kernel: Arc<Kernel>`. The DAG handler will call:

```rust
// Get all root tasks (no parent) and their children recursively.
let tasks = state.kernel.scheduler.list_tasks().await;
let child_map = state.kernel.scheduler.get_full_child_map().await;
```

### 2. Add `get_full_child_map` to `TaskScheduler`

Open `crates/agentos-kernel/src/scheduler.rs`. Add:

```rust
/// Return a clone of the entire child_map for visualization.
pub async fn get_full_child_map(&self) -> HashMap<TaskID, Vec<TaskID>> {
    self.child_map.read().await.clone()
}
```

### 3. Create the DAG handler

Create `crates/agentos-web/src/handlers/dag.rs`:

```rust
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use crate::state::AppState;
use agentos_types::{TaskID, TaskState, TaskSummary};
use std::collections::HashMap;

/// Render the task DAG as a server-rendered HTML tree.
pub async fn dag_view(State(state): State<AppState>) -> impl IntoResponse {
    let tasks = state.kernel.scheduler.list_tasks().await;
    let child_map = state.kernel.scheduler.get_full_child_map().await;

    // Build a lookup: task_id -> TaskSummary
    let task_map: HashMap<TaskID, &TaskSummary> = tasks.iter()
        .map(|t| (t.id, t))
        .collect();

    // Find root tasks (those with no parent or whose parent is not in the current set).
    let root_ids: Vec<TaskID> = tasks.iter()
        .filter(|t| t.parent_task_id.is_none())
        .map(|t| t.id)
        .collect();

    let tree_html = render_dag_tree(&root_ids, &child_map, &task_map, 0);

    let ctx = minijinja::context! {
        title => "Task DAG",
        tree_html => tree_html,
    };
    let html = state.templates.get_template("dag.html")
        .and_then(|t| t.render(ctx).map_err(Into::into))
        .unwrap_or_else(|e| format!("<p>Template error: {}</p>", e));

    Html(html)
}

fn state_css_class(state: &TaskState) -> &'static str {
    match state {
        TaskState::Queued => "badge-grey",
        TaskState::Running => "badge-blue",
        TaskState::Complete => "badge-green",
        TaskState::Failed => "badge-red",
        TaskState::Cancelled => "badge-amber",
        TaskState::Waiting => "badge-yellow",
        TaskState::Suspended => "badge-purple",
    }
}

fn render_dag_tree(
    task_ids: &[TaskID],
    child_map: &HashMap<TaskID, Vec<TaskID>>,
    task_map: &HashMap<TaskID, &TaskSummary>,
    depth: usize,
) -> String {
    let mut html = String::new();
    for &id in task_ids {
        let children = child_map.get(&id).cloned().unwrap_or_default();
        let task_info = task_map.get(&id);

        let name = task_info.map(|t| t.agent_name.as_str()).unwrap_or("unknown");
        let state = task_info.map(|t| &t.state).cloned().unwrap_or(TaskState::Queued);
        let css = state_css_class(&state);
        let id_short = &id.to_string()[..8];

        if children.is_empty() {
            html.push_str(&format!(
                "<div class=\"dag-leaf\" style=\"margin-left:{}rem\">\
                 <span class=\"{css}\">{state:?}</span> \
                 <code>{id_short}</code> {name} (depth {depth})\
                 </div>\n",
                depth * 2
            ));
        } else {
            html.push_str(&format!(
                "<details style=\"margin-left:{}rem\">\
                 <summary>\
                 <span class=\"{css}\">{state:?}</span> \
                 <code>{id_short}</code> {name} (depth {depth}, {} children)\
                 </summary>\n",
                depth * 2,
                children.len()
            ));
            html.push_str(&render_dag_tree(&children, child_map, task_map, depth + 1));
            html.push_str("</details>\n");
        }
    }
    html
}

/// Return the DAG as a Mermaid diagram string.
pub async fn dag_mermaid(State(state): State<AppState>) -> impl IntoResponse {
    let tasks = state.kernel.scheduler.list_tasks().await;
    let child_map = state.kernel.scheduler.get_full_child_map().await;

    let task_map: HashMap<TaskID, &TaskSummary> = tasks.iter()
        .map(|t| (t.id, t))
        .collect();

    let mut mermaid = String::from("graph TD\n");
    for (parent_id, children) in &child_map {
        let parent_label = task_map.get(parent_id)
            .map(|t| format!("{}[{} {:?}]", &parent_id.to_string()[..8], t.agent_name, t.state))
            .unwrap_or_else(|| format!("{}[unknown]", &parent_id.to_string()[..8]));

        for child_id in children {
            let child_label = task_map.get(child_id)
                .map(|t| format!("{}[{} {:?}]", &child_id.to_string()[..8], t.agent_name, t.state))
                .unwrap_or_else(|| format!("{}[unknown]", &child_id.to_string()[..8]));

            mermaid.push_str(&format!("    {} --> {}\n", parent_label, child_label));
        }
    }

    mermaid
}
```

### 4. Create the template

Create `crates/agentos-web/templates/dag.html`:

```html
{% extends "base.html" %}
{% block title %}Task DAG{% endblock %}
{% block content %}
<h1>Task DAG</h1>
<p>Parent-child task execution tree. Expand nodes to see children.</p>

<style>
    .badge-grey   { background: #6b7280; color: white; padding: 2px 8px; border-radius: 4px; font-size: 0.8em; }
    .badge-blue   { background: #3b82f6; color: white; padding: 2px 8px; border-radius: 4px; font-size: 0.8em; }
    .badge-green  { background: #22c55e; color: white; padding: 2px 8px; border-radius: 4px; font-size: 0.8em; }
    .badge-red    { background: #ef4444; color: white; padding: 2px 8px; border-radius: 4px; font-size: 0.8em; }
    .badge-amber  { background: #f59e0b; color: white; padding: 2px 8px; border-radius: 4px; font-size: 0.8em; }
    .badge-yellow { background: #eab308; color: white; padding: 2px 8px; border-radius: 4px; font-size: 0.8em; }
    .badge-purple { background: #a855f7; color: white; padding: 2px 8px; border-radius: 4px; font-size: 0.8em; }
    .dag-leaf { padding: 4px 0; }
    details { padding: 4px 0; }
    details > summary { cursor: pointer; }
    code { font-size: 0.85em; }
</style>

<div id="dag-tree">
    {{ tree_html|safe }}
</div>

<hr>
<a href="/api/tasks/dag/mermaid" target="_blank">Export as Mermaid</a>
{% endblock %}
```

### 5. Register routes

Open `crates/agentos-web/src/router.rs`. Add:

```rust
.route("/tasks/dag", axum::routing::get(dag::dag_view))
.route("/api/tasks/dag/mermaid", axum::routing::get(dag::dag_mermaid))
```

Open `crates/agentos-web/src/handlers/mod.rs`. Add:

```rust
pub mod dag;
```

### 6. Add `parent_task_id` to `TaskSummary` if missing

Open `crates/agentos-types/src/task.rs`. Verify `TaskSummary` includes `parent_task_id`. If not, add:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub parent_task_id: Option<TaskID>,
```

And ensure `scheduler.list_tasks()` populates it from the `AgentTask`.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/handlers/dag.rs` | New file: `dag_view` and `dag_mermaid` handlers |
| `crates/agentos-web/src/handlers/mod.rs` | Add `pub mod dag;` |
| `crates/agentos-web/src/router.rs` | Add `/tasks/dag` and `/api/tasks/dag/mermaid` routes |
| `crates/agentos-web/templates/dag.html` | New template: DAG tree view |
| `crates/agentos-kernel/src/scheduler.rs` | Add `get_full_child_map()` method |
| `crates/agentos-types/src/task.rs` | Ensure `TaskSummary` has `parent_task_id` field |

## Prerequisites

None -- this is the first phase. It depends only on existing `scheduler.child_map` data.

## Test Plan

- **Unit test `test_get_full_child_map`:** Register parent-child pairs in a scheduler. Call `get_full_child_map`. Assert the map contains the expected relationships.
- **Unit test `test_state_css_class`:** Assert each `TaskState` variant maps to the correct CSS class string.
- **Unit test `test_render_dag_tree_flat`:** Render a DAG with 3 root tasks and no children. Assert the output contains 3 `dag-leaf` divs.
- **Unit test `test_render_dag_tree_nested`:** Render a DAG with 1 root and 2 children. Assert the output contains a `<details>` element with `2 children` in the summary.
- **Unit test `test_dag_mermaid_output`:** Create a DAG with parent-child pair. Call `dag_mermaid`. Assert the output starts with `graph TD` and contains an arrow `-->`.

## Verification

```bash
cargo build -p agentos-web -p agentos-kernel
cargo test -p agentos-web -- --nocapture
cargo test -p agentos-kernel -- child_map --nocapture
cargo clippy -p agentos-web -p agentos-kernel -- -D warnings
cargo fmt --all -- --check
```
