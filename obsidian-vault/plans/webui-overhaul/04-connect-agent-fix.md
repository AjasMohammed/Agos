---
title: Phase 04 — Connect Agent Fix
tags:
  - webui
  - agents
  - htmx
  - phase-04
date: 2026-04-11
status: planned
effort: 1d
priority: high
---

# Phase 04 — Connect Agent Fix & Agents Page Polish

> Fix the broken Connect Agent flow (the form currently swaps the entire HTML page into `#agent-grid`), surface form-level error messages, expand the form to support `base_url`, `roles`, `system_prompt`, and `thinking_level`, and add a per-agent detail page tab set covering memory, scratchpad, identity, and audit history.

---

## Why this phase

User-reported: "the connect agent feature in the ui doesn't work."

Root cause analysis ([crates/agentos-web/src/handlers/agents.rs:84](crates/agentos-web/src/handlers/agents.rs#L84)):

```rust
match state.service.connect_agent(req).await {
    Ok(_) => {
        let mut response = axum::response::Redirect::to("/agents").into_response();
        // ...HX-Trigger toast...
        response
    }
    ...
}
```

`Redirect::to("/agents")` returns a `303 See Other` with `Location: /agents`. The form is configured with `hx-post="/agents" hx-target="#agent-grid" hx-swap="innerHTML"`, so HTMX follows the 303 via fetch, gets the **full HTML page** (with `<!doctype html><html>...sidebar...main...</html>`), and dumps that whole document inside `#agent-grid`. Browsers handle nested `<html>` differently — sometimes the page goes blank, sometimes the grid becomes a recursive nest.

Additionally:

- The form's `@htmx:after-request="showModal = false"` fires unconditionally, so the modal closes even on a 4xx error and the user has no idea their submit failed.
- The form is missing `base_url` (so users cannot connect to self-hosted Ollama at a custom IP), `roles`, `system_prompt`, and `thinking_level` — all of which the CLI supports.
- The grid uses 5-second SSE polling which is fine, but doesn't surface "agent currently busy" / "agent failing health checks" status.

## Current → Target State

| Concern | Current | Target |
|---------|---------|--------|
| Connect handler response | 303 redirect to `/agents` | Returns HTML fragment (refreshed grid) on 200; returns inline error fragment on 4xx |
| Modal close on success | Yes | Yes (via `HX-Trigger: closeAgentModal`) |
| Modal close on failure | Yes (BUG) | No — modal stays open with error visible |
| Form fields | name, provider, model, description | + base_url, roles, system_prompt, thinking_level |
| Provider list | Hardcoded 4 options | Loaded from `state.service.list_providers()` (uses `ProviderCatalog`) |
| Server-side validation errors | Hidden (only logged) | Echoed back as small text inside `#connect-form-error` |
| Agent detail page | Single dl + permissions table | Tabbed layout: Overview / Permissions / Memory / Scratchpad / Identity / Audit |
| Agent status indicator | Online/offline | Online + "busy with N tasks" + "last heartbeat 30s ago" |

## Detailed subtasks

### 1. Rewrite the connect handler

```rust
// crates/agentos-web/src/handlers/agents.rs
#[derive(Deserialize)]
pub struct ConnectForm {
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub roles: Option<String>,           // CSV in form, parsed into Vec<String>
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub thinking_level: Option<String>,  // "off" | "low" | "medium" | "high" | "max"
}

pub async fn connect(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::Form(form): axum::Form<ConnectForm>,
) -> Response {
    use agentos_api::types::ConnectAgentRequest;

    // Server-side validation
    if form.name.trim().is_empty() {
        return form_error(StatusCode::UNPROCESSABLE_ENTITY, "Agent name is required");
    }
    if !is_valid_agent_name(&form.name) {
        return form_error(StatusCode::UNPROCESSABLE_ENTITY,
            "Agent name must be lowercase alphanumeric with dashes");
    }
    if form.model.trim().is_empty() {
        return form_error(StatusCode::UNPROCESSABLE_ENTITY, "Model is required");
    }
    if let Some(url) = &form.base_url {
        if !url.is_empty() && url::Url::parse(url).is_err() {
            return form_error(StatusCode::UNPROCESSABLE_ENTITY, "Base URL must be a valid URL");
        }
    }

    let roles: Vec<String> = form.roles
        .as_deref().unwrap_or("")
        .split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from)
        .collect();

    let req = ConnectAgentRequest {
        name: form.name.clone(),
        provider: form.provider.clone(),
        model: form.model.clone(),
        base_url: form.base_url.filter(|u| !u.is_empty()),
        roles,
        // The API type may need to be extended for system_prompt + thinking_level —
        // see Phase 04 step 5.
    };

    match state.service.connect_agent(req).await {
        Ok(_) => {
            // Re-render the agent grid partial with the updated list.
            let agents = state.service.list_agents().await.unwrap_or_default();
            let agents_ctx: Vec<_> = agents.iter().map(agent_to_ctx).collect();
            let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
            let ctx = context! { agents => agents_ctx, csrf_token };

            let mut response = super::render(&state.templates, "partials/agent_card.html", ctx);

            let trigger = serde_json::json!({
                "showToast": {"message": format!("Agent '{}' connected", form.name), "type": "success"},
                "closeAgentModal": true
            }).to_string();
            if let Ok(hv) = axum::http::HeaderValue::from_str(&trigger) {
                response.headers_mut().insert("HX-Trigger", hv);
            }
            response
        }
        Err(e) => {
            tracing::error!(agent = %form.name, error = %e, "Failed to connect agent");
            form_error(StatusCode::UNPROCESSABLE_ENTITY,
                &format!("Failed to connect agent: {}", e))
        }
    }
}

fn form_error(status: StatusCode, message: &str) -> Response {
    let html = format!(
        r#"<small class="form-error" id="connect-form-error" role="alert">{}</small>"#,
        html_escape(message)
    );
    let mut response = (status, Html(html)).into_response();
    response.headers_mut().insert("HX-Retarget", HeaderValue::from_static("#connect-form-error"));
    response.headers_mut().insert("HX-Reswap", HeaderValue::from_static("outerHTML"));
    let trigger = serde_json::json!({
        "showToast": {"message": message, "type": "error"}
    }).to_string();
    if let Ok(hv) = HeaderValue::from_str(&trigger) {
        response.headers_mut().insert("HX-Trigger", hv);
    }
    response
}
```

The two new HTMX response headers do the heavy lifting:
- `HX-Retarget: #connect-form-error` — overrides the form's `hx-target`, so the error fragment is swapped *inside* the modal, not in the agent grid.
- `HX-Reswap: outerHTML` — replaces the entire `#connect-form-error` element so a previous error gets cleared on each new attempt.

### 2. Update the form template

```html
<!-- crates/agentos-web/src/templates/agents.html -->
<dialog :open="showModal" @close="showModal = false"
        x-on:close-agent-modal.window="showModal = false"
        aria-labelledby="connect-agent-title" role="dialog">
    <article>
        <header>
            <button aria-label="Close dialog" rel="prev" @click="showModal = false"></button>
            <h3 id="connect-agent-title">Connect Agent</h3>
        </header>
        <form id="connect-agent-form"
              hx-post="/agents"
              hx-target="#agent-grid"
              hx-swap="innerHTML">
            <input type="hidden" name="_csrf" value="{{ csrf_token }}">

            <div class="form-grid-2">
                <label>Agent Name
                    <input type="text" name="name" required pattern="[a-z0-9][a-z0-9-]*"
                           placeholder="my-agent">
                    <small class="muted">lowercase, alphanumeric + dashes</small>
                </label>
                <label>Provider
                    <select name="provider" required>
                        {% for p in providers %}
                        <option value="{{ p.id }}">{{ p.label }}</option>
                        {% endfor %}
                    </select>
                </label>
            </div>

            <div class="form-grid-2">
                <label>Model
                    <input type="text" name="model" required
                           placeholder="llama3.2 / gpt-4o / claude-opus-4-6">
                </label>
                <label>Base URL <small class="muted">(optional)</small>
                    <input type="url" name="base_url"
                           placeholder="http://192.168.1.100:11434">
                </label>
            </div>

            <label>Roles <small class="muted">(comma-separated, optional)</small>
                <input type="text" name="roles" placeholder="researcher, planner, coder">
            </label>

            <label>System Prompt <small class="muted">(optional)</small>
                <textarea name="system_prompt" rows="3"
                          placeholder="You are a helpful agent that…"></textarea>
            </label>

            <label>Thinking Budget
                <select name="thinking_level">
                    <option value="off" selected>Off</option>
                    <option value="low">Low</option>
                    <option value="medium">Medium</option>
                    <option value="high">High</option>
                    <option value="max">Max</option>
                </select>
            </label>

            <small class="form-error" id="connect-form-error" role="alert"></small>

            <div class="form-actions">
                <button type="button" class="outline secondary" @click="showModal = false">Cancel</button>
                <button type="submit">Connect</button>
            </div>
        </form>
    </article>
</dialog>
```

Key removals:
- No more `@htmx:after-request="showModal = false"` — replaced by the `closeAgentModal` event listener at the dialog level.
- No more inline `method="post" action="/agents"` (removes the no-JS fallback path that would also redirect-then-render-bad).

### 3. Wire the `closeAgentModal` event globally

In `static/js/app.js` (existing global JS):

```javascript
document.body.addEventListener('htmx:afterRequest', function (e) {
    var headers = e.detail.xhr.getResponseHeader('HX-Trigger');
    if (!headers) return;
    try {
        var triggers = JSON.parse(headers);
        if (triggers.closeAgentModal) {
            window.dispatchEvent(new CustomEvent('close-agent-modal'));
        }
        // Toast handling already exists
    } catch (_) {}
});
```

(The HTMX htmx-trigger header parsing for toasts is already present elsewhere — extend it.)

### 4. Provider list from `ProviderCatalog`

Pass `providers` to the agents.html context:

```rust
pub async fn list(...) -> Response {
    let providers = state.kernel.provider_catalog.list_providers()
        .into_iter()
        .map(|p| context! { id => p.id, label => format!("{} ({})", p.label, p.id) })
        .collect::<Vec<_>>();
    // ...
    let ctx = context! { agents, providers, csrf_token, /* ... */ };
}
```

### 5. Extend `ConnectAgentRequest` if needed

Check [crates/agentos-api/src/types.rs](crates/agentos-api/src/types.rs). If `ConnectAgentRequest` lacks `system_prompt` and `thinking_level`, add them as `Option`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConnectAgentRequest {
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub thinking_level: Option<String>,
}
```

The agent service then forwards these to the kernel `RegisterAgent` command. If the kernel command struct lacks fields too, add them similarly. The CLI's `agent connect` already supports `--system-prompt` and `--thinking-level` so the underlying types likely exist — extend the API struct only.

### 6. Agent detail page tabs

Replace the flat agent detail page with a tabbed layout:

```html
<!-- crates/agentos-web/src/templates/agent_detail.html -->
{% extends "base.html" %}
{% block content %}
<div class="page-header">
    <h1>{{ agent.name }} <small class="muted">{{ agent.provider }} · {{ agent.model }}</small></h1>
    <div class="page-header-actions">
        <button class="outline secondary btn-sm" hx-delete="/agents/{{ agent.name }}"
                hx-confirm="Disconnect this agent?" hx-target="body">Disconnect</button>
    </div>
</div>

<nav class="tab-nav" role="tablist">
    <a href="#overview" role="tab" hx-get="/agents/{{ agent.name }}/overview"
       hx-target="#tab-content" hx-trigger="click,load">Overview</a>
    <a href="#permissions" role="tab" hx-get="/agents/{{ agent.name }}/permissions"
       hx-target="#tab-content">Permissions</a>
    <a href="#memory" role="tab" hx-get="/agents/{{ agent.name }}/memory"
       hx-target="#tab-content">Memory</a>
    <a href="#scratchpad" role="tab" hx-get="/agents/{{ agent.name }}/scratchpad"
       hx-target="#tab-content">Scratchpad</a>
    <a href="#identity" role="tab" hx-get="/agents/{{ agent.name }}/identity"
       hx-target="#tab-content">Identity</a>
    <a href="#audit" role="tab" hx-get="/agents/{{ agent.name }}/audit"
       hx-target="#tab-content">Audit</a>
</nav>
<section id="tab-content" class="tab-content"></section>
{% endblock %}
```

Each tab has its own handler returning a partial. The Overview tab consolidates current detail page content + heartbeat / busy-with count. Memory shows the per-agent memory tier counts and a query box. Scratchpad lists the agent's scratchpad pages with link-out to view individually. Identity shows the Ed25519 public key + key fingerprint. Audit is a filtered audit log query (`agent_id = X`).

This phase only ships the **Overview** and **Permissions** tabs (Permissions already exists). The other tabs ship in Phase 06/07. The tab nav is added now so the detail page visually communicates the future structure.

### 7. Agent grid status indicators

In `partials/agent_card.html`:

```html
<article class="agent-card status-{{ agent.status|lower }}">
    <header>
        <strong>{{ agent.name }}</strong>
        <span class="badge badge-{{ agent.status|lower }}">{{ agent.status }}</span>
        {% if agent.busy_with > 0 %}
        <span class="muted"><small>busy with {{ agent.busy_with }} task{% if agent.busy_with != 1 %}s{% endif %}</small></span>
        {% endif %}
    </header>
    <p class="muted">{{ agent.provider }} · {{ agent.model }}</p>
    {% if agent.last_heartbeat %}
    <small class="muted">Last seen {{ agent.last_heartbeat|relative_time }}</small>
    {% endif %}
    <footer>
        <a href="/agents/{{ agent.name }}/detail" role="button" class="outline btn-sm">Details</a>
        <button class="outline secondary btn-sm" hx-delete="/agents/{{ agent.name }}"
                hx-target="closest article" hx-swap="outerHTML"
                hx-confirm="Disconnect {{ agent.name }}?">Disconnect</button>
    </footer>
</article>
```

`busy_with` and `last_heartbeat` come from the agent service — Phase 04 either adds them to `AgentInfo` or the template gracefully omits if absent.

## Files changed

| File | Change |
|------|--------|
| [crates/agentos-web/src/handlers/agents.rs](crates/agentos-web/src/handlers/agents.rs) | Rewrite `connect`; add `form_error` helper; extend ConnectForm fields |
| [crates/agentos-web/src/templates/agents.html](crates/agentos-web/src/templates/agents.html) | New form fields, error placeholder, dialog event listener |
| [crates/agentos-web/src/templates/partials/agent_card.html](crates/agentos-web/src/templates/partials/agent_card.html) | Status badge, busy count, heartbeat |
| [crates/agentos-web/src/templates/agent_detail.html](crates/agentos-web/src/templates/agent_detail.html) | Tabbed layout |
| [crates/agentos-web/static/js/app.js](crates/agentos-web/static/js/app.js) | `closeAgentModal` HX-Trigger handler |
| [crates/agentos-api/src/types.rs](crates/agentos-api/src/types.rs) | Optional `system_prompt`, `thinking_level` on ConnectAgentRequest (if not already present) |
| [crates/agentos-web/src/router.rs](crates/agentos-web/src/router.rs) | New routes for tab content handlers (Overview only in this phase) |

## Dependencies

- Independent of: 01, 02, 03, 05, 06, 07
- Soft-blocks: [[06-cli-parity-management-pages]] (provider list, identity tab)

## Test plan

1. **Integration: connect happy path**
   - POST `/agents` with valid form. Assert 200, response body contains agent card markup, response headers contain `HX-Trigger` with `closeAgentModal`.
2. **Integration: connect validation error**
   - POST `/agents` with empty name. Assert 422, body contains "Agent name is required", `HX-Retarget` is `#connect-form-error`.
3. **Integration: connect kernel error**
   - POST `/agents` with valid form but a provider unknown to the kernel. Assert 422, body contains the kernel error message.
4. **Integration: provider list**
   - Boot the kernel, GET `/agents`, assert the rendered HTML contains every provider from `ProviderCatalog`.
5. **Manual: full flow**
   - Open `/agents`, click "Connect New Agent", fill the form with a valid Ollama agent, click Connect. Expected: modal closes, toast appears, agent grid updates with the new card. No layout corruption.
6. **Manual: failure flow**
   - Same form but with invalid name "foo!bar". Expected: modal stays open, red error text appears under the form, toast says "must be lowercase alphanumeric".

## Verification

```bash
cargo build -p agentos-web -p agentos-api
cargo test -p agentos-web handlers::agents
cargo clippy -p agentos-web -- -D warnings
cargo fmt --all -- --check

# Manual smoke
docker compose restart agentos-kernel
# Open http://localhost:8080/agents in a browser
# Try valid + invalid connect submits — verify behavior matches test plan
```

## Related

- [[WebUI Overhaul Plan]]
- [[06-cli-parity-management-pages]]
