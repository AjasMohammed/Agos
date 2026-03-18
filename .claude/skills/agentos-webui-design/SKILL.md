---
name: agentos-webui-design
description: "Design and implement UI for the AgentOS web interface using its exact frontend stack: Pico CSS v2.1.1 (classless, semantic HTML), HTMX for server-driven interactivity, Alpine.js for component state, and MiniJinja2 for templating. Use this skill whenever the user asks to add, redesign, or improve any page, component, partial, or visual element in `crates/agentos-web/`. Use it even for small changes like \"add a button\", \"make this look better\", \"add an empty state\", or \"show a status indicator\". The skill enforces concrete design principles that produce polished, purposeful UIs — not generic boilerplate."
---

# AgentOS Web UI Design Skill

You are designing UI for AgentOS — a minimalist, LLM-native OS control panel. The UI must feel **purposeful and precise**, not like a generic admin dashboard thrown together with components. Every visual choice should reflect what this product actually is: a runtime for AI agents.

## Stack — non-negotiable

| Layer | Tech | Version | Location |
|-------|------|---------|----------|
| CSS framework | Pico CSS | v2.1.1 | `/static/css/pico.min.css` |
| Custom styles | app.css | — | `/static/css/app.css` |
| Interactivity | HTMX | bundled | `/static/js/htmx.min.js` |
| State | Alpine.js | bundled | `/static/js/alpine.min.js` |
| Templates | MiniJinja2 | 2.x | embedded via `include_str!()` |
| Server | Axum | 0.8 | Rust handlers in `src/handlers/` |

**Do not introduce any external CDN links, npm packages, new CSS frameworks, Tailwind, or inline `<style>` blocks.** All new styles go into `app.css`.

---

## File locations

```
crates/agentos-web/
├── src/
│   ├── templates/          ← MiniJinja .html templates
│   │   ├── base.html       ← master layout (nav, main, footer)
│   │   ├── *.html          ← one file per page
│   │   └── partials/       ← reusable fragments for HTMX swaps
│   └── handlers/           ← Rust handlers (render context data here)
└── static/
    ├── css/app.css         ← ALL new custom CSS goes here
    └── js/                 ← HTMX, Alpine, csrf.js (do not modify)
```

---

## Pico CSS essentials

Pico is **classless** — it styles semantic HTML directly. Use semantic elements first; reach for utility classes only when needed.

### Semantic elements Pico styles automatically
```html
<article>             <!-- card with padding, border-radius, shadow -->
<header> / <footer>   <!-- inside article: styled card header/footer -->
<nav>                 <!-- horizontal navigation bar -->
<figure><table>       <!-- responsive table wrapper -->
<details><summary>    <!-- native collapsible (styled by Pico) -->
<dialog>              <!-- modal dialog (Alpine toggles :open) -->
<mark>                <!-- highlighted text -->
<kbd>                 <!-- keyboard shortcut display -->
```

### Layout classes Pico provides
```html
<main class="container">          <!-- max-width centered, padded -->
<nav class="container-fluid">     <!-- full-width -->
<div class="grid">                <!-- auto-fit responsive grid -->
<div class="grid" style="--pico-grid-column-gap: 1rem"> <!-- custom gap -->
```

### Form elements — use label wrappers, not floating labels
```html
<label for="name">Label text
    <input type="text" id="name" name="name" required placeholder="...">
</label>
<label for="type">
    <select id="type" name="type">
        <option value="">Choose...</option>
    </select>
</label>
```

### Buttons
```html
<button>Primary action</button>
<button class="outline">Secondary</button>
<button class="outline secondary">Muted</button>
<button class="btn-sm outline secondary">Small</button>
<!-- Role=button on links -->
<a href="/somewhere" role="button" class="outline">Navigate</a>
```

### Pico CSS variables (use these, don't hardcode colors)
```css
var(--primary)                       /* brand color (blue) */
var(--muted-color)                   /* gray text */
var(--card-background-color)         /* card bg */
var(--code-background-color)         /* dark bg for pre/code */
var(--table-border-color)            /* subtle borders */
var(--table-row-stripped-background-color) /* hover/stripe rows */
```

---

## Custom classes already in app.css

Use these before writing new CSS.

| Class | Use |
|-------|-----|
| `.page-header` | Flex row with title (h1) + meta info. Wraps on mobile. |
| `.page-meta` | Muted right-side text in page-header |
| `.muted` | `color: var(--muted-color)` |
| `.btn-sm` | Small button padding (combine with `.outline`) |
| `.filter-bar` | Flex row with input + selects + buttons for filtering |
| `.badge` | Status indicator chip |
| `.badge-pending/.badge-queued` | Yellow |
| `.badge-running/.badge-in-progress` | Cyan |
| `.badge-completed/.badge-complete/.badge-success` | Green |
| `.badge-failed/.badge-error` | Red |
| `.badge-active` | Green |
| `.badge-inactive/.badge-disconnected` | Gray |
| `.badge-warning` | Yellow |
| `.badge-critical` | Red |
| `.badge-info` | Cyan |
| `.badge-lg` | Larger badge variant |
| `.clickable` | Pointer cursor + hover bg on `<tr>` |
| `.detail-list` | 2-col `<dl>` grid for key/value metadata |
| `.log-terminal` | Dark terminal block (GitHub-style) |
| `.log-line/.log-error/.log-warn/.log-success/.log-tool` | Terminal line colors |
| `.context-window/.context-entry` | Collapsible message container |
| `.role-badge/.role-user/.role-assistant/.role-system/.role-tool` | LLM role chips |
| `.audit-table` | Compact audit log table |

---

## Design principles

### 1. Information density with breathing room

Every page should answer: "What does the user need to know right now?" Show the most important state first, then details on demand.

- Use `<details>/<summary>` for secondary information (task details, JSON blobs)
- Show counts and statuses inline — don't make users navigate away to know things
- Use `.page-header` on every page: `<h1>` on the left, count/meta on the right

```html
<div class="page-header">
    <div>
        <h1>Agents</h1>
        <p class="page-meta">{{ agents|length }} connected</p>
    </div>
    <button @click="showModal = true">Connect Agent</button>
</div>
```

### 2. Status is always visible

Running systems change state constantly. Every entity (agent, task, tool, pipeline) must display its status badge inline, with the right semantic color. Never show a raw string where a badge belongs.

```html
<!-- Good -->
<span class="badge badge-{{ agent.status|lower }}">{{ agent.status }}</span>

<!-- Bad -->
<span>{{ agent.status }}</span>
```

### 3. Empty states are first-class UI

An empty list is a UI moment — it tells the user what to do next. Never show an empty `<div class="grid">` or a blank table.

```html
{% if agents|length == 0 %}
<div style="text-align: center; padding: 3rem 1rem; color: var(--muted-color);">
    <p style="font-size: 1.5rem; margin-bottom: 0.5rem;">No agents connected</p>
    <p>Connect an LLM agent to start processing tasks.</p>
    <button @click="showModal = true" style="margin-top: 1rem;">Connect your first agent</button>
</div>
{% else %}
<div id="agent-grid" class="grid">
    {% include "partials/agent_card.html" %}
</div>
{% endif %}
```

### 4. Live data, not stale snapshots

Use HTMX polling for anything that changes while the user watches. Dashboard stats, agent lists, task tables — all should refresh automatically. Choose the right interval:

- Agent/task lists: `every 5s`
- Dashboard counts: `every 5s`
- Audit log: `every 10s`
- Task detail metadata: `every 3s` (while running)

```html
<tbody hx-get="/tasks?partial=list" hx-trigger="every 5s" hx-swap="innerHTML">
```

### 5. Actions are contextual and immediate

Destructive actions (delete, disconnect, revoke) belong in the row/card they affect, not a separate page. Use HTMX `hx-delete` + `hx-confirm` + `hx-target="closest article"` to remove the element instantly.

```html
<button class="outline secondary btn-sm"
        hx-delete="/agents/{{ agent.name }}"
        hx-confirm="Disconnect {{ agent.name }}?"
        hx-target="closest article"
        hx-swap="outerHTML swap:0.2s">Disconnect</button>
```

### 6. Progressive disclosure in tables

Tables with many columns get overwhelming. Put the critical columns first (name, status, timestamp), and put verbose detail (IDs, JSON, prompts) behind `<details>`:

```html
<td class="details-cell">
    <details>
        <summary class="details-preview">{{ entry.details|truncate(60) }}</summary>
        <pre class="details-full">{{ entry.details }}</pre>
    </details>
</td>
```

### 7. Cards for entities, tables for events

- Use `<article>` cards for **entities** (agents, tools) — things with rich state and actions
- Use `<figure><table>` for **events/lists** (tasks, audit entries, secrets, pipelines) — ordered data with many rows

Cards give breathing room for rich content. Tables give density for high-volume data.

### 8. Modals for creation flows, not navigation

Use Alpine dialogs for "Connect Agent", "Add Secret", "Run Pipeline" — any multi-field creation form that shouldn't navigate away. Keep the dialog focused: one action, one form, one submit button. Always include a close button and `@close="showModal = false"`.

```html
<div x-data="{ showModal: false }">
    <button @click="showModal = true">New Item</button>
    <dialog :open="showModal" @close="showModal = false">
        <article>
            <header>
                <button aria-label="Close" rel="prev" @click="showModal = false"></button>
                <h3>Create Item</h3>
            </header>
            <form hx-post="/items" hx-target="#items-list" hx-swap="innerHTML"
                  @htmx:after-request="showModal = false">
                <input type="hidden" name="_csrf" value="{{ csrf_token }}">
                <!-- fields -->
                <button type="submit">Create</button>
            </form>
        </article>
    </dialog>
</div>
```

### 9. Metadata uses dl.detail-list, not ad-hoc paragraphs

When showing key/value metadata (task detail, agent info), always use the `.detail-list` pattern — it gives a clean 2-column grid aligned at the label:

```html
<dl class="detail-list">
    <dt>Status</dt>
    <dd><span class="badge badge-{{ task.status|lower }}">{{ task.status }}</span></dd>
    <dt>Created</dt>
    <dd>{{ task.created_at }}</dd>
    <dt>Agent</dt>
    <dd>{{ task.agent_id|default("—", true) }}</dd>
    <dt>Model</dt>
    <dd><code>{{ task.model|default("—", true) }}</code></dd>
</dl>
```

### 10. Responsive by default — test your grid

Pico's `.grid` is `auto-fit` with `minmax(200px, 1fr)`. For cards that need a specific minimum, add a `min-width` hint via CSS. For tables, wrap in `<figure>` so Pico adds horizontal scrolling on mobile.

```html
<!-- Cards auto-collapse on mobile -->
<div class="grid">...</div>

<!-- Tables scroll on mobile -->
<figure>
    <table>...</table>
</figure>
```

---

## HTMX patterns

### Filter bar with live search
```html
<div class="filter-bar">
    <input type="text" name="q" placeholder="Filter..."
           hx-get="/items" hx-trigger="keyup changed delay:300ms"
           hx-target="#items-list" hx-swap="innerHTML"
           hx-include="[name='limit']">
    <select name="limit" hx-get="/items" hx-trigger="change"
            hx-target="#items-list" hx-swap="innerHTML">
        <option value="25">25</option>
        <option value="50">50</option>
        <option value="100">100</option>
    </select>
</div>
```

### Partial renders — always use `?partial=list`
Handlers check `Query<Params>` for `partial=list` and return only the inner fragment. HTMX targets the inner container so the filter bar and page header are preserved.

```html
<!-- Full page renders on GET /agents -->
<!-- HTMX updates use hx-get="/agents?partial=list" hx-target="#agent-grid" -->
<div id="agent-grid" class="grid"
     hx-get="/agents?partial=list" hx-trigger="every 5s" hx-swap="innerHTML">
    {% include "partials/agent_card.html" %}
</div>
```

### HTMX loading indicators
```html
<span aria-busy="true" htmx-indicator style="display:none">Loading...</span>
```

Or use Pico's `aria-busy` on the container:
```html
<div id="content" hx-get="/data" hx-trigger="load" aria-busy="true">
```

### Confirm before destructive actions
```html
hx-confirm="Are you sure you want to delete {{ item.name }}? This cannot be undone."
```

---

## Alpine.js patterns

### Modal toggle (standard pattern)
```html
<div x-data="{ showModal: false }">
    <button @click="showModal = true">Open</button>
    <dialog :open="showModal" @close="showModal = false">
        <article>
            <header>
                <button aria-label="Close" rel="prev" @click="showModal = false"></button>
                <h3>Title</h3>
            </header>
            <!-- content -->
        </article>
    </dialog>
</div>
```

### Show/hide a form section
```html
<div x-data="{ expanded: false }">
    <button class="outline" @click="expanded = !expanded" x-text="expanded ? 'Cancel' : 'Add New'">
        Add New
    </button>
    <div x-show="expanded" x-transition>
        <form>...</form>
    </div>
</div>
```

### Dynamic select values (e.g., populate model name based on provider)
```html
<div x-data="{ provider: 'ollama', modelPlaceholder: 'llama3.2' }"
     @change="if($event.target.name==='provider') { provider=$event.target.value; }">
    <select name="provider" x-model="provider">
        <option value="ollama">Ollama</option>
        <option value="openai">OpenAI</option>
        <option value="anthropic">Anthropic</option>
    </select>
    <input type="text" name="model" :placeholder="provider === 'openai' ? 'gpt-4o' : provider === 'anthropic' ? 'claude-opus-4-6' : 'llama3.2'">
</div>
```

---

## MiniJinja templating

### Template inheritance
```html
{% extends "base.html" %}
{% block content %}
<!-- page content here -->
{% endblock %}
```

### Including partials
```html
{% include "partials/agent_card.html" %}
```

### Useful filters
```
{{ value|default("—", true) }}        <!-- show dash if null/empty -->
{{ value|lower }}                      <!-- lowercase for badge classes -->
{{ value|length }}                     <!-- count items -->
{{ list|join(", ") }}                  <!-- join list -->
{{ text|truncate(80) }}                <!-- truncate string -->
```

### Conditional rendering
```html
{% if items|length == 0 %}
    <!-- empty state -->
{% else %}
    {% for item in items %}
        <!-- render item -->
    {% endfor %}
{% endif %}
```

---

## Adding new CSS

When you need styles not already in `app.css`, add them to `app.css` using Pico's CSS variables. Do not hardcode hex values that Pico already defines. Follow the existing sectioning pattern:

```css
/* ── New Section ─────────────────────────────────────────── */
.my-new-class {
    /* use var(--primary), var(--muted-color), etc. */
}
```

---

## Security requirements (always)

- Every `<form>` must include: `<input type="hidden" name="_csrf" value="{{ csrf_token }}">`
- Never put user-provided data into `hx-vals`, `hx-headers`, or JS without escaping — MiniJinja auto-escapes in `{{ }}` blocks
- Do not add `'unsafe-inline'` scripts — the CSP blocks them; use `defer` scripts only from `/static/js/`

---

## Common page patterns

### Standard list page with filter + action

```html
{% extends "base.html" %}
{% block content %}
<div class="page-header">
    <div>
        <h1>Items</h1>
        <p class="page-meta">{{ items|length }} total</p>
    </div>
    <button @click="showModal = true">Add Item</button>
</div>

<div class="filter-bar">
    <input type="text" name="q" placeholder="Search..."
           hx-get="/items" hx-trigger="keyup changed delay:300ms"
           hx-target="#items-list" hx-swap="innerHTML">
</div>

<figure>
<table>
    <thead>
        <tr>
            <th>Name</th>
            <th>Status</th>
            <th>Created</th>
            <th></th>
        </tr>
    </thead>
    <tbody id="items-list"
           hx-get="/items?partial=list" hx-trigger="every 5s" hx-swap="innerHTML">
        {% for item in items %}
        <tr class="clickable" onclick="location.href='/items/{{ item.id }}'">
            <td><strong>{{ item.name }}</strong></td>
            <td><span class="badge badge-{{ item.status|lower }}">{{ item.status }}</span></td>
            <td><code class="muted">{{ item.created_at }}</code></td>
            <td>
                <button class="btn-sm outline secondary"
                        hx-delete="/items/{{ item.id }}"
                        hx-confirm="Delete {{ item.name }}?"
                        hx-target="closest tr"
                        hx-swap="outerHTML">Delete</button>
            </td>
        </tr>
        {% else %}
        <tr>
            <td colspan="4" style="text-align:center; color:var(--muted-color); padding:2rem;">
                No items yet.
            </td>
        </tr>
        {% endfor %}
    </tbody>
</table>
</figure>
{% endblock %}
```

### Card grid page

```html
<div id="card-grid" class="grid"
     hx-get="/agents?partial=list" hx-trigger="every 5s" hx-swap="innerHTML">
    {% for agent in agents %}
    <article>
        <header>
            <strong>{{ agent.name }}</strong>
            <span class="badge badge-{{ agent.status|lower }}">{{ agent.status }}</span>
        </header>
        <dl class="detail-list">
            <dt>Provider</dt><dd>{{ agent.provider }}</dd>
            <dt>Model</dt><dd><code>{{ agent.model }}</code></dd>
        </dl>
        <footer>
            <small class="muted">Since {{ agent.created_at }}</small>
            <button class="btn-sm outline secondary"
                    hx-delete="/agents/{{ agent.name }}"
                    hx-confirm="Disconnect {{ agent.name }}?"
                    hx-target="closest article"
                    hx-swap="outerHTML">Disconnect</button>
        </footer>
    </article>
    {% else %}
    <!-- empty state: shown when no agents -->
    <div style="grid-column:1/-1; text-align:center; padding:3rem 1rem; color:var(--muted-color);">
        <p style="font-size:1.25rem;">No agents connected</p>
        <p>Use the button above to connect your first agent.</p>
    </div>
    {% endfor %}
</div>
```

### Detail page with metadata + collapsible sections

```html
<div class="page-header">
    <div>
        <h1>Task Detail</h1>
        <code class="muted id-long">{{ task.id }}</code>
    </div>
    <span class="badge badge-lg badge-{{ task.status|lower }}">{{ task.status }}</span>
</div>

<article>
    <header>Metadata</header>
    <dl class="detail-list">
        <dt>Agent</dt><dd>{{ task.agent_id|default("—", true) }}</dd>
        <dt>Created</dt><dd>{{ task.created_at }}</dd>
        <dt>Model</dt><dd><code>{{ task.model|default("—", true) }}</code></dd>
    </dl>
</article>

<article>
    <header>Prompt</header>
    <pre class="prompt-pre">{{ task.prompt }}</pre>
</article>

<!-- Live-refreshing section while task runs -->
{% if task.status == "running" or task.status == "pending" %}
<article hx-get="/tasks/{{ task.id }}?partial=status"
         hx-trigger="every 3s" hx-swap="outerHTML">
    <header>Live Status <span aria-busy="true"></span></header>
    <!-- status content -->
</article>
{% endif %}
```

---

## What to avoid

| Anti-pattern | Why | Do instead |
|---|---|---|
| `<div class="card">` | Not a Pico class; meaningless | Use `<article>` |
| Custom CSS component system (`.stat-card`, `.stat-number`, etc.) | Pico's `<article><header><footer>` already is a card — creating 10+ new classes to recreate it fights the framework | Use semantic HTML; add minimal CSS only when Pico genuinely doesn't cover it |
| Raw `{{ status }}` string | No visual meaning | `<span class="badge badge-{{ status\|lower }}">{{ status }}</span>` |
| Empty list without message | User thinks it's broken | Add `{% else %}` empty state |
| Inline `<script>` for filtering/search | Breaks the server-driven model; creates split logic; CSP will block it | Use HTMX `hx-trigger="keyup changed delay:300ms"` with server-side filtering |
| `<style>` blocks in templates | Bypasses CSP, hard to maintain | Add to `app.css` |
| Hardcoded hex colors in HTML | Breaks dark mode / theming | Use Pico vars |
| Multiple HTMX calls on same trigger | Race conditions | Combine into one `hx-include` |
| Loading all data always | Slow pages | Use `?partial=list` pattern |
| Separate confirmation pages | Interrupts flow | `hx-confirm` attribute |
| Generic button labels ("Submit") | Unclear intent | Use action verbs ("Connect", "Revoke", "Run Pipeline") |
| Terse empty state text ("No agents found.") | Gives user no context or next step | Write 1-2 sentences explaining what the section does and what to do — mention the specific entities and actions involved |
