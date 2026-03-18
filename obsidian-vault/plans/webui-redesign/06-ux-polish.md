---
title: "Phase 06: UX Polish -- Empty States, Skeletons, Toasts, and Accessibility"
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

# Phase 06: UX Polish -- Empty States, Skeletons, Toasts, and Accessibility

> Add consistent empty states, skeleton loading placeholders, toast notifications for action feedback, and comprehensive accessibility (ARIA labels, keyboard navigation, focus management).

---

## Why This Phase

The UI currently provides no visual feedback for:
- **Empty data** -- tables and grids show blank content with no guidance
- **Loading** -- HTMX swaps happen silently; users do not know data is being fetched
- **Action results** -- form submissions redirect to pages without confirmation
- **Keyboard users** -- no skip link, no focus management, limited ARIA labeling

These are table-stakes UX patterns that make the difference between "it works" and "it feels polished." This phase addresses all of them systematically across every page.

---

## Current to Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Empty states | Blank tables/grids | Consistent empty state component with icon, message, and action link |
| Loading indicators | None | HTMX `htmx-indicator` class shows skeleton placeholders during requests |
| Toast notifications | None -- errors show as page-level responses | Alpine.js managed toast container with success/error/info types |
| ARIA labels | Partial (log terminal has `role="log"`) | Comprehensive: sidebar nav, main content, tables, forms, toasts |
| Focus management | None | Focus trapped in modals, restored on close; skip link to main content |
| Keyboard shortcuts | None (added in Phase 01) | Help modal listing all shortcuts, triggered by `Shift+?` |
| Status indicators | Colored badges only | Badges with semantic ARIA labels (`aria-label="Status: Running"`) |

---

## Detailed Subtasks

### 1. Create reusable empty state partial

Create `crates/agentos-web/src/templates/partials/empty_state.html`:

```html
{# Parameters: icon (string), title (string), message (string), action_href (optional), action_label (optional) #}
<div class="empty-state" role="status">
    <p class="empty-state-icon" aria-hidden="true">{{ icon }}</p>
    <p class="empty-state-title">{{ title }}</p>
    {% if message %}
    <p class="empty-state-message muted">{{ message }}</p>
    {% endif %}
    {% if action_href %}
    <a href="{{ action_href }}" role="button" class="outline">{{ action_label }}</a>
    {% endif %}
</div>
```

Add empty state CSS to `app.css`:

```css
/* ── Empty States ───────────────────────────────────────── */
.empty-state {
    text-align: center;
    padding: 3rem 1rem;
}
.empty-state-icon {
    font-size: 3rem;
    margin-bottom: 0.5rem;
    opacity: 0.4;
}
.empty-state-title {
    font-size: 1.1rem;
    font-weight: 600;
    margin-bottom: 0.25rem;
}
.empty-state-message {
    font-size: 0.9rem;
    margin-bottom: 1rem;
}
```

### 2. Add empty states to all page templates

For each page, wrap the data display in a conditional and show the empty state when data is absent.

**agents.html:** After the agent grid:
```html
{% if agents|length == 0 %}
{% with icon="&#9679;", title="No agents connected", message="Connect an agent to get started.", action_href="#", action_label="Connect Agent" %}
{% include "partials/empty_state.html" %}
{% endwith %}
{% endif %}
```

**tasks.html:** After the task table:
```html
{% if tasks|length == 0 %}
{% with icon="&#9654;", title="No tasks found", message="Tasks appear here when agents begin work." %}
{% include "partials/empty_state.html" %}
{% endwith %}
{% endif %}
```

**tools.html:** After the tool grid:
```html
{% if tools|length == 0 %}
{% with icon="&#9881;", title="No tools installed", message="Install a tool to extend agent capabilities.", action_href="#", action_label="Install Tool" %}
{% include "partials/empty_state.html" %}
{% endwith %}
{% endif %}
```

**secrets.html:** After the secrets table:
```html
{% if secrets|length == 0 %}
{% with icon="&#128274;", title="No secrets stored", message="Add API keys and credentials for agents to use." %}
{% include "partials/empty_state.html" %}
{% endwith %}
{% endif %}
```

**pipelines.html:** After the pipelines table:
```html
{% if pipelines|length == 0 %}
{% with icon="&#8658;", title="No pipelines installed", message="Pipelines define multi-step agent workflows." %}
{% include "partials/empty_state.html" %}
{% endwith %}
{% endif %}
```

### 3. Add skeleton loading indicators

Add skeleton CSS to `app.css`:

```css
/* ── Skeleton Loaders ───────────────────────────────────── */
.skeleton {
    display: none;
}
.htmx-request .skeleton {
    display: block;
}
.htmx-request .htmx-content {
    display: none;
}

.skeleton-line {
    height: 1rem;
    background: linear-gradient(90deg,
        var(--pico-muted-border-color) 25%,
        transparent 50%,
        var(--pico-muted-border-color) 75%
    );
    background-size: 200% 100%;
    animation: skeleton-pulse 1.5s ease-in-out infinite;
    border-radius: 0.25rem;
    margin-bottom: 0.5rem;
}
.skeleton-line:nth-child(2) { width: 80%; }
.skeleton-line:nth-child(3) { width: 60%; }

.skeleton-card {
    height: 120px;
    background: var(--pico-muted-border-color);
    border-radius: var(--pico-border-radius);
    animation: skeleton-pulse 1.5s ease-in-out infinite;
}

.skeleton-row {
    height: 2.5rem;
    background: var(--pico-muted-border-color);
    border-radius: 0.25rem;
    animation: skeleton-pulse 1.5s ease-in-out infinite;
    margin-bottom: 0.25rem;
}

@keyframes skeleton-pulse {
    0% { opacity: 1; }
    50% { opacity: 0.4; }
    100% { opacity: 1; }
}
```

Add skeleton placeholders to swap targets. For example, in the agent grid:

```html
<div id="agent-grid" class="grid" hx-get="/agents?partial=list" hx-trigger="every 5s" hx-swap="innerHTML">
    <div class="skeleton" aria-hidden="true">
        <div class="skeleton-card"></div>
        <div class="skeleton-card"></div>
        <div class="skeleton-card"></div>
    </div>
    <div class="htmx-content">
        {% include "partials/agent_card.html" %}
    </div>
</div>
```

### 4. Create toast notification system

Add to `crates/agentos-web/static/js/app.js`:

```javascript
// Toast notification store (Alpine.js component)
function toastStore() {
    return {
        toasts: [],
        addToast: function(detail) {
            var message = typeof detail === 'string' ? detail : (detail.message || '');
            var type = (detail && detail.type) ? detail.type : 'info';
            var id = Date.now() + Math.random();
            this.toasts.push({ id: id, message: message, type: type });
            var self = this;
            var timeout = type === 'error' ? 8000 : 5000;
            setTimeout(function() {
                self.removeToast(id);
            }, timeout);
        },
        removeToast: function(id) {
            this.toasts = this.toasts.filter(function(t) { return t.id !== id; });
        }
    };
}
```

Create `crates/agentos-web/src/templates/partials/toast_container.html`:

```html
<div class="toast-container" x-data="toastStore()" @show-toast.window="addToast($event.detail)">
    <template x-for="toast in toasts" :key="toast.id">
        <div class="toast" :class="'toast-' + toast.type" role="alert" aria-live="assertive">
            <span x-text="toast.message"></span>
            <button class="toast-close" @click="removeToast(toast.id)" aria-label="Dismiss">&times;</button>
        </div>
    </template>
</div>
```

Add toast CSS to `app.css`:

```css
/* ── Toast Notifications ────────────────────────────────── */
.toast-container {
    position: fixed;
    bottom: 1rem;
    right: 1rem;
    z-index: 1000;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    max-width: 400px;
}

.toast {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-radius: var(--pico-border-radius);
    font-size: 0.85rem;
    color: #fff;
    box-shadow: 0 4px 12px rgba(0,0,0,0.15);
    animation: toast-slide-in 0.3s ease-out;
}

.toast-info { background: #17a2b8; }
.toast-success { background: #28a745; }
.toast-error { background: #dc3545; }
.toast-warning { background: #ffc107; color: #000; }

.toast-close {
    background: none;
    border: none;
    color: inherit;
    font-size: 1.2rem;
    cursor: pointer;
    padding: 0;
    line-height: 1;
    opacity: 0.7;
}
.toast-close:hover { opacity: 1; }

@keyframes toast-slide-in {
    from { transform: translateX(100%); opacity: 0; }
    to { transform: translateX(0); opacity: 1; }
}
```

Add the toast container to `base.html` (before closing `</body>` or inside the `main-wrapper`):

```html
{% include "partials/toast_container.html" %}
```

### 5. Wire HTMX response headers to trigger toasts

Update Rust handlers to return `HX-Trigger` headers on success/error. For example, in `handlers/agents.rs` `connect()`:

```rust
pub async fn connect(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<ConnectForm>,
) -> Response {
    // ... existing validation ...

    match state.kernel.api_connect_agent(form.name.clone(), provider, form.model, None, vec![]).await {
        Ok(()) => {
            let mut response = axum::response::Redirect::to("/agents").into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_str(
                    &format!(r#"{{"showToast": {{"message": "Agent '{}' connected", "type": "success"}}}}"#, form.name)
                ).unwrap_or_else(|_| axum::http::HeaderValue::from_static("")),
            );
            response
        }
        Err(msg) => {
            tracing::error!(agent = %form.name, error = %msg, "Failed to connect agent");
            let mut response = (StatusCode::BAD_REQUEST, "Failed to connect agent").into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast": {"message": "Failed to connect agent", "type": "error"}}"#
                ),
            );
            response
        }
    }
}
```

Apply the same pattern to: `agents::disconnect`, `tools::install`, `tools::remove`, `secrets::create`, `secrets::revoke`, `pipelines::run`, `tasks::cancel`.

Add HTMX event listener in `app.js` to bridge `HX-Trigger` events to Alpine:

```javascript
// Bridge HTMX HX-Trigger "showToast" events to Alpine's custom event system
document.addEventListener('htmx:trigger', function(event) {
    // htmx:trigger fires for each key in HX-Trigger header
    // The event name comes from the key, detail from the value
});

// Alternative: listen for the custom event name directly
document.body.addEventListener('showToast', function(event) {
    // Alpine x-on:show-toast.window will catch this
    window.dispatchEvent(new CustomEvent('show-toast', { detail: event.detail }));
});
```

### 6. Add comprehensive ARIA labels

Update templates with proper ARIA attributes:

**base.html sidebar:**
```html
<aside class="sidebar" aria-label="Main navigation" role="navigation">
```

**All table captions:**
```html
<table class="audit-table" aria-label="Audit log entries">
```

**All badges:**
```html
<span class="badge badge-{{ state|lower }}" role="status" aria-label="Status: {{ state }}">{{ state }}</span>
```

**Dialog modals:**
```html
<dialog :open="showModal" @close="showModal = false" aria-labelledby="modal-title" role="dialog">
    <article>
        <header>
            <button aria-label="Close dialog" rel="prev" @click="showModal = false"></button>
            <h3 id="modal-title">Connect Agent</h3>
        </header>
```

**Form inputs:**
```html
<input type="text" id="name" name="name" required aria-required="true"
       aria-describedby="name-hint">
<small id="name-hint" class="muted">A unique name for this agent</small>
```

### 7. Add keyboard shortcut help modal

Create `crates/agentos-web/src/templates/partials/shortcuts_modal.html`:

```html
<dialog x-data="{ show: false }" @show-shortcuts.window="show = true" :open="show" @close="show = false"
        aria-labelledby="shortcuts-title" role="dialog">
    <article style="max-width: 480px;">
        <header>
            <button aria-label="Close" rel="prev" @click="show = false"></button>
            <h3 id="shortcuts-title">Keyboard Shortcuts</h3>
        </header>
        <table>
            <thead>
                <tr><th>Key</th><th>Action</th></tr>
            </thead>
            <tbody>
                <tr><td><kbd>g</kbd> then <kbd>d</kbd></td><td>Go to Dashboard</td></tr>
                <tr><td><kbd>g</kbd> then <kbd>a</kbd></td><td>Go to Agents</td></tr>
                <tr><td><kbd>g</kbd> then <kbd>t</kbd></td><td>Go to Tasks</td></tr>
                <tr><td><kbd>g</kbd> then <kbd>o</kbd></td><td>Go to Tools</td></tr>
                <tr><td><kbd>g</kbd> then <kbd>s</kbd></td><td>Go to Secrets</td></tr>
                <tr><td><kbd>g</kbd> then <kbd>p</kbd></td><td>Go to Pipelines</td></tr>
                <tr><td><kbd>g</kbd> then <kbd>l</kbd></td><td>Go to Audit Log</td></tr>
                <tr><td><kbd>?</kbd></td><td>Show this help</td></tr>
            </tbody>
        </table>
    </article>
</dialog>
```

Include in `base.html`:
```html
{% include "partials/shortcuts_modal.html" %}
```

### 8. Register new partials in `templates.rs`

Open `crates/agentos-web/src/templates.rs`. Add:

```rust
env.add_template(
    "partials/empty_state.html",
    include_str!("templates/partials/empty_state.html"),
)?;
env.add_template(
    "partials/toast_container.html",
    include_str!("templates/partials/toast_container.html"),
)?;
env.add_template(
    "partials/shortcuts_modal.html",
    include_str!("templates/partials/shortcuts_modal.html"),
)?;
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/partials/empty_state.html` | **New** -- reusable empty state component |
| `crates/agentos-web/src/templates/partials/toast_container.html` | **New** -- Alpine.js managed toast container |
| `crates/agentos-web/src/templates/partials/shortcuts_modal.html` | **New** -- keyboard shortcut help dialog |
| `crates/agentos-web/src/templates/base.html` | Include toast container and shortcuts modal |
| `crates/agentos-web/src/templates/agents.html` | Add empty state for zero agents |
| `crates/agentos-web/src/templates/tasks.html` | Add empty state for zero tasks |
| `crates/agentos-web/src/templates/tools.html` | Add empty state for zero tools |
| `crates/agentos-web/src/templates/secrets.html` | Add empty state for zero secrets |
| `crates/agentos-web/src/templates/pipelines.html` | Add empty state for zero pipelines |
| `crates/agentos-web/static/css/app.css` | Add empty-state, skeleton, toast, and animation styles |
| `crates/agentos-web/static/js/app.js` | Add `toastStore()` Alpine component, HTMX-to-Alpine toast bridge |
| `crates/agentos-web/src/templates.rs` | Register 3 new partials |
| `crates/agentos-web/src/handlers/agents.rs` | Add `HX-Trigger` toast headers to connect/disconnect responses |
| `crates/agentos-web/src/handlers/tools.rs` | Add `HX-Trigger` toast headers to install/remove responses |
| `crates/agentos-web/src/handlers/secrets.rs` | Add `HX-Trigger` toast headers to create/revoke responses |
| `crates/agentos-web/src/handlers/pipelines.rs` | Add `HX-Trigger` toast header to run response |
| `crates/agentos-web/src/handlers/tasks.rs` | Add `HX-Trigger` toast header to cancel response |

---

## Dependencies

[[01-layout-navigation]] must be complete first (base.html shell with `app.js` loaded).

Other phases (02-05) can be complete or in-progress -- this phase adds cross-cutting polish that applies to all pages.

---

## Test Plan

- `cargo build -p agentos-web` must compile
- `cargo test -p agentos-web` must pass
- `cargo clippy -p agentos-web -- -D warnings` must pass
- Manual verification:
  - **Empty states:** With no agents, the agents page shows "No agents connected" with a CTA button. Same for tasks, tools, secrets, pipelines.
  - **Skeleton loaders:** During an HTMX request, the skeleton placeholder pulses in the swap target area. Visible by throttling network in DevTools.
  - **Toasts:** Connecting an agent shows a green "Agent 'X' connected" toast that auto-dismisses after 5 seconds. Errors show red toasts that persist for 8 seconds. Toast has a close button.
  - **ARIA:** Screen reader (or browser accessibility inspector) correctly announces:
    - Sidebar navigation as "Main navigation"
    - Status badges as "Status: Running"
    - Toast notifications as alerts
    - Modal dialogs with their titles
  - **Keyboard shortcuts:** Pressing `Shift+?` opens the shortcuts help modal. Pressing `g` then `a` navigates to Agents. The modal closes with `Escape`.
  - **Focus management:** When a modal opens, focus moves to the first input. When it closes, focus returns to the trigger button.

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
cargo clippy -p agentos-web -- -D warnings
cargo fmt -p agentos-web -- --check
```
