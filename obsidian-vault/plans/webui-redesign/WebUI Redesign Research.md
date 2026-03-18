---
title: WebUI Redesign Research
tags:
  - webui
  - htmx
  - frontend
  - reference
date: 2026-03-18
status: planned
effort: 0d
priority: high
---

# WebUI Redesign Research

> Design rationale, technology patterns, and reference implementations for the AgentOS WebUI redesign.

---

## Pico CSS Patterns

### Custom Properties for Theming

Pico CSS v2.1.1 exposes extensive CSS custom properties for customization. Key properties:

```css
/* Override in app.css to customize without modifying Pico */
:root {
    --pico-font-family: system-ui, -apple-system, sans-serif;
    --pico-border-radius: 0.375rem;
    --pico-spacing: 1rem;
    --pico-color: #373c44;
    --pico-primary: #1095c1;
    --pico-primary-hover: #0d7fa3;
}

/* Dark theme is built into Pico via [data-theme="dark"] */
[data-theme="dark"] {
    --pico-background-color: #11191f;
    --pico-color: #c2c7d0;
}
```

### Pico Grid System

Pico provides a simple grid via the `.grid` class. For the sidebar layout, we use native CSS Grid instead, since Pico's grid is for equal-width columns:

```css
.app-shell {
    display: grid;
    grid-template-columns: 240px 1fr;
    grid-template-rows: auto 1fr auto;
    min-height: 100vh;
}

@media (max-width: 768px) {
    .app-shell {
        grid-template-columns: 1fr;
    }
}
```

### Pico Dialog/Modal

Pico provides native `<dialog>` styling. Current agents and secrets pages already use this pattern correctly:

```html
<dialog :open="showModal" @close="showModal = false">
    <article>
        <header>...</header>
        <form>...</form>
    </article>
</dialog>
```

---

## HTMX Patterns

### Partial Swap Pattern (already used)

All pages follow the same pattern for HTMX partial rendering:

```
GET /resource            -> full page (extends base.html)
GET /resource?partial=X  -> partial HTML fragment (just the inner content)
```

Handler code pattern:
```rust
if query.partial.as_deref() == Some("list") {
    return super::render(&state.templates, "partials/thing.html", ctx);
}
```

### HTMX SSE Extension

HTMX provides `hx-ext="sse"` for Server-Sent Events integration:

```html
<div hx-ext="sse" sse-connect="/events/agents" sse-swap="agent-update">
    <!-- Content is swapped when server sends event with name "agent-update" -->
</div>
```

Server sends named events:
```
event: agent-update
data: <rendered HTML partial>

event: task-update
data: <rendered HTML partial>
```

**Note:** The HTMX SSE extension is a separate JS file (`sse.js`) that must be loaded alongside `htmx.min.js`. It is available as part of the HTMX distribution.

### HTMX Loading Indicators

HTMX automatically adds the `htmx-request` class to elements during requests:

```css
/* Show skeleton loader during HTMX requests */
.htmx-indicator {
    display: none;
}
.htmx-request .htmx-indicator {
    display: block;
}
.htmx-request .htmx-content {
    display: none;
}
```

### HTMX Response Headers for Toasts

Handlers can trigger client-side events via the `HX-Trigger` response header:

```rust
// In Rust handler:
let mut response = axum::response::Redirect::to("/agents").into_response();
response.headers_mut().insert(
    "HX-Trigger",
    HeaderValue::from_static(r#"{"showToast": {"message": "Agent connected", "type": "success"}}"#),
);
```

Alpine.js listens for this event:
```html
<div x-data="toastStore()" @show-toast.window="addToast($event.detail)">
```

---

## Alpine.js Patterns

### Toast Notification Store

```javascript
// static/js/app.js
function toastStore() {
    return {
        toasts: [],
        addToast(detail) {
            var toast = {
                id: Date.now(),
                message: detail.message || detail,
                type: detail.type || 'info',
            };
            this.toasts.push(toast);
            setTimeout(() => {
                this.toasts = this.toasts.filter(t => t.id !== toast.id);
            }, 5000);
        },
        removeToast(id) {
            this.toasts = this.toasts.filter(t => t.id !== id);
        }
    };
}
```

### Theme Toggle

```javascript
function themeToggle() {
    return {
        dark: localStorage.getItem('theme') === 'dark',
        toggle() {
            this.dark = !this.dark;
            document.documentElement.setAttribute(
                'data-theme', this.dark ? 'dark' : 'light'
            );
            localStorage.setItem('theme', this.dark ? 'dark' : 'light');
        },
        init() {
            if (this.dark) {
                document.documentElement.setAttribute('data-theme', 'dark');
            }
        }
    };
}
```

### Keyboard Shortcut Handler

```javascript
function keyboardShortcuts() {
    return {
        init() {
            document.addEventListener('keydown', (e) => {
                // Do not intercept when typing in inputs
                if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA' ||
                    e.target.tagName === 'SELECT' || e.target.isContentEditable) return;

                if (e.key === 'g' && !e.ctrlKey && !e.metaKey) {
                    // "g" prefix for go-to shortcuts
                    this.awaitingGoto = true;
                    setTimeout(() => { this.awaitingGoto = false; }, 1000);
                } else if (this.awaitingGoto) {
                    this.awaitingGoto = false;
                    var routes = { d: '/', a: '/agents', t: '/tasks', o: '/tools',
                                   s: '/secrets', p: '/pipelines', l: '/audit' };
                    if (routes[e.key]) window.location.href = routes[e.key];
                } else if (e.key === '?' && e.shiftKey) {
                    // Show keyboard shortcut help
                    this.$dispatch('show-shortcuts');
                }
            });
        },
        awaitingGoto: false
    };
}
```

---

## SSE Architecture

### Connection Management

Each page opens at most one SSE connection. Connections are closed when:
- The browser tab is hidden (`visibilitychange` event)
- The user navigates away (standard EventSource behavior)
- The server signals completion (`event: done`)

### Multiplexed Event Streams

Rather than one SSE endpoint per resource, we use domain-scoped endpoints:

| Endpoint | Events | Used By |
|----------|--------|---------|
| `/events/dashboard` | `agent-count`, `task-count`, `audit-recent` | Dashboard page |
| `/events/agents` | `agent-update` | Agents page |
| `/events/tasks` | `task-update` | Tasks page |
| `/events/audit` | `audit-entry` | Audit log page |
| `/tasks/{id}/logs/stream` | `message`, `done` | Task detail (already exists) |

### SSE Handler Pattern

```rust
pub async fn events_dashboard(
    State(state): State<AppState>,
) -> Sse<KeepAliveStream<BoxStream<'static, Result<Event, Infallible>>>> {
    let kernel = state.kernel.clone();
    let templates = state.templates.clone();

    let stream = stream::unfold((), move |()| {
        let kernel = kernel.clone();
        let templates = templates.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            // Render the dashboard stats partial
            let agent_count = kernel.agent_registry.read().await.list_online().len();
            // ... build context, render partial ...
            let html = templates.get_template("partials/dashboard_stats.html")
                .and_then(|t| t.render(ctx)).unwrap_or_default();
            Some((Ok(Event::default().event("dashboard-stats").data(html)), ()))
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}
```

---

## Accessibility Requirements

### ARIA Landmarks

```html
<nav aria-label="Main navigation">...</nav>
<main role="main" aria-label="Page content">...</main>
<aside aria-label="Sidebar navigation">...</aside>
<div role="log" aria-live="polite">...</div>  <!-- already used in task_detail.html -->
```

### Focus Management

- Modal open: focus first input inside the dialog
- Modal close: restore focus to the trigger button
- Toast appear: `role="alert"` with `aria-live="assertive"`
- HTMX swap: do not move focus unless navigating to a new section

### Skip Navigation

```html
<a href="#main-content" class="skip-link">Skip to main content</a>
```

---

## Related

- [[WebUI Redesign Plan]]
- [[WebUI Redesign Data Flow]]
- [[23-WebUI Security Fixes]]
