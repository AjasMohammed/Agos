---
title: Phase 06 — CLI Parity (Management Pages)
tags:
  - webui
  - cli-parity
  - plugins
  - channels
  - schedule
  - phase-06
date: 2026-04-11
status: planned
effort: 4d
priority: medium
---

# Phase 06 — CLI Parity: Management Pages

> Add web pages for the CLI command groups that manage agent configuration and integrations: **plugins, channels, schedules, roles, config, escalations, MCP, and webhooks**. Each page follows the same partial-driven template so the implementation cost stays low and the look-and-feel stays consistent.

---

## Why this phase

User-reported: "every thing that can be done using the agentos cli command should be possible in the web ui in a user friendly manner."

Of ~35 CLI command groups, 19 have no web UI. This phase covers the 8 highest-leverage management gaps. Phase 07 covers observability (doctor, scratchpad, snapshot, events).

| Group | Why it matters | New page |
|-------|---------------|----------|
| `plugin` | Plugin manifest discovery & enable/disable is a Day-1 admin task | `/plugins` |
| `channel` | Discord/Slack/etc adapters need pairing codes and health visibility | `/channels` |
| `schedule` | Cron-style task scheduling — currently CLI-only | `/schedules` |
| `role` | Role definitions for RBAC | `/roles` |
| `config` | Edit `config.toml` without dropping to terminal | `/config` |
| `escalation` | Pending tool escalations need an inbox to approve/deny | `/escalations` |
| `mcp` | MCP server discovery and install | `/mcp` |
| `webhooks` | Inbound webhook endpoint configuration | `/webhooks` |

The 8 pages share so much structure that we extract a single base template and per-page handler shells.

## Current → Target State

| Group | CLI commands | Web today | Web target |
|-------|--------------|-----------|------------|
| plugin | list / info / enable / disable | none | List + detail modal + enable/disable buttons |
| channel | list / pair / revoke / health | none | Card grid; per-card pair UI; health badge |
| schedule | list / create / delete / pause / run-now | none | Table + create modal + pause/delete inline |
| role | list / create / delete / show | none | Table + create modal + member count |
| config | get / set / list / reload | none | Two-pane: section list + key/value form per section |
| escalation | list / approve / deny | partial (notification bell) | Dedicated inbox with diff view of input/output |
| mcp | list / discover / install / remove | none | Server cards with discovery/install actions |
| webhooks | list / create / rotate / delete | none | Endpoint table + secret rotation modal |

## Detailed subtasks

### 1. Shared `partials/management_page.html`

```html
<!-- crates/agentos-web/src/templates/partials/management_page.html -->
{# Renders a header + actions toolbar + content area #}
<div class="page-header">
    <div>
        <h1>{{ title }}</h1>
        <p class="page-meta">{{ subtitle }}</p>
    </div>
    <div class="page-header-actions">{% block page_actions %}{% endblock %}</div>
</div>

{% if empty %}
{% include "partials/empty_state.html" %}
{% else %}
{% block table_or_grid %}{% endblock %}
{% endif %}
```

Each new page extends `base.html`, optionally re-uses this fragment, and provides its own table/grid + actions blocks.

### 2. Plugin management

#### Routes
```rust
.route("/plugins", get(plugins::list))
.route("/plugins/{id}", get(plugins::detail))
.route("/plugins/{id}/enable", post(plugins::enable))
.route("/plugins/{id}/disable", post(plugins::disable))
.route("/plugins/discover", post(plugins::discover))
```

#### Handler outline
```rust
// crates/agentos-web/src/handlers/plugins.rs
pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Response {
    let plugins = state.kernel.plugin_registry.list().await;
    let plugin_ctx: Vec<_> = plugins.iter().map(|p| context! {
        id => p.id,
        name => p.name,
        version => p.version,
        trust_tier => format!("{:?}", p.trust_tier),
        status => format!("{:?}", p.status),
        channels => p.channels.clone(),
        tools => p.tools.clone(),
        permissions => p.permissions.clone(),
    }).collect();
    let csrf = csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Plugins",
        breadcrumbs => vec![context! { label => "Plugins" }],
        plugins => plugin_ctx,
        csrf_token => csrf,
    };
    super::render(&state.templates, "plugins.html", ctx)
}
```

#### Template
```html
<!-- crates/agentos-web/src/templates/plugins.html -->
{% extends "base.html" %}
{% block content %}
<div class="page-header">
    <div><h1>Plugins</h1><p class="page-meta">{{ plugins|length }} discovered</p></div>
    <div class="page-header-actions">
        <button hx-post="/plugins/discover" hx-target="#plugins-table" hx-swap="outerHTML">
            Re-scan plugin directories
        </button>
    </div>
</div>

<table id="plugins-table">
    <thead>
        <tr><th>Name</th><th>Version</th><th>Trust</th><th>Status</th><th>Channels</th><th>Tools</th><th></th></tr>
    </thead>
    <tbody>
        {% for p in plugins %}
        <tr data-plugin-id="{{ p.id }}">
            <td><strong>{{ p.name }}</strong><br><small class="muted">{{ p.id }}</small></td>
            <td>{{ p.version }}</td>
            <td><span class="badge badge-trust-{{ p.trust_tier|lower }}">{{ p.trust_tier }}</span></td>
            <td><span class="badge badge-status-{{ p.status|lower }}">{{ p.status }}</span></td>
            <td>{{ p.channels|length }}</td>
            <td>{{ p.tools|length }}</td>
            <td>
                {% if p.status == "Active" %}
                <button class="outline secondary btn-sm" hx-post="/plugins/{{ p.id }}/disable"
                        hx-target="closest tr" hx-swap="outerHTML">Disable</button>
                {% elif p.status == "Disabled" %}
                <button class="outline btn-sm" hx-post="/plugins/{{ p.id }}/enable"
                        hx-target="closest tr" hx-swap="outerHTML">Enable</button>
                {% endif %}
                <a href="/plugins/{{ p.id }}" class="outline btn-sm" role="button">Info</a>
            </td>
        </tr>
        {% endfor %}
    </tbody>
</table>
{% endblock %}
```

### 3. Channel management

Same pattern. Channels have additional pairing flow:

#### Pairing modal
```html
<dialog x-data="{ open: false, code: '' }" :open="open">
    <article>
        <header><h3>Pair Channel</h3></header>
        <p>Send <code>/pair</code> to {{ channel.name }} from your account, then enter the 6-character code below.</p>
        <form hx-post="/channels/{{ channel.id }}/pair">
            <input type="hidden" name="_csrf" value="{{ csrf_token }}">
            <label>Pairing Code
                <input type="text" name="code" maxlength="6" pattern="[A-Z0-9]{6}" required>
            </label>
            <button type="submit">Confirm Pairing</button>
        </form>
    </article>
</dialog>
```

The handler calls `state.kernel.channel_manager.confirm_pairing(channel_id, code)`.

### 4. Schedule management

The CLI `agentos schedule create --cron "0 9 * * *" --agent mavrick --prompt "daily standup"` becomes:

```html
<form hx-post="/schedules" hx-target="#schedules-table" hx-swap="outerHTML">
    <input type="hidden" name="_csrf" value="{{ csrf_token }}">
    <div class="form-grid-2">
        <label>Name <input name="name" required></label>
        <label>Agent
            <select name="agent_name">
                {% for a in agents %}<option>{{ a.name }}</option>{% endfor %}
            </select>
        </label>
    </div>
    <label>Cron Expression
        <input name="cron" placeholder="0 9 * * *" required>
        <small class="muted" id="cron-preview">Next run: —</small>
    </label>
    <label>Prompt
        <textarea name="prompt" rows="4" required></textarea>
    </label>
    <button type="submit">Create Schedule</button>
</form>
```

Add a small JS file `schedule-preview.js` that calls a new endpoint `POST /api/schedules/preview` with the cron expression and updates `#cron-preview` with the next 3 firing times. The endpoint uses the `cron` crate (already in workspace).

### 5. Role management

Simple table + create modal. The CLI commands map directly:

| CLI | HTTP |
|-----|------|
| `role list` | `GET /roles` |
| `role create <name> --perms ...` | `POST /roles` |
| `role show <name>` | `GET /roles/{name}` |
| `role delete <name>` | `DELETE /roles/{name}` |

Permissions are entered as a textarea (one per line) in the create modal. Server validates and converts to a `PermissionSet`.

### 6. Config editor

Two-pane layout: left = section list (kernel, llm, vault, audit, tools, scratchpad), right = key/value form for the selected section.

```html
<div class="config-layout">
    <aside class="config-sections">
        {% for section in sections %}
        <a href="#" hx-get="/config/{{ section }}" hx-target="#config-pane">{{ section }}</a>
        {% endfor %}
    </aside>
    <main id="config-pane">
        <p class="muted">Select a section</p>
    </main>
</div>
```

`GET /config/{section}` returns a fragment with one `<input>` per key. Save is `POST /config/{section}/{key}` with the new value. The handler calls `kernel.config_manager.set(section, key, value)`. The `ConfigWatcher` (already implemented in the OpenClaw improvements) detects the change and applies it.

**Safety:** Sensitive keys (vault key derivation, audit signing key) are read-only with a lock icon. Modifying them requires editing the file directly. The handler enforces a `READ_ONLY_KEYS` set.

### 7. Escalation inbox

The CLI `agentos escalation list` becomes a dashboard at `/escalations`:

```html
{% for esc in escalations %}
<article class="escalation-card">
    <header>
        <strong>{{ esc.tool_name }}</strong>
        <span class="badge badge-risk-{{ esc.risk_class|lower }}">{{ esc.risk_class }}</span>
        <small class="muted">expires {{ esc.expires_at|relative_time }}</small>
    </header>
    <div class="escalation-input">
        <h4>Tool input</h4>
        <pre><code class="language-json">{{ esc.input_json|pretty_json }}</code></pre>
    </div>
    <div class="escalation-context">
        <h4>Why agent wants this</h4>
        <p>{{ esc.justification }}</p>
    </div>
    <div class="escalation-actions">
        <button hx-post="/escalations/{{ esc.id }}/approve" hx-target="closest article">Approve</button>
        <button class="outline secondary" hx-post="/escalations/{{ esc.id }}/deny" hx-target="closest article">Deny</button>
    </div>
</article>
{% endfor %}
```

The notification bell in the topbar already shows pending escalation counts (Phase 02 of UNIS). This page is the full-screen detail view.

### 8. MCP server management

```html
<div class="grid">
    {% for srv in mcp_servers %}
    <article class="mcp-card">
        <header>
            <strong>{{ srv.name }}</strong>
            <span class="badge badge-status-{{ srv.status|lower }}">{{ srv.status }}</span>
        </header>
        <p class="muted">{{ srv.transport }} · {{ srv.command|truncate_middle(40) }}</p>
        <p>{{ srv.tools|length }} tools, {{ srv.resources|length }} resources</p>
        <footer>
            <button class="outline btn-sm" hx-get="/mcp/{{ srv.name }}/tools"
                    hx-target="#mcp-tools-modal">View tools</button>
            <button class="outline secondary btn-sm" hx-delete="/mcp/{{ srv.name }}"
                    hx-target="closest article" hx-swap="outerHTML"
                    hx-confirm="Remove MCP server {{ srv.name }}?">Remove</button>
        </footer>
    </article>
    {% endfor %}
</div>
```

Discover button POSTs to `/mcp/discover` which scans the configured discovery directories and adds new servers to the registry.

### 9. Webhooks

```html
<table>
    <thead><tr><th>Endpoint ID</th><th>Description</th><th>Created</th><th>Calls (24h)</th><th></th></tr></thead>
    <tbody>
        {% for hook in webhooks %}
        <tr>
            <td><code>{{ hook.endpoint_id|truncate_middle(20) }}</code></td>
            <td>{{ hook.description }}</td>
            <td>{{ hook.created_at|relative_time }}</td>
            <td>{{ hook.call_count_24h }}</td>
            <td>
                <button class="outline btn-sm" hx-post="/webhooks/{{ hook.endpoint_id }}/rotate"
                        hx-target="closest tr" hx-swap="outerHTML">Rotate Secret</button>
                <button class="outline secondary btn-sm" hx-delete="/webhooks/{{ hook.endpoint_id }}"
                        hx-target="closest tr" hx-swap="outerHTML"
                        hx-confirm="Delete this webhook?">Delete</button>
            </td>
        </tr>
        {% endfor %}
    </tbody>
</table>
```

The full URL (`/api/v1/webhooks/incoming/{endpoint_id}`) is shown in a copy-on-click code box so users can paste it into Stripe/Linear/etc.

### 10. Sidebar navigation update

Add the new pages to the sidebar in `base.html`. Group them under collapsible sections to avoid overflow:

```html
<details class="nav-section" open>
    <summary>Operations</summary>
    <ul>
        <li><a href="/">Dashboard</a></li>
        <li><a href="/chat">Chat</a></li>
        <li><a href="/agents">Agents</a></li>
        <li><a href="/tasks">Tasks</a></li>
        <li><a href="/escalations">Escalations</a></li>
    </ul>
</details>
<details class="nav-section">
    <summary>Capabilities</summary>
    <ul>
        <li><a href="/tools">Tools</a></li>
        <li><a href="/skills">Skills</a></li>
        <li><a href="/pipelines">Pipelines</a></li>
        <li><a href="/schedules">Schedules</a></li>
        <li><a href="/roles">Roles</a></li>
    </ul>
</details>
<details class="nav-section">
    <summary>Integrations</summary>
    <ul>
        <li><a href="/plugins">Plugins</a></li>
        <li><a href="/channels">Channels</a></li>
        <li><a href="/mcp">MCP Servers</a></li>
        <li><a href="/webhooks">Webhooks</a></li>
        <li><a href="/connectors">OAuth Connectors</a></li>
    </ul>
</details>
<details class="nav-section">
    <summary>System</summary>
    <ul>
        <li><a href="/audit">Audit Log</a></li>
        <li><a href="/cost">Cost</a></li>
        <li><a href="/secrets">Secrets</a></li>
        <li><a href="/config">Config</a></li>
    </ul>
</details>
```

## Files changed

(Truncated — full list per page module)

| Module | New files |
|--------|-----------|
| Plugins | `handlers/plugins.rs`, `templates/plugins.html`, `templates/plugin_detail.html` |
| Channels | `handlers/channels.rs`, `templates/channels.html` |
| Schedules | `handlers/schedules.rs`, `templates/schedules.html`, `static/js/schedule-preview.js` |
| Roles | `handlers/roles.rs`, `templates/roles.html` |
| Config | `handlers/config.rs`, `templates/config.html`, `templates/partials/config_section.html` |
| Escalations | `handlers/escalations.rs`, `templates/escalations.html` |
| MCP | `handlers/mcp.rs`, `templates/mcp.html` |
| Webhooks | `handlers/webhooks_admin.rs` (the existing `webhooks.rs` is the inbound dispatcher), `templates/webhooks_admin.html` |
| Shared | `templates/partials/management_page.html` |

Plus new routes in `router.rs` and the sidebar update in `base.html`.

## Dependencies

- Requires: [[03-template-rendering-fixes]] for `pretty_json`, `relative_time`, `truncate_middle`, `humanize_event_type`
- Requires: [[01-chat-streaming-engine]] (no — independent)
- Independent of: 02, 04, 05
- Soft-soft: extends [[WebUI Redesign Plan]] sidebar

## Test plan

1. **Per-page list rendering**
   - For each new page, write an integration test that seeds the underlying registry/store with 2–3 items and asserts the rendered HTML contains them.
2. **Plugin enable/disable round-trip**
   - Discover plugins, disable one via POST, GET `/plugins`, assert the row shows "Disabled" status. POST enable, assert "Active".
3. **Schedule create**
   - POST `/schedules` with cron `*/5 * * * *`. Assert created. GET, assert visible. Wait 5 min in test (or use mocked clock). Assert task was created.
4. **Config edit**
   - Read current `kernel.max_concurrent_tasks`. POST a new value. Assert config file was written. Assert `ConfigWatcher` reloaded.
5. **Escalation approve**
   - Trigger an escalation via mock tool. GET `/escalations`, assert visible. POST approve. Assert tool execution proceeds.
6. **Manual: full sidebar tour**
   - Click every sidebar entry. Each page must load without 500/404 and must display either content or an empty state.

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web handlers::plugins handlers::channels handlers::schedules handlers::roles handlers::config handlers::escalations handlers::mcp handlers::webhooks_admin
cargo clippy -p agentos-web -- -D warnings
cargo fmt --all -- --check
```

## Related

- [[WebUI Overhaul Plan]]
- [[03-template-rendering-fixes]]
- [[07-cli-parity-observability]]
