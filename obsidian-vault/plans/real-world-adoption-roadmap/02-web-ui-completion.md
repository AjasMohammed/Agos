---
title: Web UI Completion
tags:
  - web-ui
  - htmx
  - plan
  - v3
  - adoption
date: 2026-03-25
status: planned
effort: 6d
priority: high
---

# Phase 2 — Web UI Completion

> Complete the AgentOS web interface so that non-CLI users can manage agents, monitor tasks, respond to human-in-the-loop prompts, and review costs — all from a browser. The existing chat UI is the foundation; this phase adds task management, agent management, notification inbox, and cost dashboard.

---

## Why This Phase

The web UI currently works only for chat. Everything else — task monitoring, agent configuration, notification responses, cost review — requires the CLI. This creates two problems:

1. **Non-technical users are completely excluded.** Business users who want to supervise agents or respond to `ask-user` prompts have no interface.
2. **The ask-user tool blocks tasks indefinitely** if the user is not monitoring their CLI. The notification inbox with in-browser response support removes this blocker.

The existing stack (Axum + HTMX + Pico CSS + Alpine.js) is proven. This phase adds pages, not infrastructure.

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Chat UI | Complete | Unchanged |
| Task management | None | Task list, task detail, task trace link, cancel/pause/resume buttons |
| Agent management | None | Agent list, connect form, permission viewer, cost summary per agent |
| Notification inbox | None | Inbox view, ask-user response form, mark-as-read, notification count badge |
| Cost dashboard | None | Total spend by agent, by day, by model; budget status indicators |
| Real-time updates | SSE for chat only | SSE for task status changes, notification badge counts |
| Navigation | Minimal | Sidebar nav with sections: Chat, Tasks, Agents, Notifications, Costs |

---

## Detailed Subtasks

### Subtask 2.1 — Global navigation sidebar

**File:** `crates/agentos-web/src/templates/base.html`

Add a persistent left sidebar replacing the current minimal nav:

```html
<nav class="sidebar">
  <a href="/" class="logo">AgentOS</a>
  <ul>
    <li><a href="/chat" hx-boost="true">Chat</a></li>
    <li>
      <a href="/tasks" hx-boost="true">Tasks</a>
      <span id="task-running-count"
            hx-get="/api/tasks/running/count"
            hx-trigger="every 5s"
            hx-swap="innerHTML"></span>
    </li>
    <li><a href="/agents" hx-boost="true">Agents</a></li>
    <li>
      <a href="/notifications" hx-boost="true">Notifications</a>
      <span id="notif-badge"
            hx-get="/api/notifications/unread/count"
            hx-trigger="every 5s, load"
            hx-swap="innerHTML"></span>
    </li>
    <li><a href="/costs" hx-boost="true">Costs</a></li>
  </ul>
  <div id="kernel-status"
       hx-get="/api/status"
       hx-trigger="every 10s, load"
       hx-swap="innerHTML"></div>
</nav>
```

The `hx-boost="true"` on nav links enables SPA-like navigation without a full page reload.

---

### Subtask 2.2 — Task list page

**File:** `crates/agentos-web/src/templates/tasks/list.html` (new)
**File:** `crates/agentos-web/src/handlers/task_ui.rs` (new)

Task list with live status polling:
```html
<div id="task-list"
     hx-get="/api/tasks"
     hx-trigger="every 3s"
     hx-swap="innerHTML">
  <!-- Each row: task ID, agent, status badge, created, cost, actions -->
  <!-- Status badges: Pending (yellow), Running (blue spinner), Completed (green), Failed (red) -->
  <!-- Actions: View Trace, Cancel, Pause/Resume -->
</div>
```

Add filter controls: by agent, by status, date range.

**API endpoint:** `GET /api/tasks?agent_id=&status=&limit=50`

Returns `Vec<TaskSummary>` as JSON. TaskSummary includes: id, agent_name, status, created_at, finished_at, total_cost_usd, iteration_count.

---

### Subtask 2.3 — Task detail page

**File:** `crates/agentos-web/src/templates/tasks/detail.html` (new)

Show:
- Task metadata (ID, agent, model, created, duration)
- Current status with auto-refresh (SSE or polling)
- Live log stream: subscribe to SSE endpoint `/api/tasks/:id/events` for real-time output
- Link to full trace view (from Phase 1): "View Execution Trace"
- Snapshot list with restore buttons
- Cancel / Pause / Resume action buttons (HTMX form POSTs)

For running tasks, stream tool call results in real time:
```html
<div id="task-log"
     hx-ext="sse"
     sse-connect="/api/tasks/{id}/events"
     sse-swap="beforeend">
  <!-- Tool call results appear here as they stream in -->
</div>
```

**File:** `crates/agentos-web/src/handlers/task_ui.rs`

Add SSE handler that subscribes to the kernel event bus for the given task_id and streams `TaskToolCallCompleted` and `TaskIterationCompleted` events.

---

### Subtask 2.4 — Agent management pages

**File:** `crates/agentos-web/src/templates/agents/list.html` (new)
**File:** `crates/agentos-web/src/templates/agents/detail.html` (new)
**File:** `crates/agentos-web/src/handlers/agent_ui.rs` (new)

Agent list:
- Table: name, ID, connected_at, active tasks, total spend, health status
- "Connect New Agent" button → modal form with: name, model, LLM provider, budget limits

Agent detail:
- Profile info (name, ID, model, created, keypair fingerprint)
- Permission grants table with revoke buttons and "Grant Permission" form
- Active tasks list (links to task detail)
- Cost history chart (last 7 days, daily bars, per-model breakdown)
- Danger zone: Delete Agent button (confirmation dialog)

**API endpoints needed:**
- `GET /api/agents` → agent list
- `GET /api/agents/:id` → agent detail
- `POST /api/agents` → connect new agent
- `DELETE /api/agents/:id` → delete agent
- `GET /api/agents/:id/permissions` → permission list
- `POST /api/agents/:id/permissions` → grant permission
- `DELETE /api/agents/:id/permissions/:perm` → revoke

---

### Subtask 2.5 — Notification inbox with ask-user support

**File:** `crates/agentos-web/src/templates/notifications/inbox.html` (new — skeleton may exist)
**File:** `crates/agentos-web/src/handlers/notifications.rs` (already exists, extend it)

Inbox view:
- Unread count badge in nav (already wired with SSE placeholder in subtask 2.1)
- Message list: sender (agent name), timestamp, preview, type badge (INFO / ASK / ALERT)
- Click to expand: full message body, "Mark as Read" button
- For `ask-user` messages: inline response form with text input and "Send Reply" button
  - Reply POST to `POST /api/notifications/:id/respond`
  - On success: HTMX swaps the row to show "Replied" state and unblocks the waiting task

Real-time delivery: subscribe to SSE endpoint and append new notifications as they arrive:
```html
<div id="inbox"
     hx-ext="sse"
     sse-connect="/api/notifications/stream"
     sse-swap="afterbegin">
</div>
```

**File:** `crates/agentos-web/src/handlers/notifications.rs`

Extend with:
- `GET /notifications` — page route
- `GET /api/notifications` — paginated JSON list
- `GET /api/notifications/stream` — SSE stream of new messages
- `GET /api/notifications/unread/count` — badge count (JSON `{"count": N}`)
- `POST /api/notifications/:id/respond` — respond to ask-user prompt
- `POST /api/notifications/:id/read` — mark as read

---

### Subtask 2.6 — Cost dashboard

**File:** `crates/agentos-web/src/templates/costs/dashboard.html` (new)
**File:** `crates/agentos-web/src/handlers/costs.rs` (new)

Dashboard sections:
1. **Today's spend** — total input tokens, output tokens, cost in USD, task count
2. **Spend by agent** — horizontal bar chart (Alpine.js + inline SVG, no external charting library)
3. **Spend by model** — doughnut breakdown: claude-sonnet-4-6 vs claude-haiku-4-5 vs gpt-4o, etc.
4. **Budget status** — for each agent with a budget: progress bar (green/yellow/red), soft/pause/hard limit markers
5. **7-day trend** — sparkline showing daily cost

Data source: existing `CostTracker` in kernel via new API endpoint `GET /api/costs/summary?days=7&agent_id=`.

**File:** `crates/agentos-web/src/handlers/costs.rs` (new)

```rust
pub async fn get_cost_summary(
    State(state): State<AppState>,
    Query(params): Query<CostSummaryParams>,
) -> Result<Json<CostSummary>>;

pub struct CostSummary {
    pub total_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub by_agent: Vec<AgentCostEntry>,
    pub by_model: Vec<ModelCostEntry>,
    pub daily: Vec<DailyCostEntry>,
    pub budgets: Vec<BudgetStatus>,
}
```

---

### Subtask 2.7 — Kernel command: API data endpoints

Some data for the web UI needs new kernel commands. Add to `crates/agentos-bus/src/message.rs`:

```rust
// New commands
KernelCommand::GetCostSummary { days: u32, agent_id: Option<AgentID> },
KernelCommand::GetRunningTaskCount,
KernelCommand::GetNotificationUnreadCount,

// New responses
KernelResponse::CostSummary(CostSummary),
KernelResponse::RunningTaskCount(u32),
KernelResponse::UnreadCount(u32),
```

Add handlers in `crates/agentos-kernel/src/commands/` for each.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/base.html` | Modified — add sidebar navigation |
| `crates/agentos-web/src/templates/tasks/list.html` | New — task list page |
| `crates/agentos-web/src/templates/tasks/detail.html` | New — task detail + live log |
| `crates/agentos-web/src/templates/agents/list.html` | New — agent list page |
| `crates/agentos-web/src/templates/agents/detail.html` | New — agent detail + permissions |
| `crates/agentos-web/src/templates/notifications/inbox.html` | New — notification inbox |
| `crates/agentos-web/src/templates/costs/dashboard.html` | New — cost dashboard |
| `crates/agentos-web/src/handlers/task_ui.rs` | New — task UI handlers + SSE stream |
| `crates/agentos-web/src/handlers/agent_ui.rs` | New — agent CRUD handlers |
| `crates/agentos-web/src/handlers/notifications.rs` | Modified — add respond, stream, count |
| `crates/agentos-web/src/handlers/costs.rs` | New — cost summary API |
| `crates/agentos-web/src/router.rs` | Modified — add all new routes |
| `crates/agentos-bus/src/message.rs` | Modified — add cost/count commands |
| `crates/agentos-kernel/src/commands/task.rs` | Modified — add running count handler |

---

## Dependencies

- Phase 1 (Task Trace Debugger) — task detail page links to trace view
- User notification system (already complete) — inbox consumes it

---

## Test Plan

1. **Task list polling** — start 3 tasks, open `/tasks`, verify all appear with correct status badges, auto-update when one completes
2. **ask-user response** — start a task that calls `ask-user`, open `/notifications`, verify message appears, submit reply, verify task resumes
3. **Agent connect form** — POST to `/api/agents`, verify agent appears in list and in `agentctl agent list`
4. **Cost dashboard** — run 5 tasks across 2 agents, open `/costs`, verify per-agent bar chart totals match `agentctl cost` output
5. **Budget status bar** — set a budget of $0.10 for an agent, spend $0.07, verify yellow warning state in dashboard

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web

# Start kernel + web server
agentctl web serve &
# Open browser to http://localhost:8080/tasks
# Open browser to http://localhost:8080/agents
# Open browser to http://localhost:8080/notifications
# Open browser to http://localhost:8080/costs
```

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[01-task-trace-debugger]] — trace page linked from task detail
- [[10-visual-pipeline-builder]] — builds on web UI foundation from this phase
