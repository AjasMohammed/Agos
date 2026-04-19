---
title: Phase 07 — CLI Parity (Observability)
tags:
  - webui
  - cli-parity
  - observability
  - phase-07
date: 2026-04-11
status: planned
effort: 1d
priority: medium
---

# Phase 07 — CLI Parity: Observability

> Add web pages for the remaining CLI command groups focused on inspecting AgentOS state: **doctor, scratchpad, snapshot, event, log search, identity, a2a, hal, resource, team**. These are mostly read-only views, so the implementation cost is small once Phase 03 (filters) and Phase 06 (page templates) are in place.

---

## Why this phase

The remaining CLI groups not covered by Phase 06 are observability and introspection — they don't take destructive actions, so they're easier to wire up but still essential for daily operation.

| Group | Why it matters | New page |
|-------|---------------|----------|
| `doctor` | One-click system health check | `/doctor` |
| `scratchpad` | View per-agent markdown notes (with backlink graph) | `/scratchpad` and `/agents/{name}/scratchpad` (Phase 04 tab) |
| `snapshot` | View task snapshots | `/tasks/{id}/snapshots` |
| `event` | Search/filter the event log (different from audit) | `/events` |
| `log` | Ad-hoc log search across components | `/logs` |
| `identity` | View agent Ed25519 public keys + key fingerprints | tab on `/agents/{name}` |
| `a2a` | View A2A messages between agents | `/a2a` |
| `hal` | View HAL drivers and twins | `/hal` |
| `resource` | View resource locks and arbiter state | `/resources` |
| `team` | View multi-agent teams | `/teams` |

## Detailed subtasks

### 1. Doctor page

```rust
// crates/agentos-web/src/handlers/doctor.rs
pub async fn doctor(State(state): State<AppState>) -> Response {
    let checks = run_doctor_checks(&state.kernel).await;
    let ctx = context! {
        page_title => "System Doctor",
        checks => checks.into_iter().map(|c| context! {
            name => c.name,
            status => format!("{:?}", c.status),
            details => c.details,
            fix_command => c.fix_command,
        }).collect::<Vec<_>>(),
    };
    super::render(&state.templates, "doctor.html", ctx)
}

pub async fn doctor_fix(State(state): State<AppState>, Path(check_name): Path<String>) -> Response {
    // Calls the same fix logic as the CLI doctor --fix command
    let result = state.kernel.doctor.fix(&check_name).await;
    // ...
}
```

The CLI already has `agentos doctor [--fix]` with 6 checks (config file, TOML validity, vault/audit dirs, bus socket, tools). The web page calls into the same `run_doctor_checks` function.

### 2. Scratchpad viewer

```html
<!-- crates/agentos-web/src/templates/scratchpad.html -->
{% extends "base.html" %}
{% block content %}
<div class="page-header">
    <h1>Agent Scratchpads</h1>
    <p class="page-meta">{{ pages|length }} pages across {{ agents|length }} agents</p>
</div>
<div class="scratchpad-layout">
    <aside class="scratchpad-sidebar">
        <input type="search" placeholder="Search pages..."
               hx-get="/scratchpad/search"
               hx-trigger="input changed delay:200ms"
               hx-target="#scratchpad-results">
        <div id="scratchpad-results" class="scratchpad-page-list">
            {% for p in pages %}
            <a href="#" hx-get="/scratchpad/{{ p.agent }}/{{ p.slug }}"
               hx-target="#scratchpad-content">
                <strong>{{ p.title }}</strong>
                <small class="muted">{{ p.agent }} · {{ p.updated_at|relative_time }}</small>
            </a>
            {% endfor %}
        </div>
    </aside>
    <main id="scratchpad-content" class="scratchpad-content">
        <p class="muted">Select a page</p>
    </main>
</div>
{% endblock %}
```

`GET /scratchpad/{agent}/{slug}` returns a partial that includes the rendered markdown plus a backlinks list. Wikilinks `[[Title]]` in the markdown are rewritten to `<a href="/scratchpad/{agent}/{slug}">Title</a>` server-side via a small post-processor over the markdown filter output.

### 3. Snapshot viewer (per-task)

Add a "Snapshots" tab to the task detail page:

```html
<nav class="tab-nav">
    <a href="/tasks/{{ task_id }}">Overview</a>
    <a href="/tasks/{{ task_id }}/trace">Trace</a>
    <a href="/tasks/{{ task_id }}/snapshots">Snapshots</a>
</nav>
```

The snapshot list shows `created_at`, `iteration`, `tool_call_count`, with a "Restore" button that calls the kernel's snapshot restore. Restore is gated behind a confirmation dialog.

### 4. Event search

`/events` is similar to `/audit` but for the *event log* (which is a separate concern in AgentOS — events are agent-emitted lifecycle markers, audit is security-relevant). Same table layout with filters.

### 5. Log search

```html
<form hx-get="/logs/search" hx-target="#log-results" hx-trigger="submit, input changed delay:300ms from:#log-query">
    <div class="form-grid-3">
        <input id="log-query" name="q" placeholder="Search logs..." type="search">
        <select name="component">
            <option value="">All components</option>
            <option value="kernel">Kernel</option>
            <option value="bus">Bus</option>
            <option value="web">Web</option>
            <option value="llm">LLM</option>
        </select>
        <select name="level">
            <option value="">All levels</option>
            <option value="error">Error</option>
            <option value="warn">Warn</option>
            <option value="info">Info</option>
            <option value="debug">Debug</option>
        </select>
    </div>
    <div class="form-grid-2">
        <input name="from" type="datetime-local">
        <input name="to" type="datetime-local">
    </div>
</form>
<div id="log-results" class="log-search-results"></div>
```

The handler tails the kernel log files (`/var/lib/agentos/logs/*.log`) and filters in-memory. Caps results at 1,000 lines per query.

### 6. Identity tab on agent detail

Phase 04 created the tab nav. Phase 07 implements the tab content:

```html
<!-- crates/agentos-web/src/templates/partials/agent_identity.html -->
<dl class="detail-list">
    <dt>Agent ID</dt>
    <dd><code>{{ agent.id }}</code></dd>
    <dt>Public Key</dt>
    <dd><code>{{ agent.identity.public_key|truncate_middle(40) }}</code>
        <button class="outline btn-sm" data-clip="{{ agent.identity.public_key }}">Copy</button>
    </dd>
    <dt>Key Fingerprint</dt>
    <dd><code>{{ agent.identity.fingerprint }}</code></dd>
    <dt>Created</dt>
    <dd>{{ agent.identity.created_at|relative_time }}</dd>
</dl>
```

### 7. A2A messages

Read-only timeline of agent-to-agent messages, filterable by sender/receiver agent name. Useful for debugging multi-agent coordination.

### 8. HAL viewer

Lists HAL drivers (system, process, network, GPU, mqtt, mqtt-broker, home-assistant, etc.), their twin state, and last heartbeat. Read-only — actual HAL operations stay CLI-only because they have higher safety implications.

### 9. Resource arbiter

Shows current resource locks (filesystem paths, network sockets, GPU slots) and which agents hold them. Useful for debugging contention.

### 10. Teams page

Lists multi-agent teams (a feature added in the multi-agent coordination work) with members, current task graph, and a link to the orchestrating task.

## Files changed

| Module | New files |
|--------|-----------|
| Doctor | `handlers/doctor.rs`, `templates/doctor.html` |
| Scratchpad | `handlers/scratchpad.rs`, `templates/scratchpad.html`, `templates/partials/scratchpad_page.html` |
| Snapshots | extend `handlers/tasks.rs`, `templates/task_snapshots.html` |
| Events | `handlers/events_log.rs`, `templates/events_log.html` |
| Logs | `handlers/logs.rs`, `templates/logs.html` |
| Identity | `templates/partials/agent_identity.html` |
| A2A | `handlers/a2a.rs`, `templates/a2a.html` |
| HAL | `handlers/hal.rs`, `templates/hal.html` |
| Resources | `handlers/resources.rs`, `templates/resources.html` |
| Teams | `handlers/teams.rs`, `templates/teams.html` |

Plus router additions for each new route, sidebar updates in `base.html` (under "System" and "Capabilities").

## Dependencies

- Requires: [[03-template-rendering-fixes]] for filters
- Soft-requires: [[06-cli-parity-management-pages]] for the shared `partials/management_page.html`
- Soft-requires: [[04-connect-agent-fix]] for the agent detail tab nav

## Test plan

1. **Per-page rendering** — same pattern as Phase 06: seed underlying state, GET the page, assert content visible.
2. **Doctor fix** — break a known check (e.g. delete a config file), GET `/doctor`, assert check shows failed, POST `/doctor/fix/{check}`, assert check now passes.
3. **Scratchpad wikilink** — create a page with `[[Other Page]]`, GET that page, assert anchor has correct href.
4. **Log search** — seed log file with known lines, GET `/logs/search?q=foo`, assert matching lines returned.
5. **Snapshot restore confirmation** — POST `/tasks/{id}/snapshots/{snap_id}/restore`, assert confirm token required.

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web handlers::doctor handlers::scratchpad handlers::logs handlers::a2a handlers::hal handlers::resources handlers::teams
cargo clippy -p agentos-web -- -D warnings
cargo fmt --all -- --check
```

## Related

- [[WebUI Overhaul Plan]]
- [[06-cli-parity-management-pages]]
- [[03-template-rendering-fixes]]
