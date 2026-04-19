---
title: WebUI Overhaul Research
tags:
  - webui
  - chat
  - htmx
  - research
date: 2026-04-11
status: complete
effort: 0.5d
priority: high
---

# WebUI Overhaul Research

> Findings from auditing the running container, source files, and existing planning docs that informed the WebUI Overhaul Plan.

---

## 1. Live Container State (2026-04-11)

The `agentos-kernel` Docker container has been up 16 minutes and is healthy. Key signals from the kernel logs (filtered for chat / web):

```
agentos_web::server: Web UI listening on http://0.0.0.0:8080
agentos::chat: Chat streaming LLM response received agent=mavrick iteration=1
               text_len=2349 native_tool_calls=0 tokens_used=6745
               model=kimi-k2.5:cloud duration_ms=16380
agentos::chat: Streaming chat completed and persisted answer_len=2349 iterations=1 tool_calls=0
```

**Observations:**
- Server-side streaming works — `chat_infer_streaming` runs successfully and persists the response.
- The 16.3-second `duration_ms` for a 2349-char response suggests **the LLM adapter is buffering** the OpenAI-compatible SSE stream and only emitting one big `Token` event at the end. A real token stream would surface as many smaller `TextChunk` events on the bus.
- No errors or warnings from the web layer in the visible window.
- Docker container runtime is disabled (`Docker daemon unreachable`) but that's unrelated to the web UI.

## 2. Chat Code Path (Server)

`POST /chat/{id}/send` flow ([src/handlers/chat.rs:235](crates/agentos-web/src/handlers/chat.rs#L235)):

1. Validate session ID (UUID).
2. Reserve in-flight slot via `state.inflight_chat.try_start(&session_id)`. Returns `409 CONFLICT` if a reply is already being generated.
3. Load history (user/assistant only).
4. Persist user message.
5. **Spawn detached task**: opens an `mpsc::channel<ChatStreamEvent>(64)`, calls `kernel.chat_infer_streaming`, forwards events into the in-flight buffer.
6. Return an HTMX partial: user bubble + empty `#chat-stream-target` div + `<script src="/static/js/chat-stream.js">`.

`GET /chat/{id}/stream` ([src/handlers/chat.rs:448](crates/agentos-web/src/handlers/chat.rs#L448)):

1. Look up the in-flight entry.
2. Subscribe to events (replays buffered events from cursor 0 + tail until done).
3. Map each `ChatStreamEvent` to an SSE frame:
   - `Thinking { iteration }` → `event: chat-thinking, data: {"iteration":1}`
   - `TextChunk { text }` → `event: chat-text-chunk, data: <plain text>`
   - `ToolStart { tool_name, iteration }` → `event: chat-tool-start, data: {"tool_name":"…","iteration":1}`
   - `ToolResult { tool_name, success, duration_ms, … }` → `event: chat-tool-result, data: {…}`
   - `Done { answer }` → `event: chat-done, data: <full HTML bubble>`
   - `Error { message }` → `event: chat-done, data: <error HTML>`

**Mixing JSON and HTML and plain text in `data:` is the root of the rendering disaster.** Every event should emit JSON.

## 3. Chat Code Path (Browser)

`static/js/chat-stream.js` (76 lines):

- Reads `#chat-stream-target` data-* attributes.
- Creates `EventSource('/chat/<id>/stream')`.
- Listeners:
  - `chat-text-chunk`: appends `e.data` to `#chat-stream-text` via `textContent +=`.
  - `chat-tool-start`: parses JSON, appends an "Using X..." pill before the response.
  - `chat-tool-result`: parses JSON, appends a result pill.
  - `chat-done`: closes EventSource and **does `container.outerHTML = e.data`**, replacing the streamed text with server-rendered HTML.
- **No `chat-thinking` listener.** Server emits this but browser silently drops it.
- `onerror` closes the source. No retry, no user feedback.

**Why the user perceives "no streaming":**
- If the LLM adapter emits one big `Token` chunk at the end, the user sees: brief "Thinking…" → all 2349 chars appear in one frame → `chat-done` replaces it with the same content. Looks instant.
- If the LLM adapter actually streams: same outcome, because `chat-done` then replaces the streamed text with the server-rendered version. The streaming work is invisible.

## 4. Markdown Rendering Investigation

Searched the entire web crate for any markdown rendering — none. `chat_conversation.html` renders `{{ msg.content }}` directly inside `<div class="chat-bubble-content-agent">`. MiniJinja auto-escape converts the markdown to escaped text.

The workspace has `pulldown-cmark` available (used elsewhere for Obsidian scratchpad rendering), so server-side rendering is feasible. But for incremental rendering during streaming, client-side is preferable.

`marked` is the de facto standard for browser markdown. Bundle size: ~30KB minified. CommonMark + GFM tables, fenced code, task lists. Pairs with `DOMPurify` (~22KB) for XSS sanitisation.

`highlight.js` core is ~33KB. Adding only the languages we care about (rust, python, javascript, bash, json, toml, yaml, markdown) brings it under 50KB.

## 5. Connect Agent Bug

`agents.html:19`:
```html
<form method="post" action="/agents" hx-post="/agents" hx-target="#agent-grid" hx-swap="innerHTML"
      @htmx:after-request="showModal = false">
```

`agents.rs:82`:
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

`Redirect::to("/agents")` returns a 303. HTMX uses `fetch` which follows 30x by default. The follow-up GET hits the auth-protected `/agents` route which returns the **full HTML page** including `<!DOCTYPE html><html>...`. HTMX swaps this whole document into `#agent-grid` (innerHTML).

Result in the DOM:
```
<div id="agent-grid">
    <!DOCTYPE html>
    <html>
        <head>...</head>
        <body>
            <aside class="sidebar">...</aside>
            <main>...</main>
        </body>
    </html>
</div>
```

The page appears to "do nothing" because the user is still on the original page; they just see the agent grid become weirdly nested. Worse, browsers handle nested `<html>` elements inconsistently, sometimes silently dropping the entire grid.

The Alpine handler `@htmx:after-request="showModal = false"` ALSO fires unconditionally. So even on a 400 error, the modal closes and the user has no idea their submit failed.

## 6. Task Detail Freeze

[task_detail.html:120](crates/agentos-web/src/templates/task_detail.html#L120):
```javascript
function appendLine(text) {
    if (!text.trim()) return;
    var line = document.createElement('div');
    line.className = 'log-line';
    var cls = classifyLine(text);
    if (cls) line.classList.add(cls);
    line.textContent = text;
    terminal.appendChild(line);
    scrollToBottom();
}
```

No cap. A long-running task that writes 50,000 audit lines results in 50,000 `<div>` nodes plus their associated layout state. On a low-end machine the tab freezes around 10–20K nodes.

The log stream itself ([tasks.rs:320](crates/agentos-web/src/handlers/tasks.rs#L320)) polls every 1s and only terminates when the task reaches a terminal state. A `Waiting` task waiting on user input never terminates → stream never closes → DOM keeps growing forever.

The context window section also renders `<pre>{{ msg.content }}</pre>` for each message. If a message payload is several KB of escaped JSON, the layout reflow on `<details>` toggle is significant.

## 7. CLI Coverage Analysis

CLI command groups in `crates/agentos-cli/src/commands/`:

agent, a2a, audit, bg, channel, config, cost, doctor, escalation, event, hal, healthz, identity, init, log, mcp, notifications, onboard, perm, pipeline, plugin, provider, resource, role, schedule, scratchpad, secret, skill, snapshot, status, task, team, tool, webhooks, web

Web UI page coverage:

| Group | Web UI? | Notes |
|-------|---------|-------|
| agent | ✅ partial | List/connect/disconnect; missing detail page tabs for memory, scratchpad, identity |
| audit | ✅ | List + detail |
| chat | ✅ (broken) | This plan fixes it |
| cost | ✅ partial | Dashboard widget; no per-agent breakdown, no historical chart |
| notifications | ✅ | Inbox + detail + respond |
| pipeline | ✅ | Builder + run |
| secret | ✅ | List + create + revoke |
| task | ✅ partial | Detail page can freeze (Phase 05) |
| tool | ✅ | List + install + remove |
| **a2a** | ❌ | Agent-to-agent messaging |
| **channel** | ❌ | Discord/Slack/etc adapter management |
| **config** | ❌ | Config get/set/list (CLI has it) |
| **doctor** | ❌ | System health checks |
| **escalation** | ❌ | Pending escalations approval |
| **hal** | ❌ | Hardware drivers / HAL twins |
| **identity** | ❌ | Agent Ed25519 keypair display |
| **mcp** | ❌ | MCP server discovery / install |
| **plugin** | ❌ | Plugin manifests, enable/disable |
| **resource** | ❌ | Resource locks / arbiter |
| **role** | ❌ | Role definitions |
| **schedule** | ❌ | Cron-style task schedules |
| **scratchpad** | ❌ | Per-agent markdown notes |
| **snapshot** | ❌ | Task snapshots |
| **team** | ❌ | Multi-agent teams |
| **webhooks** | ❌ | Inbound webhook endpoint config |

19 of 35 CLI groups have no UI at all. This plan covers the highest-leverage gaps in Phases 06 and 07.

## 8. Existing Plan Overlap

`obsidian-vault/plans/webui-redesign/` exists with 6 phases focused on:
- Layout & navigation shell (sidebar)
- Agent dashboard charts
- Task management table polish
- Audit log filters
- SSE migration for dashboard widgets
- UX polish (empty states, skeletons, toasts)

**No overlap with this plan** except where empty states / toasts apply to the new pages we're adding. We will reuse `partials/empty_state.html` from the existing plan.

## 9. Decisions Influenced by This Research

- Chat protocol becomes JSON-only (Decision 2)
- DOMPurify is mandatory for any innerHTML in chat (Decision 1, Risk row)
- Tool messages migrate to a separate FK table (Decision 3)
- `chat-done` no longer replaces the streamed text (Decision 4)
- Task log gets a 5K-line ring buffer + 30-min server max stream (Decision 5)
- Connect handler returns the rendered partial directly (Decision 6)
- Pretty-printing centralised as MiniJinja filters, not in handlers (Decision 7)
- All new management pages share `partials/management_page.html` (Decision 8)

## Related

- [[WebUI Overhaul Plan]]
- [[WebUI Overhaul Data Flow]]
- [[WebUI Redesign Plan]] — companion plan
