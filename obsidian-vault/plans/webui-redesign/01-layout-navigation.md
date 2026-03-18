---
title: "Phase 01: Layout and Navigation Shell"
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

# Phase 01: Layout and Navigation Shell

> Replace the flat horizontal nav with a responsive sidebar + topbar shell layout using CSS Grid and Pico CSS custom properties.

---

## Why This Phase

The navigation shell is the foundation that every page inherits via `base.html`. Currently it is a single horizontal nav bar that:
- Does not scale to mobile screens (7 nav items overflow horizontally)
- Provides no visual hierarchy between sections
- Has no breadcrumb trail for nested pages (e.g., `/tasks/{id}`)
- Has no global actions area (theme toggle, connection status)

All subsequent phases depend on this shell being in place, since they add content that must fit within the new layout grid.

---

## Current to Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Layout | Single `<nav>` + `<main class="container">` + `<footer>` | CSS Grid shell: `aside.sidebar` + `header.topbar` + `main#main-content` + `footer` |
| Navigation | 7 links in a horizontal `<ul>` inside `<nav>` | Vertical sidebar with section icons (Unicode), active state highlight, collapsible on mobile |
| Mobile | No responsive handling | Hamburger button in topbar reveals sidebar as overlay on screens < 768px |
| Breadcrumbs | None | Topbar shows `Dashboard > Tasks > Task abc123` using MiniJinja `breadcrumbs` variable |
| Theme toggle | None | Dark/light toggle button in topbar using `[data-theme]` attribute (Pico built-in) |
| Skip link | None | `<a href="#main-content" class="skip-link">Skip to main content</a>` for keyboard users |
| Active nav | JS scans `data-nav` and adds `.nav-active` class | Same approach, but styled differently for vertical sidebar |

---

## Detailed Subtasks

### 1. Update `base.html` with the sidebar layout shell

Open `crates/agentos-web/src/templates/base.html`.

Replace the current `<body>` content with the new shell structure:

```html
<body x-data="{ sidebarOpen: false }" @keydown.escape="sidebarOpen = false">
    <a href="#main-content" class="skip-link">Skip to main content</a>

    <!-- Sidebar -->
    <aside class="sidebar" :class="{ 'sidebar-open': sidebarOpen }" aria-label="Main navigation">
        <div class="sidebar-brand">
            <a href="/">
                <strong>AgentOS</strong>
            </a>
        </div>
        <nav>
            <ul class="sidebar-nav">
                <li><a href="/" data-nav="/"><span class="nav-icon" aria-hidden="true">&#9632;</span> Dashboard</a></li>
                <li><a href="/agents" data-nav="/agents"><span class="nav-icon" aria-hidden="true">&#9679;</span> Agents</a></li>
                <li><a href="/tasks" data-nav="/tasks"><span class="nav-icon" aria-hidden="true">&#9654;</span> Tasks</a></li>
                <li><a href="/tools" data-nav="/tools"><span class="nav-icon" aria-hidden="true">&#9881;</span> Tools</a></li>
                <li><a href="/secrets" data-nav="/secrets"><span class="nav-icon" aria-hidden="true">&#128274;</span> Secrets</a></li>
                <li><a href="/pipelines" data-nav="/pipelines"><span class="nav-icon" aria-hidden="true">&#8658;</span> Pipelines</a></li>
                <li><a href="/audit" data-nav="/audit"><span class="nav-icon" aria-hidden="true">&#128196;</span> Audit</a></li>
            </ul>
        </nav>
        <div class="sidebar-footer">
            <small class="muted">AgentOS Web UI</small>
        </div>
    </aside>

    <!-- Sidebar overlay for mobile -->
    <div class="sidebar-overlay" x-show="sidebarOpen" @click="sidebarOpen = false"
         x-transition:enter="fade-enter" x-transition:leave="fade-leave"></div>

    <!-- Main wrapper (topbar + content + footer) -->
    <div class="main-wrapper">
        <!-- Topbar -->
        <header class="topbar">
            <div class="topbar-left">
                <button class="topbar-hamburger" @click="sidebarOpen = !sidebarOpen"
                        aria-label="Toggle navigation" aria-expanded="false"
                        :aria-expanded="sidebarOpen.toString()">
                    &#9776;
                </button>
                {% if breadcrumbs %}
                <nav aria-label="Breadcrumb" class="breadcrumb-nav">
                    <ol class="breadcrumb">
                        {% for crumb in breadcrumbs %}
                        <li>
                            {% if crumb.href %}<a href="{{ crumb.href }}">{{ crumb.label }}</a>
                            {% else %}<span aria-current="page">{{ crumb.label }}</span>{% endif %}
                        </li>
                        {% endfor %}
                    </ol>
                </nav>
                {% endif %}
            </div>
            <div class="topbar-right" x-data="themeToggle()">
                <button class="topbar-btn" @click="toggle()" :title="dark ? 'Switch to light theme' : 'Switch to dark theme'" aria-label="Toggle theme">
                    <span x-text="dark ? '&#9788;' : '&#9790;'">&#9790;</span>
                </button>
            </div>
        </header>

        <!-- Main content -->
        <main id="main-content" class="main-content">
            {% block content %}{% endblock %}
        </main>

        <footer class="site-footer">
            <small class="muted">AgentOS Web UI</small>
        </footer>
    </div>

    <meta name="csrf-token" content="{{ csrf_token }}">
    <link rel="stylesheet" href="/static/css/pico.min.css">
    <link rel="stylesheet" href="/static/css/app.css">
    <script src="/static/js/htmx.min.js"></script>
    <script src="/static/js/alpine.min.js" defer></script>
    <script src="/static/js/app.js" defer></script>
    <script src="/static/js/csrf.js" defer></script>

    <script>
    (function () {
        var path = window.location.pathname;
        document.querySelectorAll('[data-nav]').forEach(function (a) {
            var href = a.getAttribute('data-nav');
            var active = href === '/' ? path === '/' : path === href || path.startsWith(href + '/');
            if (active) {
                a.classList.add('nav-active');
                a.setAttribute('aria-current', 'page');
            }
        });
    }());
    </script>
</body>
```

**Note:** Move `<meta>`, `<link>`, and `<script>` tags back into `<head>` where appropriate. The above shows the logical structure.

### 2. Add sidebar and topbar CSS to `app.css`

Open `crates/agentos-web/static/css/app.css`.

Add these sections (replacing the existing `/* -- Navigation -- */` section):

```css
/* ── App Shell ──────────────────────────────────────────── */
html, body { margin: 0; padding: 0; height: 100%; }

.skip-link {
    position: absolute;
    top: -40px;
    left: 0;
    padding: 0.5rem 1rem;
    background: var(--pico-primary);
    color: #fff;
    z-index: 1000;
    transition: top 0.2s;
}
.skip-link:focus { top: 0; }

/* ── Sidebar ────────────────────────────────────────────── */
.sidebar {
    position: fixed;
    top: 0;
    left: 0;
    bottom: 0;
    width: 240px;
    background: var(--pico-card-background-color, #fff);
    border-right: 1px solid var(--pico-muted-border-color, #e0e0e0);
    display: flex;
    flex-direction: column;
    z-index: 100;
    overflow-y: auto;
    transition: transform 0.25s ease;
}

.sidebar-brand {
    padding: 1.25rem 1rem;
    border-bottom: 1px solid var(--pico-muted-border-color, #e0e0e0);
}
.sidebar-brand a { text-decoration: none; font-size: 1.2rem; }

.sidebar-nav {
    list-style: none;
    margin: 0;
    padding: 0.5rem 0;
}
.sidebar-nav li { margin: 0; }
.sidebar-nav a {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.6rem 1rem;
    text-decoration: none;
    color: var(--pico-color);
    font-size: 0.9rem;
    border-left: 3px solid transparent;
    transition: background 0.15s, border-color 0.15s;
}
.sidebar-nav a:hover {
    background: var(--pico-table-row-stripped-background-color, #f5f5f5);
}
.sidebar-nav a.nav-active {
    color: var(--pico-primary);
    font-weight: 600;
    border-left-color: var(--pico-primary);
    background: color-mix(in srgb, var(--pico-primary) 8%, transparent);
}

.nav-icon { font-size: 1rem; width: 1.25rem; text-align: center; }

.sidebar-footer {
    margin-top: auto;
    padding: 1rem;
    border-top: 1px solid var(--pico-muted-border-color, #e0e0e0);
}

.sidebar-overlay {
    display: none;
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,0.4);
    z-index: 99;
}

/* ── Main Wrapper ───────────────────────────────────────── */
.main-wrapper {
    margin-left: 240px;
    min-height: 100vh;
    display: flex;
    flex-direction: column;
}

/* ── Topbar ─────────────────────────────────────────────── */
.topbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 1.5rem;
    border-bottom: 1px solid var(--pico-muted-border-color, #e0e0e0);
    background: var(--pico-card-background-color, #fff);
    position: sticky;
    top: 0;
    z-index: 50;
    min-height: 48px;
}
.topbar-left { display: flex; align-items: center; gap: 0.75rem; }
.topbar-right { display: flex; align-items: center; gap: 0.5rem; }
.topbar-hamburger {
    display: none; /* visible only on mobile */
    background: none;
    border: none;
    font-size: 1.5rem;
    cursor: pointer;
    padding: 0.25rem;
    color: var(--pico-color);
}
.topbar-btn {
    background: none;
    border: 1px solid var(--pico-muted-border-color);
    border-radius: var(--pico-border-radius);
    cursor: pointer;
    padding: 0.25rem 0.5rem;
    font-size: 1rem;
    color: var(--pico-color);
}

/* ── Breadcrumbs ────────────────────────────────────────── */
.breadcrumb {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    list-style: none;
    margin: 0;
    padding: 0;
    font-size: 0.85rem;
}
.breadcrumb li:not(:last-child)::after {
    content: "/";
    margin-left: 0.25rem;
    color: var(--pico-muted-color);
}
.breadcrumb a { text-decoration: none; color: var(--pico-muted-color); }
.breadcrumb a:hover { color: var(--pico-primary); }
.breadcrumb [aria-current="page"] { color: var(--pico-color); font-weight: 500; }

/* ── Main Content ───────────────────────────────────────── */
.main-content {
    flex: 1;
    padding: 1.5rem;
    max-width: 1200px;
    width: 100%;
    margin: 0 auto;
}

/* ── Responsive: Mobile ─────────────────────────────────── */
@media (max-width: 768px) {
    .sidebar {
        transform: translateX(-100%);
    }
    .sidebar.sidebar-open {
        transform: translateX(0);
    }
    .sidebar-overlay {
        display: block;
    }
    .main-wrapper {
        margin-left: 0;
    }
    .topbar-hamburger {
        display: block;
    }
    .main-content {
        padding: 1rem;
    }
}
```

### 3. Create `static/js/app.js` with theme toggle and keyboard shortcuts

Create a new file `crates/agentos-web/static/js/app.js`:

```javascript
// AgentOS Web UI — client-side utilities

// Theme toggle (Alpine.js component)
function themeToggle() {
    return {
        dark: localStorage.getItem('agentos-theme') === 'dark',
        toggle: function() {
            this.dark = !this.dark;
            document.documentElement.setAttribute(
                'data-theme', this.dark ? 'dark' : 'light'
            );
            localStorage.setItem('agentos-theme', this.dark ? 'dark' : 'light');
        },
        init: function() {
            if (this.dark) {
                document.documentElement.setAttribute('data-theme', 'dark');
            }
        }
    };
}

// Keyboard shortcuts (Alpine.js component)
function keyboardNav() {
    return {
        awaitingGoto: false,
        init: function() {
            var self = this;
            document.addEventListener('keydown', function(e) {
                var tag = e.target.tagName;
                if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || e.target.isContentEditable) return;

                if (e.key === 'g' && !e.ctrlKey && !e.metaKey && !e.altKey) {
                    self.awaitingGoto = true;
                    setTimeout(function() { self.awaitingGoto = false; }, 1000);
                    return;
                }
                if (self.awaitingGoto) {
                    self.awaitingGoto = false;
                    var routes = {
                        d: '/', a: '/agents', t: '/tasks', o: '/tools',
                        s: '/secrets', p: '/pipelines', l: '/audit'
                    };
                    if (routes[e.key]) {
                        e.preventDefault();
                        window.location.href = routes[e.key];
                    }
                }
            });
        }
    };
}
```

### 4. Update all handler functions to pass `breadcrumbs`

Each handler must pass a `breadcrumbs` array to the template context. Example for `dashboard.rs`:

```rust
// In handlers/dashboard.rs index():
let ctx = context! {
    page_title => "Dashboard",
    breadcrumbs => vec![
        context! { label => "Dashboard" },
    ],
    csrf_token,
    // ... existing fields
};
```

For nested pages like task detail:

```rust
// In handlers/tasks.rs detail():
let ctx = context! {
    page_title => format!("Task {}", task.id),
    breadcrumbs => vec![
        context! { label => "Tasks", href => "/tasks" },
        context! { label => format!("Task {}", &task.id.to_string()[..8]) },
    ],
    // ... existing fields
};
```

Update all 7 handler files: `dashboard.rs`, `agents.rs`, `tasks.rs` (list + detail), `tools.rs`, `secrets.rs`, `pipelines.rs`, `audit.rs`.

### 5. Register `app.js` in the template engine (no change needed)

The `app.js` file is served from the `static/` directory via `ServeDir` in `router.rs`. No template engine changes needed -- just adding the `<script>` tag to `base.html`.

### 6. Remove old `nav#main-nav` styling from `app.css`

Remove the old navigation styles:
```css
/* REMOVE: */
.brand { text-decoration: none; }
nav a.nav-active { ... }
```

These are replaced by the `.sidebar-nav a.nav-active` styles above.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/base.html` | Replace horizontal nav with sidebar + topbar shell layout |
| `crates/agentos-web/static/css/app.css` | Replace nav styles with sidebar/topbar/breadcrumb/responsive styles |
| `crates/agentos-web/static/js/app.js` | **New file** -- theme toggle + keyboard shortcuts (Alpine.js components) |
| `crates/agentos-web/src/handlers/dashboard.rs` | Add `breadcrumbs` to template context |
| `crates/agentos-web/src/handlers/agents.rs` | Add `breadcrumbs` to template context |
| `crates/agentos-web/src/handlers/tasks.rs` | Add `breadcrumbs` to template context (list + detail) |
| `crates/agentos-web/src/handlers/tools.rs` | Add `breadcrumbs` to template context |
| `crates/agentos-web/src/handlers/secrets.rs` | Add `breadcrumbs` to template context |
| `crates/agentos-web/src/handlers/pipelines.rs` | Add `breadcrumbs` to template context |
| `crates/agentos-web/src/handlers/audit.rs` | Add `breadcrumbs` to template context |

---

## Dependencies

None -- this is the foundation phase.

---

## Test Plan

- `cargo build -p agentos-web` must compile without errors
- `cargo test -p agentos-web` must pass (existing CSRF tests unaffected)
- `cargo clippy -p agentos-web -- -D warnings` must pass
- Manual verification:
  - Load each page (`/`, `/agents`, `/tasks`, `/tools`, `/secrets`, `/pipelines`, `/audit`) and confirm the sidebar renders correctly
  - Click each nav link and confirm the active state highlights correctly
  - Resize browser to < 768px and confirm the sidebar collapses and the hamburger button works
  - Open a task detail page and confirm the breadcrumb shows `Tasks > Task abc123`
  - Toggle theme and confirm dark/light switch persists across page loads
  - Press `g` then `a` to navigate to Agents via keyboard shortcut
  - Press `Tab` and confirm the skip link appears and jumps to `#main-content`

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
cargo clippy -p agentos-web -- -D warnings
cargo fmt -p agentos-web -- --check
```
