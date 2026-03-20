---
title: Chat Conversation Template HTMX
tags:
  - web
  - v3
  - plan
date: 2026-03-18
status: planned
effort: 2d
priority: high
---

# Phase 04 -- Chat Conversation Template with HTMX Streaming

> Rewrite the conversation template (`chat_conversation.html`) to use HTMX SSE for real-time streaming, render tool call activity as collapsible indicators, and show a typing/thinking animation during inference.

---

## Why This Phase

After Phase 03, the backend streams `ChatStreamEvent` values via SSE. But the frontend template (`chat_conversation.html`) is still a static page that renders all messages on load with a form that does a full POST-redirect. This phase connects the frontend to the SSE stream using HTMX's `sse` extension, adds JavaScript to handle each event type, and renders tool calls as collapsible activity indicators instead of raw JSON.

The CSS classes for chat activity indicators (`chat-activity`, `chat-activity-tool`, `chat-thinking`, etc.) already exist in `app.css` lines 855-937.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Message send | Full form POST -> redirect -> page reload | HTMX `hx-post` -> append user bubble -> start SSE stream |
| Thinking indicator | None | Animated dots appear immediately after send |
| Tool call display | Raw JSON in assistant bubble | Collapsible `chat-activity-tool` entries between bubbles |
| Response rendering | Full page load shows completed response | Tokens stream in; final answer replaces thinking indicator |
| Auto-scroll | None | Page scrolls to bottom as new messages arrive |
| Tool messages in history | Not rendered differently | Rendered as compact activity rows, not full bubbles |

---

## What to Do

### Step 1: Create the chat message partial

Create `crates/agentos-web/src/templates/partials/chat_message.html`:

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
    <details class="chat-activity chat-activity-tool" style="width:100%; max-width:80%;">
        <summary>
            <span class="chat-activity-icon" aria-hidden="true">&#9881;</span>
            <span class="chat-activity-label">
                Tool: <strong>{{ msg.tool_name }}</strong>
                {% if msg.tool_duration_ms %}<small class="muted">({{ msg.tool_duration_ms }}ms)</small>{% endif %}
            </span>
        </summary>
        <pre style="font-size:0.78rem; max-height:200px; overflow:auto; margin:0.5rem 0 0;">{{ msg.content }}</pre>
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

### Step 2: Rewrite `chat_conversation.html`

Replace the template to use HTMX for form submission and SSE for response streaming:

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

<!-- Streaming target: SSE events append here -->
<div id="chat-stream-area"></div>

<!-- Reply form: HTMX POST, no page reload -->
<div class="chat-reply-area">
    <form id="chat-form"
          hx-post="/chat/{{ session_id }}/send"
          hx-target="#chat-stream-area"
          hx-swap="innerHTML"
          hx-on::after-request="this.reset(); scrollChatToBottom();">
        <input type="hidden" name="_csrf" value="{{ csrf_token }}">
        <label for="reply-message">Continue the conversation
            <textarea id="reply-message" name="message" rows="3"
                      placeholder="Reply to {{ agent_name }}..."
                      onkeydown="if(event.key==='Enter' && !event.shiftKey){event.preventDefault();this.form.requestSubmit();}"></textarea>
        </label>
        <div style="display: flex; justify-content: flex-end;">
            <button type="submit" id="chat-send-btn">Send &rarr;</button>
        </div>
    </form>
</div>

<script>
function scrollChatToBottom() {
    var area = document.getElementById('chat-messages');
    if (area) area.scrollTop = area.scrollHeight;
    var stream = document.getElementById('chat-stream-area');
    if (stream) stream.scrollIntoView({ behavior: 'smooth', block: 'end' });
}

// Listen for SSE events from the stream target
document.addEventListener('htmx:sseMessage', function(evt) {
    var data = JSON.parse(evt.detail.data || '{}');
    var type = evt.detail.type;
    var stream = document.getElementById('chat-stream-area');
    var messages = document.getElementById('chat-messages');

    if (type === 'chat-thinking') {
        // Show thinking indicator if not already showing
        if (!stream.querySelector('.chat-thinking')) {
            stream.insertAdjacentHTML('beforeend',
                '<div class="chat-thinking">' +
                '<div class="chat-thinking-dots"><span></span><span></span><span></span></div>' +
                '<span class="muted">Thinking...</span></div>');
        }
        scrollChatToBottom();
    }
    else if (type === 'chat-tool-start') {
        // Remove thinking indicator, show tool activity
        var thinking = stream.querySelector('.chat-thinking');
        if (thinking) thinking.remove();
        stream.insertAdjacentHTML('beforeend',
            '<div class="chat-activity chat-activity-tool">' +
            '<span class="chat-activity-icon" aria-hidden="true">&#9881;</span>' +
            '<span class="chat-activity-label">Running <strong>' +
            escapeHtml(data.tool_name) + '</strong>...</span></div>');
        scrollChatToBottom();
    }
    else if (type === 'chat-tool-result') {
        // Update the last tool activity entry
        var activities = stream.querySelectorAll('.chat-activity-tool');
        var last = activities[activities.length - 1];
        if (last) {
            var cls = data.success ? 'chat-activity-done' : 'chat-activity-error';
            last.classList.remove('chat-activity-tool');
            last.classList.add(cls);
            var label = last.querySelector('.chat-activity-label');
            if (label) {
                label.innerHTML = '<strong>' + escapeHtml(data.tool_name) +
                    '</strong> <small class="muted">(' + data.duration_ms + 'ms)</small>';
            }
        }
        scrollChatToBottom();
    }
    else if (type === 'chat-done') {
        // Remove thinking indicator, render final answer as agent bubble
        var thinking = stream.querySelector('.chat-thinking');
        if (thinking) thinking.remove();

        // Move activity entries to permanent messages area
        var activities = stream.querySelectorAll('.chat-activity');
        activities.forEach(function(a) { messages.appendChild(a); });

        // Render agent bubble
        messages.insertAdjacentHTML('beforeend',
            '<div class="chat-row chat-row-agent">' +
            '<div class="chat-agent-avatar" aria-hidden="true">{{ agent_name[:1]|upper }}</div>' +
            '<div class="chat-agent-column">' +
            '<div class="chat-agent-name muted">{{ agent_name }}</div>' +
            '<div class="chat-bubble chat-bubble-agent">' +
            '<div class="chat-bubble-content-agent">' + escapeHtml(data.answer) + '</div>' +
            '</div></div></div>');

        stream.innerHTML = '';
        scrollChatToBottom();
        document.getElementById('chat-send-btn').disabled = false;
    }
    else if (type === 'chat-error') {
        var thinking = stream.querySelector('.chat-thinking');
        if (thinking) thinking.remove();
        stream.insertAdjacentHTML('beforeend',
            '<div class="chat-done-banner chat-done-error">' +
            '<span class="chat-done-icon">!</span> ' +
            escapeHtml(data.message) + '</div>');
        document.getElementById('chat-send-btn').disabled = false;
    }
});

function escapeHtml(text) {
    var div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

// Initial scroll to bottom on page load
scrollChatToBottom();
</script>

{% endblock %}
```

### Step 3: Register the chat_message partial

In `crates/agentos-web/src/templates.rs`, add:

```rust
env.add_template(
    "partials/chat_message.html",
    include_str!("templates/partials/chat_message.html"),
)?;
```

### Step 4: Update the conversation handler context

In `crates/agentos-web/src/handlers/chat.rs`, in the `conversation()` handler, update the message context building to include the new fields:

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

### Step 5: Update the `send()` handler response

The `send()` handler (updated in Phase 03) should return an HTML partial that the HTMX form swaps into `#chat-stream-area`. This partial includes the user's message bubble appended to the messages area and starts the SSE connection.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/chat_conversation.html` | Full rewrite with HTMX SSE integration |
| `crates/agentos-web/src/templates/partials/chat_message.html` | New partial for rendering individual messages |
| `crates/agentos-web/src/templates.rs` | Register `partials/chat_message.html` |
| `crates/agentos-web/src/handlers/chat.rs` | Update `conversation()` context with tool fields; update `send()` response |

---

## Dependencies

- [[03-chat-sse-streaming-endpoint]] -- provides the SSE endpoint and event types.
- [[02-chat-store-tool-metadata]] -- provides `tool_name` and `tool_duration_ms` fields on `ChatMessage`.

---

## Test Plan

- `cargo build -p agentos-web` must compile.
- Manual test: Open chat, send a message, verify thinking dots appear immediately.
- Manual test: If the LLM calls a tool, verify a collapsible tool activity indicator appears.
- Manual test: Verify the final response renders as an agent bubble, not raw JSON.
- Manual test: Verify old sessions (pre-migration) still render correctly.
- Verify auto-scroll works when messages overflow the viewport.
- Verify Enter key submits the form (Shift+Enter adds newline).

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web -- --nocapture
cargo clippy -p agentos-web -- -D warnings
```
