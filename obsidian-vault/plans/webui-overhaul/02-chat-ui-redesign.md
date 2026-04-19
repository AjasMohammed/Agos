---
title: Phase 02 — Claude-Code-Style Chat UI
tags:
  - webui
  - chat
  - ui
  - markdown
  - phase-02
date: 2026-04-11
status: planned
effort: 3d
priority: critical
---

# Phase 02 — Claude-Code-Style Chat UI

> Rebuild the chat conversation UI to feel like Claude Code: a unified timeline of thinking blocks, tool cards, and markdown-rendered messages, with inline syntax highlighting, collapsible tool details, copy-to-clipboard, message editing for the user, and a properly clearing reply input.

---

## Why this phase

The current chat UI is a vertical list of three distinct bubble styles (user / tool / assistant). It does not communicate the *narrative* of an agent reasoning across multiple iterations and tool invocations. After Phase 01 the streaming protocol can carry that narrative; this phase makes it visible.

The reference experience is Claude Code itself: thinking is collapsible, tool calls show name + duration + a pretty-printed result, code blocks have syntax highlighting and a copy button, the user's last message can be edited and re-submitted, and the reply input clears reliably after a successful send.

User-reported issues this phase closes:
- Markdown not rendered (closes via `marked.parse` already wired in Phase 01)
- Reply input not cleared on send
- "the chat feature in the frontend is not fully completed it has some issues"
- "for the chat page the ui should beautifully show the tools, thinking, etc created/called during that chat session"

## Current → Target State

| Concern | Current | Target |
|---------|---------|--------|
| Message timeline | flat user/tool/assistant rows | unified timeline mixing thinking blocks, tool cards, and markdown messages in chronological order |
| Markdown rendering | none | client-side via `marked` + `DOMPurify` + `highlight.js` (text rendered as it streams in Phase 01, server-rendered for stored messages) |
| Tool display | `<pre>{stringified JSON}</pre>` | structured card: name pill, status icon, duration, collapsible payload + result with `pretty_json` |
| Thinking display | none | collapsible "Thinking…" block per iteration with iteration number and elapsed time |
| Reply input clearing | unreliable `hx-on::after-request` | reliable using `htmx:afterOnLoad` event with `event.detail.xhr.status === 200` check |
| Code blocks | none | syntax highlighted with copy button |
| User message edit | not supported | hover toolbar with edit + retry buttons |
| Session management | list / create / open | list / create / **rename / delete / export / fork** |
| New session UX | synchronous LLM call blocks page load | redirect to `/chat/{id}` immediately after session row is inserted; chat page reconnects to in-flight stream |
| Empty state | text-only "No messages yet" | empty state with suggested prompts based on agent role |
| Token + cost meta | not shown | per-message footer with token count and cost (if available) |

## Detailed subtasks

### 1. New chat conversation template

Replace [src/templates/chat_conversation.html](crates/agentos-web/src/templates/chat_conversation.html) with a timeline-based layout. Render messages from a new `timeline` context variable (a `Vec<TimelineEntry>` produced by the handler — see Phase 03 for the storage migration).

```html
{% extends "base.html" %}
{% block content %}
<div class="chat-page" x-data="chatSession()">
    <header class="chat-page-header">
        <div>
            <h1>
                <span x-text="title" x-on:dblclick="startRename()"></span>
                <small class="muted">with {{ agent_name }} · {{ model }}</small>
            </h1>
        </div>
        <div class="chat-header-actions">
            <button class="outline secondary btn-sm" hx-get="/chat/{{ session_id }}/export"
                    hx-swap="none">Export</button>
            <button class="outline secondary btn-sm" hx-post="/chat/{{ session_id }}/fork"
                    hx-target="body" hx-push-url="true">Fork</button>
            <button class="outline secondary btn-sm" hx-delete="/chat/{{ session_id }}"
                    hx-confirm="Delete this session?" hx-target="body" hx-push-url="true">Delete</button>
            <a href="/chat" class="outline secondary btn-sm" role="button">← All Sessions</a>
        </div>
    </header>

    <div id="chat-messages-list" class="chat-timeline" role="log" aria-live="polite">
        {% if timeline|length == 0 %}
        {% include "partials/chat_empty_state.html" %}
        {% else %}
        {% for entry in timeline %}
            {% if entry.kind == "user" %}
                {% include "partials/chat_user_msg.html" %}
            {% elif entry.kind == "assistant" %}
                {% include "partials/chat_assistant_msg.html" %}
            {% elif entry.kind == "tool" %}
                {% include "partials/chat_tool_call.html" %}
            {% elif entry.kind == "thinking" %}
                {% include "partials/chat_thinking_block.html" %}
            {% endif %}
        {% endfor %}
        {% endif %}

        {% if needs_stream_reconnect %}
            {% include "partials/chat_stream_target.html" %}
        {% endif %}
    </div>

    <form id="chat-reply-form"
          class="chat-reply-form"
          hx-post="/chat/{{ session_id }}/send"
          hx-target="#chat-messages-list"
          hx-swap="beforeend"
          hx-disabled-elt="textarea, button">
        <input type="hidden" name="_csrf" value="{{ csrf_token }}">
        <textarea name="message"
                  rows="3"
                  placeholder="Reply to {{ agent_name }}…  (Cmd/Ctrl+Enter to send)"
                  required
                  x-on:keydown.meta.enter.prevent="$el.form.requestSubmit()"
                  x-on:keydown.ctrl.enter.prevent="$el.form.requestSubmit()"></textarea>
        <div class="chat-reply-actions">
            <small class="muted" x-text="charCount + ' chars'"></small>
            <button type="submit">Send →</button>
        </div>
    </form>
</div>

<script src="/static/js/marked.min.js"></script>
<script src="/static/js/dompurify.min.js"></script>
<script src="/static/js/highlight.min.js"></script>
<script src="/static/js/chat-page.js"></script>
{% endblock %}
```

### 2. Reply input clearing — the reliable fix

The current `hx-on::after-request` race is real. Replace with a top-level htmx event listener in `chat-page.js`:

```javascript
// crates/agentos-web/static/js/chat-page.js
document.body.addEventListener('htmx:afterRequest', function (e) {
    if (e.detail.elt && e.detail.elt.id === 'chat-reply-form'
        && e.detail.xhr && e.detail.xhr.status >= 200 && e.detail.xhr.status < 300) {
        e.detail.elt.reset();
        var ta = e.detail.elt.querySelector('textarea[name="message"]');
        if (ta) {
            ta.style.height = 'auto';
            ta.focus();
        }
    }
});
```

Why this works where the inline `hx-on` did not:
- `htmx:afterRequest` fires *after* the response body has been processed but before swap is complete, and `e.detail.xhr.status` is reliable at that point.
- The handler runs on `document.body`, so it survives form re-renders.
- Explicit status check beats `event.detail.successful` which has had bug history with redirects.

Also wire `Cmd+Enter` / `Ctrl+Enter` to submit (Alpine inline handlers in the template above).

### 3. Alpine `chatSession` component

Drives the rename/delete/export inline UX. Stays small — fetches no data, just toggles UI state and emits HTMX requests.

```javascript
function chatSession() {
    return {
        title: document.title,
        charCount: 0,
        renaming: false,
        startRename() {
            var newTitle = prompt('Rename session', this.title);
            if (newTitle && newTitle.trim()) {
                fetch('/chat/{{ session_id }}/rename', {
                    method: 'POST',
                    headers: {'Content-Type': 'application/x-www-form-urlencoded'},
                    body: 'title=' + encodeURIComponent(newTitle.trim())
                }).then(function (r) {
                    if (r.ok) location.reload();
                });
            }
        }
    };
}
```

### 4. New partial templates

Each timeline entry renders via its own partial so the look-and-feel can iterate independently.

#### `partials/chat_user_msg.html`

```html
<div class="chat-row chat-row-user" data-msg-id="{{ entry.id }}">
    <div class="chat-bubble chat-bubble-user">
        <div class="chat-bubble-content">{{ entry.content }}</div>
        <div class="chat-bubble-actions">
            <button class="chat-msg-action" title="Copy" data-clip="{{ entry.content|escape }}">⎘</button>
            <button class="chat-msg-action" title="Edit and resend"
                    hx-get="/chat/{{ session_id }}/edit/{{ entry.id }}"
                    hx-target="closest .chat-row"
                    hx-swap="outerHTML">✎</button>
        </div>
        <div class="chat-bubble-meta">{{ entry.created_at|relative_time }}</div>
    </div>
</div>
```

#### `partials/chat_assistant_msg.html`

```html
<div class="chat-row chat-row-agent" data-msg-id="{{ entry.id }}">
    <div class="chat-agent-avatar" aria-hidden="true">{{ agent_initial }}</div>
    <div class="chat-agent-column">
        <div class="chat-agent-name muted">{{ agent_name }}</div>
        <div class="chat-bubble chat-bubble-agent">
            <div class="chat-bubble-content-agent markdown-body">{{ entry.content|markdown }}</div>
            <div class="chat-bubble-meta">
                <span>{{ entry.created_at|relative_time }}</span>
                {% if entry.tokens_used %}<span>· {{ entry.tokens_used }} tokens</span>{% endif %}
                <button class="chat-msg-action" title="Copy"
                        data-clip="{{ entry.content|escape }}">⎘</button>
            </div>
        </div>
    </div>
</div>
```

The `markdown` filter is added in Phase 03 — it runs `pulldown-cmark` server-side. For *streamed* content the JS owns the rendering; for *stored* content the server renders once.

#### `partials/chat_tool_call.html`

```html
<div class="chat-tool-call" data-tool-id="{{ entry.id }}">
    <div class="chat-tool-card chat-tool-{{ 'done' if entry.success else 'error' }}">
        <div class="chat-tool-card-head">
            <span class="chat-tool-icon" aria-hidden="true">{{ '✓' if entry.success else '✗' }}</span>
            <span class="chat-tool-name">{{ entry.tool_name }}</span>
            <span class="chat-tool-intent muted">{{ entry.intent_type|default('') }}</span>
            <span class="chat-tool-status">{{ entry.duration_ms }}ms</span>
        </div>
        <details class="chat-tool-details">
            <summary class="muted">Payload</summary>
            <pre class="chat-tool-payload"><code class="language-json">{{ entry.payload_json|pretty_json }}</code></pre>
        </details>
        <details class="chat-tool-details" open>
            <summary class="muted">Result</summary>
            <pre class="chat-tool-result"><code class="language-json">{{ entry.result_json|pretty_json }}</code></pre>
        </details>
    </div>
</div>
```

#### `partials/chat_thinking_block.html`

```html
<details class="chat-thinking-block">
    <summary class="muted">
        <span class="chat-thinking-dot"></span>
        Thinking · iteration {{ entry.iteration }} · {{ entry.duration_ms }}ms
    </summary>
    <div class="chat-thinking-body">{{ entry.content|markdown }}</div>
</details>
```

#### `partials/chat_empty_state.html`

```html
<div class="chat-empty">
    <div class="chat-empty-icon">💭</div>
    <h3>Start a conversation with {{ agent_name }}</h3>
    <p class="muted">Try one of these:</p>
    <div class="chat-suggested-prompts">
        <button class="chat-suggested-prompt" data-prompt="What can you help me with?">What can you help me with?</button>
        <button class="chat-suggested-prompt" data-prompt="Show me my available tools.">Show me my available tools.</button>
        <button class="chat-suggested-prompt" data-prompt="Summarise the AgentOS architecture.">Summarise the AgentOS architecture.</button>
    </div>
</div>
```

Suggested-prompt clicks insert text into the reply textarea (handled in `chat-page.js`).

#### `partials/chat_stream_target.html`

The streaming target markup that Phase 01 expects (extracted into a partial so both `chat_conversation.html` and the `send()` partial can include it).

### 5. New chat CSS

Add `static/css/chat.css` (linked from `chat_conversation.html`). Approximate skeleton:

```css
.chat-timeline {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    padding: 1rem 0;
    max-height: calc(100vh - 280px);
    overflow-y: auto;
}

.chat-tool-card {
    border: 1px solid var(--pico-muted-border-color);
    border-radius: 0.5rem;
    background: var(--pico-card-background-color);
    margin-left: 2.75rem;
}
.chat-tool-card-head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid var(--pico-muted-border-color);
}
.chat-tool-icon { font-weight: bold; }
.chat-tool-done .chat-tool-icon { color: var(--pico-color-green-500); }
.chat-tool-error .chat-tool-icon { color: var(--pico-color-red-500); }
.chat-tool-running .chat-tool-icon { animation: spin 1s linear infinite; }

.chat-thinking-block {
    margin-left: 2.75rem;
    border-left: 2px solid var(--pico-muted-border-color);
    padding: 0.25rem 0 0.25rem 0.75rem;
    font-size: 0.9em;
}

.markdown-body pre {
    position: relative;
    background: var(--pico-code-background-color);
    border-radius: 0.375rem;
    padding: 0.75rem;
    overflow-x: auto;
}
.markdown-body pre code { background: transparent; }
.markdown-body pre .copy-btn {
    position: absolute;
    top: 0.25rem;
    right: 0.25rem;
    opacity: 0;
    transition: opacity 0.15s;
}
.markdown-body pre:hover .copy-btn { opacity: 1; }

@keyframes spin {
    from { transform: rotate(0deg); }
    to   { transform: rotate(360deg); }
}
```

(Final styling details land in Phase 02 implementation — this is a starting skeleton.)

### 6. New session creation no longer blocks

Today `new_session` calls `chat_infer_with_tools` synchronously. Replace with a streaming variant:

```rust
pub async fn new_session(...) -> Response {
    // 1. Validate
    // 2. Create session row
    let session_id = store.create_session_with_first_message(&agent, &msg)?;

    // 3. Reserve in-flight slot for the first message
    let inflight = state.inflight_chat.try_start(&session_id)?;

    // 4. Spawn detached inference task (same as send())
    tokio::spawn(...);

    // 5. Redirect to /chat/{session_id} — page will reconnect to /stream
    Redirect::to(&format!("/chat/{}", session_id)).into_response()
}
```

The conversation page will detect `needs_stream_reconnect` and start streaming via `chat-stream.js`.

### 7. Session rename / delete / fork / export

Add new routes:

```rust
.route("/chat/{session_id}/rename", post(chat::rename_session))
.route("/chat/{session_id}", delete(chat::delete_session))
.route("/chat/{session_id}/fork", post(chat::fork_session))
.route("/chat/{session_id}/export", get(chat::export_session))
.route("/chat/{session_id}/edit/{message_id}", get(chat::edit_message_form))
.route("/chat/{session_id}/edit/{message_id}", post(chat::edit_message_submit))
```

Each handler updates `chat_store` and returns either a small partial or a redirect with `HX-Trigger` toast.

`export_session` returns `Content-Disposition: attachment; filename="chat-{id}.md"` with the conversation rendered as a markdown transcript:

```
# Chat with mavrick (kimi-k2.5:cloud)
*Started 2026-04-11T11:25:49Z*

## You
What can you help me with?

## Assistant
[markdown content]

### Tool: web-search (1.2s)
**Payload:** ...
**Result:** ...
```

### 8. Sessions list page polish

Update [src/templates/chat.html](crates/agentos-web/src/templates/chat.html):
- Show last message preview snippet (first 80 chars of last assistant message, plain text)
- Show tool count badge
- Show updated_at as relative time
- Add inline rename + delete buttons
- Add a search box (filters client-side via Alpine)

## Files changed

| File | Change |
|------|--------|
| [crates/agentos-web/src/templates/chat_conversation.html](crates/agentos-web/src/templates/chat_conversation.html) | Full rewrite — timeline layout |
| [crates/agentos-web/src/templates/chat.html](crates/agentos-web/src/templates/chat.html) | Add preview/delete/rename/search |
| [crates/agentos-web/src/templates/partials/chat_user_msg.html](crates/agentos-web/src/templates/partials/chat_user_msg.html) | NEW |
| [crates/agentos-web/src/templates/partials/chat_assistant_msg.html](crates/agentos-web/src/templates/partials/chat_assistant_msg.html) | NEW |
| [crates/agentos-web/src/templates/partials/chat_tool_call.html](crates/agentos-web/src/templates/partials/chat_tool_call.html) | NEW |
| [crates/agentos-web/src/templates/partials/chat_thinking_block.html](crates/agentos-web/src/templates/partials/chat_thinking_block.html) | NEW |
| [crates/agentos-web/src/templates/partials/chat_empty_state.html](crates/agentos-web/src/templates/partials/chat_empty_state.html) | NEW |
| [crates/agentos-web/src/templates/partials/chat_stream_target.html](crates/agentos-web/src/templates/partials/chat_stream_target.html) | NEW |
| [crates/agentos-web/src/handlers/chat.rs](crates/agentos-web/src/handlers/chat.rs) | Pass `timeline: Vec<TimelineEntry>` instead of `messages`; new handlers for rename/delete/fork/export/edit; non-blocking new_session |
| [crates/agentos-web/src/router.rs](crates/agentos-web/src/router.rs) | New routes for rename/delete/fork/export/edit |
| [crates/agentos-web/static/js/chat-page.js](crates/agentos-web/static/js/chat-page.js) | NEW — Alpine component, reply form clearer, copy-clip handlers, suggested prompts |
| [crates/agentos-web/static/css/chat.css](crates/agentos-web/static/css/chat.css) | NEW |
| [crates/agentos-web/src/templates/base.html](crates/agentos-web/src/templates/base.html) | Link `chat.css` only on chat pages via a `{% block extra_css %}` |

## Dependencies

- Requires: [[01-chat-streaming-engine]] (frame protocol, marked.js vendoring)
- Requires: [[03-template-rendering-fixes]] (`markdown` and `pretty_json` MiniJinja filters)
- Independent of: 04, 05, 06, 07

## Test plan

1. **Unit: chat-page.js reply form clearing**
   - Use a headless browser test (or jsdom) — POST to `/chat/{id}/send`, simulate 200 response, assert textarea value is empty afterwards.
2. **Integration: timeline rendering**
   - Seed a session with: 2 user messages, 1 assistant message, 2 tool calls (1 success, 1 error), in chronological order.
   - GET `/chat/{id}`, assert HTML contains all entries in correct order, assistant message renders markdown headers/lists, tool cards have correct success class.
3. **Integration: rename / delete / fork**
   - POST `/chat/{id}/rename` with `title=foo`, GET `/chat/{id}`, assert `<title>` contains "foo".
   - DELETE `/chat/{id}`, assert 200, GET `/chat`, assert session not in list.
   - POST `/chat/{id}/fork`, assert response 302 to a new session ID, GET that ID, assert messages are duplicated.
4. **Integration: export**
   - GET `/chat/{id}/export`, assert `Content-Type: text/markdown`, body contains "# Chat with".
5. **Manual: keyboard shortcut**
   - Type in textarea, press Cmd+Enter (or Ctrl+Enter), assert form submits.
6. **Manual: copy-to-clipboard**
   - Click copy button on a code block, assert clipboard content matches code text.

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web chat
cargo clippy -p agentos-web -- -D warnings
cargo fmt --all -- --check

# Manual smoke
docker compose restart agentos-kernel
# Open http://localhost:8080/chat in a browser, create a new session, ask a markdown-heavy question
# Expected: streaming visible, code blocks highlighted, copy button works, reply input clears after send
```

## Related

- [[WebUI Overhaul Plan]]
- [[01-chat-streaming-engine]]
- [[03-template-rendering-fixes]]
