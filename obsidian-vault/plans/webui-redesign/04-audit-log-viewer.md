---
title: "Phase 04: Audit Log Viewer Enhancement"
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

# Phase 04: Audit Log Viewer Enhancement

> Add severity filtering, date range selection, export capability, and improved event detail rendering to the audit log viewer.

---

## Why This Phase

The audit log is the observability backbone of AgentOS. Currently it supports only event type text search and a row limit selector. Operators need:

- **Severity filtering** -- quickly find Critical/Security events during incident response
- **Multi-filter combination** -- filter by event type AND severity simultaneously
- **Better detail rendering** -- JSON details should be syntax-highlighted and collapsible
- **Row count display** -- show "showing X of Y total" to indicate current view scope
- **Auto-scroll toggle for live mode** -- when new events come in, auto-scroll to latest
- **Clear filters button** -- one click to reset all filters

---

## Current to Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Event type filter | Text input with HTMX partial swap | Same, keep as-is |
| Severity filter | None | Dropdown: All / Info / Warning / Security / Critical |
| Row limit | Select: 50/100/500/1000 | Same, keep as-is |
| Filter status | No indication | "Showing X of Y" count below filter bar |
| Clear filters | Not available | "Clear" button that resets all filters |
| Detail rendering | Raw text or `<details>` for long strings | JSON detection with `<pre>` formatting and copy button |
| Auto-refresh | Polling every 10s | Polling every 10s (SSE upgrade deferred to Phase 05) |
| Export | Not available | "Export CSV" button that downloads current filtered view |

---

## Detailed Subtasks

### 1. Add severity filter to `audit.html`

Open `crates/agentos-web/src/templates/audit.html`. Update the filter bar:

```html
{% extends "base.html" %}
{% block content %}
<div class="page-header">
    <h1>Audit Log</h1>
    <span class="page-meta">{{ total_count }} total entries</span>
</div>

<div class="filter-bar">
    <input type="text" name="event_type" placeholder="Filter by event type..."
           hx-get="/audit" hx-target="#audit-body" hx-swap="innerHTML"
           hx-trigger="keyup changed delay:300ms"
           hx-include="[name='event_type'],[name='severity'],[name='limit']"
           hx-vals='{"partial": "list"}'>
    <select name="severity"
            hx-get="/audit" hx-target="#audit-body" hx-swap="innerHTML"
            hx-trigger="change"
            hx-include="[name='event_type'],[name='severity'],[name='limit']"
            hx-vals='{"partial": "list"}'>
        <option value="">All Severities</option>
        <option value="info">Info</option>
        <option value="warning">Warning</option>
        <option value="security">Security</option>
        <option value="critical">Critical</option>
    </select>
    <select name="limit"
            hx-get="/audit" hx-target="#audit-body" hx-swap="innerHTML"
            hx-trigger="change"
            hx-include="[name='event_type'],[name='severity'],[name='limit']"
            hx-vals='{"partial": "list"}'>
        <option value="50">50 rows</option>
        <option value="100">100 rows</option>
        <option value="500">500 rows</option>
        <option value="1000">1000 rows</option>
    </select>
    <button class="outline secondary btn-sm"
            hx-get="/audit?partial=list&limit=50"
            hx-target="#audit-body"
            hx-swap="innerHTML">Refresh</button>
    <button class="outline btn-sm" type="button"
            onclick="clearAuditFilters()">Clear</button>
</div>

<p id="audit-showing" class="muted" style="font-size: 0.85rem; margin-bottom: 0.5rem;">
    Showing {{ entries|length }} of {{ total_count }} entries
</p>

<figure>
<table class="audit-table">
    <thead>
        <tr>
            <th>Timestamp</th>
            <th>Event</th>
            <th>Severity</th>
            <th>Agent</th>
            <th>Task</th>
            <th>Tool</th>
            <th>Details</th>
        </tr>
    </thead>
    <tbody id="audit-body" hx-get="/audit?partial=list" hx-trigger="every 10s" hx-swap="innerHTML">
        {% include "partials/log_line.html" %}
    </tbody>
</table>
</figure>

<script>
function clearAuditFilters() {
    document.querySelector('[name="event_type"]').value = '';
    document.querySelector('[name="severity"]').value = '';
    document.querySelector('[name="limit"]').value = '50';
    htmx.ajax('GET', '/audit?partial=list&limit=50', {target: '#audit-body', swap: 'innerHTML'});
}
</script>
{% endblock %}
```

### 2. Update severity filtering in `handlers/audit.rs`

Open `crates/agentos-web/src/handlers/audit.rs`. Add `severity` to `ListQuery`:

```rust
#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
    pub limit: Option<u32>,
    pub event_type: Option<String>,
    pub severity: Option<String>,
}
```

Update the filter logic in `list()`:

```rust
let rows: Vec<_> = entries
    .iter()
    .filter(|e| {
        // Event type filter
        if let Some(ref et) = query.event_type {
            if !et.is_empty() {
                if !format!("{:?}", e.event_type)
                    .to_lowercase()
                    .contains(&et.to_lowercase()) {
                    return false;
                }
            }
        }
        // Severity filter
        if let Some(ref sev) = query.severity {
            if !sev.is_empty() {
                let severity_str = format!("{:?}", e.severity).to_lowercase();
                if severity_str != sev.to_lowercase() {
                    return false;
                }
            }
        }
        true
    })
    .map(|e| {
        // ... existing mapping code ...
    })
    .collect();
```

Also pass `breadcrumbs` to the template context:

```rust
let ctx = context! {
    page_title => "Audit Log",
    breadcrumbs => vec![context! { label => "Audit Log" }],
    entries => rows,
    total_count,
    csrf_token,
};
```

### 3. Improve detail rendering in `partials/log_line.html`

Open `crates/agentos-web/src/templates/partials/log_line.html`. Improve JSON detection:

```html
{% for entry in entries %}
<tr>
    <td><code class="ts">{{ entry.timestamp }}</code></td>
    <td><code class="event-type">{{ entry.event_type }}</code></td>
    <td><span class="badge badge-{{ entry.severity|lower }}">{{ entry.severity }}</span></td>
    <td>{% if entry.agent_id %}<code class="id-short" title="{{ entry.agent_id }}">{{ entry.agent_id[:8] }}</code>{% else %}<span class="muted">--</span>{% endif %}</td>
    <td>{% if entry.task_id %}<a href="/tasks/{{ entry.task_id }}" class="id-link" title="{{ entry.task_id }}"><code>{{ entry.task_id[:8] }}</code></a>{% else %}<span class="muted">--</span>{% endif %}</td>
    <td>{% if entry.tool_id %}<code class="id-short" title="{{ entry.tool_id }}">{{ entry.tool_id[:8] }}</code>{% else %}<span class="muted">--</span>{% endif %}</td>
    <td class="details-cell">
        {% if entry.details|length > 120 %}
        <details>
            <summary><small class="details-preview">{{ entry.details[:120] }}...</small></summary>
            <pre class="details-full">{{ entry.details }}</pre>
        </details>
        {% elif entry.details|length > 0 %}
        <small>{{ entry.details }}</small>
        {% else %}
        <span class="muted">--</span>
        {% endif %}
    </td>
</tr>
{% endfor %}
```

### 4. Add severity filter badge colors to `app.css`

The existing `app.css` already has `.badge-critical`, `.badge-warning`, `.badge-info`, and `.badge-security` is missing. Add:

```css
.badge-security { background-color: #6f42c1; color: #fff; }
```

This ensures the severity badges in the audit log render with the correct color for the "Security" severity level.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/audit.html` | Add severity dropdown, clear button, showing count |
| `crates/agentos-web/src/templates/partials/log_line.html` | Improve empty detail rendering |
| `crates/agentos-web/src/handlers/audit.rs` | Add `severity` to `ListQuery`, filter logic, breadcrumbs |
| `crates/agentos-web/static/css/app.css` | Add `.badge-security` style |

---

## Dependencies

[[01-layout-navigation]] must be complete first (new base.html shell with breadcrumbs).

---

## Test Plan

- `cargo build -p agentos-web` must compile
- `cargo test -p agentos-web` must pass
- `cargo clippy -p agentos-web -- -D warnings` must pass
- Manual verification:
  - Audit page shows severity dropdown with options: All, Info, Warning, Security, Critical
  - Selecting "Critical" filters to only Critical severity entries
  - Combining event type search + severity filter works correctly (both filters applied)
  - "Showing X of Y" count updates after filtering
  - "Clear" button resets all filters and reloads with defaults
  - Security severity badge renders with purple background
  - Long JSON details still expand correctly in `<details>` elements
  - 10-second auto-refresh continues to work with filters applied

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
cargo clippy -p agentos-web -- -D warnings
cargo fmt -p agentos-web -- --check
```
