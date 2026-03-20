---
title: Rewrite Chat Conversation Template with HTMX
tags:
  - web
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 2d
priority: high
---

# Rewrite Chat Conversation Template with HTMX

> Rewrite `chat_conversation.html` to use HTMX for form submission and SSE event handling, create a `partials/chat_message.html` partial for message rendering, and add JavaScript to handle streaming events (thinking dots, tool activity indicators, final answer rendering).

---

## Why This Subtask

After subtask 28-03, the backend streams `ChatStreamEvent` values via SSE at `/chat/{session_id}/stream`. The frontend needs to connect to this stream and render events as they arrive. The current template at `crates/agentos-web/src/templates/chat_conversation.html` uses a static form POST with redirect -- it needs a complete rewrite.

The CSS classes for chat activity indicators already exist in `crates/agentos-web/static/css/app.css` (lines 855-937): `.chat-activity`, `.chat-activity-tool`, `.chat-activity-done`, `.chat-activity-error`, `.chat-thinking`, `.chat-thinking-dots`, `.chat-done-banner`.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Form submission | `<form method="post" action="/chat/{id}/send">` with full page reload | `<form hx-post="/chat/{id}/send" hx-target="#chat-stream-area" hx-swap="innerHTML">` |
| Response rendering | All messages rendered server-side on page load | Existing messages rendered server-side; new response streamed client-side via SSE |
| Thinking indicator | None | `.chat-thinking` with animated dots appears immediately after send |
| Tool call display | Raw JSON in assistant bubble | `.chat-activity-tool` collapsible `<details>` elements between bubbles |
| Tool role messages | Not rendered differently from assistant | Rendered as compact activity rows with tool name, duration, expand/collapse |
| Auto-scroll | None | JavaScript scrolls to bottom on new content |
| Enter-to-send | None | Enter submits, Shift+Enter adds newline |
| Button state | Always enabled | Disabled during inference, re-enabled on `chat-done` or `chat-error` |

---

## What to Do

### Step 1: Create `partials/chat_message.html`

Create the file `crates/agentos-web/src/templates/partials/chat_message.html`:

```html
{% if msg.role == "user" %}
<div class="chat-row chat-row-user">
    <div class="chat-bubble chat-bubble-user">
        <div class="chat-bubble-content">{{ msg.content }}</div>
        <div class="chat-bubble-meta">You &middot; {{ msg.created_at }}</div>
    </div>
</div>

{% elif msg.role == "tool" %}
<div class="chat-row">
    <details class="chat-activity chat-activity-done" style="width:100%; max-width:80%;">
        <summary style="cursor:pointer;">
            <span class="chat-activity-icon" aria-hidden="true">&#9881;</span>
            <span class="chat-activity-label">
                <strong>{{ msg.tool_name }}</strong>
                {% if msg.tool_duration_ms %}
                <small class="muted">({{ msg.tool_duration_ms }}ms)</small>
                {% endif %}
            </span>
        </summary>
        <pre style="font-size:0.78rem; max-height:200px; overflow:auto; margin:0.5rem 0 0; white-space:pre-wrap; word-break:break-word;">{{ msg.content }}</pre>
    </details>
</div>

{% else %}
<div class="chat-row chat-row-agent">
    <div class="chat-agent-avatar" aria-hidden="true">{{ agent_name[:1]|upper }}</div>
    <div class="chat-agent-column">
        <div class="chat-agent-name muted">{{ agent_name }}</div>
        <div class="chat-bubble chat-bubble-agent">
            <div class="chat-bubble-content-agent">{{ msg.content }}</div>
            <div class="chat-bubble-meta chat-bubble-meta-left">{{ msg.created_at }}</div>
        </div>
    </div>
</div>
{% endif %}
```

### Step 2: Register the partial

Open `crates/agentos-web/src/templates.rs`. Add after line 35:

```rust
env.add_template(
    "partials/chat_message.html",
    include_str!("templates/partials/chat_message.html"),
)?;
```

### Step 3: Rewrite `chat_conversation.html`

Replace `crates/agentos-web/src/templates/chat_conversation.html` with:

```html
{% extends "base.html" %}
{% block content %}

<div class="page-header">
    <div>
        <h1>Chat <code class="id-long">{{ session_id[:8] }}</code></h1>
        <p class="page-meta">with <strong>{{ agent_name }}</strong></p>
    </div>
    <a href="/chat" role="button" class="outline secondary btn-sm">&larr; Chat</a>
</div>

<div id="chat-messages" class="chat-conversation-area">
    {% for msg in messages %}
        {% include "partials/chat_message.html" %}
    {% endfor %}
</div>

<div id="chat-stream-area"></div>

<div class="chat-reply-area">
    <form id="chat-form"
          hx-post="/chat/{{ session_id }}/send"
          hx-target="#chat-stream-area"
          hx-swap="innerHTML"
          hx-on::before-request="document.getElementById('chat-send-btn').disabled=true"
          hx-on::after-request="this.reset();">
        <input type="hidden" name="_csrf" value="{{ csrf_token }}">
        <label for="reply-message">Continue the conversation
            <textarea id="reply-message" name="message" rows="3"
                      placeholder="Reply to {{ agent_name }}..."
                      onkeydown="if(event.key==='Enter'&&!event.shiftKey){event.preventDefault();this.form.requestSubmit();}"></textarea>
        </label>
        <div style="display: flex; justify-content: flex-end;">
            <button type="submit" id="chat-send-btn">Send &rarr;</button>
        </div>
    </form>
</div>

<script>
(function() {
    var agentInitial = '{{ agent_name[:1]|upper }}';
    var agentName = '{{ agent_name }}';

    function scrollToBottom() {
        var el = document.getElementById('chat-stream-area');
        if (el && el.lastElementChild) {
            el.lastElementChild.scrollIntoView({ behavior: 'smooth', block: 'end' });
        } else {
            var msgs = document.getElementById('chat-messages');
            if (msgs && msgs.lastElementChild) {
                msgs.lastElementChild.scrollIntoView({ behavior: 'smooth', block: 'end' });
            }
        }
    }

    function escapeHtml(t) {
        var d = document.createElement('div');
        d.textContent = t;
        return d.innerHTML;
    }

    // Handle all SSE events from the chat stream
    document.body.addEventListener('htmx:sseMessage', function(evt) {
        var data;
        try { data = JSON.parse(evt.detail.data || '{}'); } catch(e) { return; }
        var type = evt.detail.type;
        var stream = document.getElementById('chat-stream-area');
        var messages = document.getElementById('chat-messages');
        if (!stream) return;

        if (type === 'chat-thinking') {
            if (!stream.querySelector('.chat-thinking')) {
                stream.insertAdjacentHTML('beforeend',
                    '<div class="chat-thinking">' +
                    '<div class="chat-thinking-dots"><span></span><span></span><span></span></div>' +
                    '<span class="muted">Thinking...</span></div>');
            }
            scrollToBottom();
        }
        else if (type === 'chat-tool-start') {
            var thinking = stream.querySelector('.chat-thinking');
            if (thinking) thinking.remove();
            stream.insertAdjacentHTML('beforeend',
                '<div class="chat-activity chat-activity-tool" data-tool="' + escapeHtml(data.tool_name) + '">' +
                '<span class="chat-activity-icon" aria-hidden="true">&#9881;</span>' +
                '<span class="chat-activity-label">Running <strong>' + escapeHtml(data.tool_name) + '</strong>...</span></div>');
            scrollToBottom();
        }
        else if (type === 'chat-tool-result') {
            var acts = stream.querySelectorAll('.chat-activity-tool');
            var last = acts[acts.length - 1];
            if (last) {
                last.classList.remove('chat-activity-tool');
                last.classList.add(data.success ? 'chat-activity-done' : 'chat-activity-error');
                var lbl = last.querySelector('.chat-activity-label');
                if (lbl) {
                    lbl.innerHTML = '<strong>' + escapeHtml(data.tool_name) + '</strong>' +
                        ' <small class="muted">(' + data.duration_ms + 'ms)</small>';
                }
            }
            scrollToBottom();
        }
        else if (type === 'chat-done') {
            var thinking = stream.querySelector('.chat-thinking');
            if (thinking) thinking.remove();

            // Move activity entries to permanent area
            stream.querySelectorAll('.chat-activity').forEach(function(a) {
                messages.appendChild(a);
            });

            // Render agent bubble
            messages.insertAdjacentHTML('beforeend',
                '<div class="chat-row chat-row-agent">' +
                '<div class="chat-agent-avatar" aria-hidden="true">' + agentInitial + '</div>' +
                '<div class="chat-agent-column">' +
                '<div class="chat-agent-name muted">' + escapeHtml(agentName) + '</div>' +
                '<div class="chat-bubble chat-bubble-agent">' +
                '<div class="chat-bubble-content-agent">' + escapeHtml(data.answer) + '</div>' +
                '</div></div></div>');

            stream.innerHTML = '';
            document.getElementById('chat-send-btn').disabled = false;
            scrollToBottom();
        }
        else if (type === 'chat-error') {
            var thinking = stream.querySelector('.chat-thinking');
            if (thinking) thinking.remove();
            stream.insertAdjacentHTML('beforeend',
                '<div class="chat-done-banner chat-done-error">' +
                '<span class="chat-done-icon">!</span> ' + escapeHtml(data.message) + '</div>');
            document.getElementById('chat-send-btn').disabled = false;
        }
    });

    // Scroll to bottom on page load
    scrollToBottom();
}());
</script>

{% endblock %}
```

### Step 4: Update the conversation handler context

In `crates/agentos-web/src/handlers/chat.rs`, in the `conversation()` handler, ensure the template context includes `tool_name` and `tool_duration_ms`:

```rust
.map(|m| {
    context! {
        role => m.role,
        content => m.content,
        created_at => m.created_at,
        tool_name => m.tool_name.clone().unwrap_or_default(),
        tool_duration_ms => m.tool_duration_ms.unwrap_or(0),
    }
})
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/chat_conversation.html` | Full rewrite with HTMX + SSE JavaScript |
| `crates/agentos-web/src/templates/partials/chat_message.html` | New file: renders user/tool/assistant messages |
| `crates/agentos-web/src/templates.rs` | Register `partials/chat_message.html` |
| `crates/agentos-web/src/handlers/chat.rs` | Update `conversation()` context to include tool fields |

---

## Prerequisites

- [[28-03-Add Chat SSE Streaming Endpoint]] -- provides the SSE endpoint and event format.
- [[28-02-Extend ChatStore Schema for Tool Metadata]] -- provides `tool_name` and `tool_duration_ms` fields.

---

## Test Plan

- `cargo build -p agentos-web` must compile (template is included via `include_str!`).
- Manual test: Load a conversation page. Verify all existing messages render correctly.
- Manual test: Send a message. Verify the send button disables, thinking dots appear, tool calls show as activity entries, and the final answer renders as an agent bubble.
- Manual test: Verify Enter submits and Shift+Enter adds a newline.
- Manual test: Verify the page scrolls to the bottom as new content arrives.
- Manual test: Trigger an error (disconnect the LLM). Verify the error banner appears and the send button re-enables.
- Manual test: Verify tool activity entries show in old conversations with tool calls stored from subtask 28-02.

---

## Verification

```bash
cargo build -p agentos-web
cargo clippy -p agentos-web -- -D warnings
cargo fmt --all -- --check
```
