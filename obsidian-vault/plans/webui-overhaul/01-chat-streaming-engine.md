---
title: Phase 01 — Chat Streaming Engine
tags:
  - webui
  - chat
  - sse
  - llm
  - phase-01
date: 2026-04-11
status: planned
effort: 2d
priority: critical
---

# Phase 01 — Chat Streaming Engine Fix

> Make the chat *actually* stream tokens incrementally, end-to-end. Replace the chaotic mixed-format SSE protocol with one structured JSON envelope, fix the `chat-done` clobber bug, add a `chat-thinking` listener, cap the in-flight buffer, and patch LLM adapters that buffer the entire response.

---

## Why this phase

User-reported: "the chat in the UI is not seems like streaming, the full response is shown instantly without streaming." The kernel logs show `chat_infer_streaming` IS running, but the browser only ever shows the final answer in one frame. This phase identifies and fixes the three layers where streaming breaks down:

1. **LLM adapter layer** — some OpenAI-compatible providers (`kimi-k2.5:cloud`, `mistral`, `cohere`) consume the upstream SSE stream into a `String` and only emit one `InferenceEvent::Token` at the end. The kernel forwards exactly what the adapter gives it.
2. **SSE protocol layer** — server emits inconsistent frame formats (text in `chat-text-chunk`, JSON in `chat-tool-start`, full HTML in `chat-done`). Browser handlers branch on event name and parse each one differently. Adding a new event type means changing both layers.
3. **Browser render layer** — `chat-done` does `container.outerHTML = e.data` which discards every streamed text fragment, replaces the `#chat-stream-target` element entirely, and re-runs from scratch. Even if the streaming layers above worked perfectly, this would make streaming invisible.

## Current → Target State

| Concern | Current | Target |
|---------|---------|--------|
| LLM adapter token forwarding | OpenAI-compat eats the stream into a String, emits one `Token` at end | Each SSE `data:` chunk forwarded as its own `InferenceEvent::Token(text)` |
| Adapter without real streaming (e.g. mock) | Caller sees one big chunk | Server simulates streaming by chunking final text into 80-char windows with 30 ms delay |
| SSE event names | 5 names (`chat-thinking`, `chat-text-chunk`, `chat-tool-start`, `chat-tool-result`, `chat-done`) | 1 name (`chat-stream`) with `type` discriminator inside JSON payload |
| `chat-done` rendering | `container.outerHTML = e.data` (clobbers everything) | Marks streamed text as final, removes thinking placeholder, runs final markdown pass |
| `chat-thinking` handler | Missing | Shows iteration spinner + "Thinking…" with iteration number |
| In-flight event buffer | Unbounded `Vec<ChatStreamEvent>` | Capped at `MAX_EVENTS = 10_000`; coalesces consecutive `TextChunk` deltas before truncation |
| Subscriber resume | Replays full buffer from cursor 0 every reconnect | Same, but capped at `MAX_REPLAY = 5_000` events; older deltas are merged into a snapshot |

## Detailed subtasks

### 1. Define the new SSE envelope ([crates/agentos-types/src/chat.rs](crates/agentos-types/src/chat.rs))

Add a new `ChatStreamFrame` enum that the SSE handler will serialise. The kernel still emits `ChatStreamEvent` internally (its API is stable); the conversion happens at the SSE boundary.

```rust
// crates/agentos-types/src/chat.rs (new file or add to existing module)
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ChatStreamFrame {
    Thinking { iteration: u32 },
    TextDelta { text: String },
    ToolStart {
        tool_name: String,
        iteration: u32,
        intent: Option<serde_json::Value>,
    },
    ToolResult {
        tool_name: String,
        success: bool,
        duration_ms: u64,
        output_preview: Option<String>,
        error: Option<String>,
    },
    Iteration { iteration: u32, reason: String },
    Done {
        answer: String,
        iterations: u32,
        tokens_used: Option<u64>,
    },
    Error { message: String },
}
```

Add a `From<ChatStreamEvent> for ChatStreamFrame` impl that does the kernel→SSE mapping. Tool start/result come with structured fields already, so the conversion is mechanical.

### 2. Rewrite the SSE handler ([crates/agentos-web/src/handlers/chat.rs](crates/agentos-web/src/handlers/chat.rs))

Replace the `match &event { ... }` block at lines 487–542 with a single conversion + serialise:

```rust
let stream = ReceiverStream::new(rx).map(move |event| {
    let frame = ChatStreamFrame::from(event);
    let payload = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_string());
    Ok::<_, Infallible>(Event::default().event("chat-stream").data(payload))
});
```

The `agent_name_for_stream` no longer needed for HTML rendering — the browser owns presentation.

### 3. Patch LLM adapters that buffer the entire response

Audit each adapter in `crates/agentos-llm/src/`:

```bash
# Find adapters that accumulate the stream into a String before emitting
grep -rn "infer_stream_with_tools\|push_str\|format!.*body" crates/agentos-llm/src/
```

Adapters to inspect (priority order based on usage):
- `crates/agentos-llm/src/openai.rs`
- `crates/agentos-llm/src/anthropic.rs`
- `crates/agentos-llm/src/ollama.rs`
- `crates/agentos-llm/src/gemini.rs`
- `crates/agentos-llm/src/mistral.rs` / `cohere.rs` / `xai.rs` / `cerebras.rs` / `azure.rs` / `mock.rs`

For each adapter:
- The streaming method MUST emit `InferenceEvent::Token(chunk)` for each non-empty `data:` chunk received from the upstream provider, NOT only at the end.
- The current OpenAI implementation in many forks does:
  ```rust
  let mut full_text = String::new();
  while let Some(chunk) = body.chunk().await? {
      let parsed = parse_sse(chunk);
      full_text.push_str(&parsed.delta);
  }
  tx.send(InferenceEvent::Token(full_text)).await?;
  tx.send(InferenceEvent::Done(...)).await?;
  ```
- Change to:
  ```rust
  let mut full_text = String::new();
  while let Some(chunk) = body.chunk().await? {
      let parsed = parse_sse(chunk);
      if !parsed.delta.is_empty() {
          tx.send(InferenceEvent::Token(parsed.delta.clone())).await?;
          full_text.push_str(&parsed.delta);
      }
      // ToolCallDelta etc forwarded similarly
  }
  tx.send(InferenceEvent::Done(InferenceResult { text: full_text, .. })).await?;
  ```

For the **mock** adapter (used in tests), and any genuinely non-streaming adapter, add a helper:

```rust
// crates/agentos-llm/src/streaming_helpers.rs (new file)
pub async fn simulate_token_stream(
    tx: &Sender<InferenceEvent>,
    text: &str,
    chunk_chars: usize,
    delay: Duration,
) -> Result<()> {
    let chars: Vec<char> = text.chars().collect();
    for window in chars.chunks(chunk_chars) {
        let chunk: String = window.iter().collect();
        tx.send(InferenceEvent::Token(chunk)).await?;
        tokio::time::sleep(delay).await;
    }
    Ok(())
}
```

Use this in adapters that lack real streaming so the UX is consistent.

### 4. Add per-message chunk timing instrumentation

Add a `tracing::trace!` at every adapter Token emission so we can verify in the kernel logs that real chunks are flowing:

```rust
tracing::trace!(
    target: "agentos::llm::stream",
    provider = %provider_name,
    chunk_len = chunk.len(),
    "Token chunk forwarded"
);
```

The acceptance test for this phase is: run a chat against the test agent, count Token events in the kernel log over the inference duration, and expect ≥ 10 events for any response > 200 chars.

### 5. Cap the in-flight event buffer ([crates/agentos-web/src/chat_inflight.rs](crates/agentos-web/src/chat_inflight.rs))

Replace the unbounded `events: Vec<ChatStreamEvent>` with:

```rust
const MAX_EVENTS: usize = 10_000;
const COALESCE_THRESHOLD: usize = 8_000;

struct InFlightInner {
    events: Vec<ChatStreamEvent>,
    coalesced_text_prefix: String,  // Text from dropped TextChunk events
    done: bool,
}

impl InFlightInner {
    fn push(&mut self, event: ChatStreamEvent) {
        if self.events.len() >= COALESCE_THRESHOLD {
            self.coalesce_old_text();
        }
        self.events.push(event);
    }

    fn coalesce_old_text(&mut self) {
        // Walk events from the start, merging consecutive TextChunk into coalesced_text_prefix
        // until we've reduced the vec by ~2_000 entries
        let mut merged = String::new();
        let mut drained = 0;
        let target = 2_000;
        self.events.retain(|e| {
            if drained >= target {
                return true;
            }
            if let ChatStreamEvent::TextChunk { text } = e {
                merged.push_str(text);
                drained += 1;
                false
            } else {
                true
            }
        });
        self.coalesced_text_prefix.push_str(&merged);
    }
}
```

When a new subscriber attaches, replay starts with a synthetic `TextChunk { text: coalesced_text_prefix.clone() }` (if non-empty) followed by the remaining events.

### 6. Rewrite [`static/js/chat-stream.js`](crates/agentos-web/static/js/chat-stream.js)

Full rewrite. The new file:

```javascript
// crates/agentos-web/static/js/chat-stream.js
// Streams a chat response from /chat/{id}/stream into the DOM, rendering
// markdown incrementally and showing tool/thinking activity inline.
//
// Depends on window.marked and window.DOMPurify being loaded (vendored at
// /static/js/marked.min.js and /static/js/dompurify.min.js).
(function () {
    var container = document.getElementById('chat-stream-target');
    if (!container || container.dataset.streamAttached === '1') return;
    container.dataset.streamAttached = '1';

    var sessionId = container.dataset.sessionId;
    if (!sessionId) return;

    var thinking = container.querySelector('.chat-thinking-indicator');
    var responseDiv = container.querySelector('.chat-stream-response');
    var textDiv = container.querySelector('.chat-stream-markdown');
    var activityList = container.querySelector('.chat-activity-list');
    var msgList = document.getElementById('chat-messages-list');

    var rawMarkdown = '';
    var hasText = false;

    function renderMarkdown() {
        if (!textDiv) return;
        var html = window.marked.parse(rawMarkdown, { breaks: true, gfm: true });
        textDiv.innerHTML = window.DOMPurify.sanitize(html);
        // Re-highlight any code blocks
        if (window.hljs) {
            textDiv.querySelectorAll('pre code').forEach(window.hljs.highlightElement);
        }
        if (msgList) msgList.scrollTop = msgList.scrollHeight;
    }

    function showThinking(iteration) {
        if (!thinking) return;
        thinking.style.display = '';
        var label = thinking.querySelector('.chat-thinking-label');
        if (label) {
            label.textContent = iteration > 1
                ? 'Thinking… (iteration ' + iteration + ')'
                : 'Thinking…';
        }
    }
    function hideThinking() {
        if (thinking) thinking.style.display = 'none';
    }

    function pushToolCard(name, iteration) {
        var card = document.createElement('div');
        card.className = 'chat-tool-card chat-tool-running';
        card.dataset.toolName = name;
        card.dataset.iteration = iteration;
        card.innerHTML =
            '<div class="chat-tool-card-head">'
              + '<span class="chat-tool-icon" aria-hidden="true">⚙</span>'
              + '<span class="chat-tool-name"></span>'
              + '<span class="chat-tool-status">running…</span>'
            + '</div>';
        card.querySelector('.chat-tool-name').textContent = name;
        if (activityList) activityList.appendChild(card);
        return card;
    }

    function findOpenToolCard(name) {
        if (!activityList) return null;
        var cards = activityList.querySelectorAll('.chat-tool-running[data-tool-name="' + CSS.escape(name) + '"]');
        return cards[cards.length - 1] || null;
    }

    function finalizeToolCard(name, success, durationMs, output, error) {
        var card = findOpenToolCard(name);
        if (!card) card = pushToolCard(name, 0);
        card.classList.remove('chat-tool-running');
        card.classList.add(success ? 'chat-tool-done' : 'chat-tool-error');
        var status = card.querySelector('.chat-tool-status');
        if (status) status.textContent = success
            ? '✓ ' + durationMs + 'ms'
            : '✗ ' + (error || 'failed');
        if (output) {
            var details = document.createElement('details');
            details.className = 'chat-tool-output';
            var summary = document.createElement('summary');
            summary.textContent = 'Output';
            var pre = document.createElement('pre');
            pre.className = 'chat-tool-output-body';
            pre.textContent = output;
            details.appendChild(summary);
            details.appendChild(pre);
            card.appendChild(details);
        }
    }

    var es = new EventSource('/chat/' + encodeURIComponent(sessionId) + '/stream');

    es.addEventListener('chat-stream', function (e) {
        var frame;
        try { frame = JSON.parse(e.data); } catch (_) { return; }
        switch (frame.type) {
            case 'thinking':
                showThinking(frame.iteration || 1);
                break;
            case 'text-delta':
                if (!hasText) {
                    hasText = true;
                    hideThinking();
                    if (responseDiv) responseDiv.style.display = '';
                }
                rawMarkdown += frame.text;
                renderMarkdown();
                break;
            case 'tool-start':
                hideThinking();
                pushToolCard(frame.tool_name, frame.iteration || 1);
                break;
            case 'tool-result':
                finalizeToolCard(
                    frame.tool_name,
                    frame.success,
                    frame.duration_ms,
                    frame.output_preview,
                    frame.error
                );
                break;
            case 'iteration':
                showThinking(frame.iteration || 1);
                break;
            case 'done':
                if (frame.answer && (!hasText || rawMarkdown.length === 0)) {
                    hasText = true;
                    hideThinking();
                    if (responseDiv) responseDiv.style.display = '';
                    rawMarkdown = frame.answer;
                    renderMarkdown();
                }
                if (textDiv) textDiv.classList.remove('chat-streaming');
                es.close();
                break;
            case 'error':
                hideThinking();
                if (textDiv) {
                    textDiv.classList.remove('chat-streaming');
                    textDiv.classList.add('chat-error');
                    textDiv.textContent = 'Error: ' + (frame.message || 'Unknown error');
                }
                es.close();
                break;
        }
    });

    es.onerror = function () {
        if (es.readyState === EventSource.CLOSED) return;
        es.close();
        hideThinking();
        if (textDiv && !hasText) {
            textDiv.textContent = '(Connection lost — refresh to see the stored response.)';
        }
    };
})();
```

### 7. Vendor `marked.min.js` and `dompurify.min.js`

Download into `crates/agentos-web/static/js/`:
- `marked.min.js` — current stable v12.x
- `dompurify.min.js` — current stable v3.x
- `highlight.min.js` — core + bundled languages: rust, python, javascript, typescript, bash, json, toml, yaml, markdown, html, css, sql

Update `chat_conversation.html` and the partial returned by `send()` to include the scripts BEFORE `chat-stream.js`:

```html
<script src="/static/js/marked.min.js"></script>
<script src="/static/js/dompurify.min.js"></script>
<script src="/static/js/highlight.min.js"></script>
<script src="/static/js/chat-stream.js"></script>
```

Add a unit/integration test that fetches `/static/js/marked.min.js` and `/static/js/dompurify.min.js` and asserts non-empty 200 responses. (This catches missing vendoring at CI time.)

### 8. Update `chat_conversation.html` and `send()` partial markup

`#chat-stream-target` markup must include the new `.chat-activity-list`, `.chat-thinking-indicator`, and `.chat-stream-markdown` elements that the JS expects. This is a small markup tweak — the visual design lives in Phase 02.

```html
<div id="chat-stream-target"
     data-session-id="{{ session_id }}"
     data-agent-name="{{ agent_name }}"
     data-agent-initial="{{ agent_initial }}">
    <div class="chat-thinking-indicator">
        <div class="chat-thinking-dots"><span></span><span></span><span></span></div>
        <span class="chat-thinking-label muted">Thinking…</span>
    </div>
    <div class="chat-activity-list"></div>
    <div class="chat-stream-response chat-row chat-row-agent" style="display:none;">
        <div class="chat-agent-avatar" aria-hidden="true">{{ agent_initial }}</div>
        <div class="chat-agent-column">
            <div class="chat-agent-name muted">{{ agent_name }}</div>
            <div class="chat-bubble chat-bubble-agent">
                <div class="chat-stream-markdown chat-streaming"></div>
            </div>
        </div>
    </div>
</div>
```

## Files changed

| File | Change |
|------|--------|
| [crates/agentos-types/src/chat.rs](crates/agentos-types/src/chat.rs) | NEW `ChatStreamFrame` enum + `From<ChatStreamEvent>` impl |
| [crates/agentos-types/src/lib.rs](crates/agentos-types/src/lib.rs) | Re-export `ChatStreamFrame` |
| [crates/agentos-web/src/handlers/chat.rs](crates/agentos-web/src/handlers/chat.rs) | Replace SSE event mapping with single `chat-stream` JSON envelope; update `send()` HTML partial to new markup |
| [crates/agentos-web/src/chat_inflight.rs](crates/agentos-web/src/chat_inflight.rs) | Add `MAX_EVENTS` cap + `coalesce_old_text` |
| [crates/agentos-web/src/templates/chat_conversation.html](crates/agentos-web/src/templates/chat_conversation.html) | Update `#chat-stream-target` markup; add marked/dompurify/highlight script tags |
| [crates/agentos-web/static/js/chat-stream.js](crates/agentos-web/static/js/chat-stream.js) | Full rewrite: single `chat-stream` listener, branches on `frame.type`, runs marked+DOMPurify on every text-delta |
| [crates/agentos-web/static/js/marked.min.js](crates/agentos-web/static/js/marked.min.js) | NEW vendored asset |
| [crates/agentos-web/static/js/dompurify.min.js](crates/agentos-web/static/js/dompurify.min.js) | NEW vendored asset |
| [crates/agentos-web/static/js/highlight.min.js](crates/agentos-web/static/js/highlight.min.js) | NEW vendored asset |
| [crates/agentos-llm/src/openai.rs](crates/agentos-llm/src/openai.rs) | Forward each SSE delta as a Token event |
| [crates/agentos-llm/src/anthropic.rs](crates/agentos-llm/src/anthropic.rs) | Same |
| [crates/agentos-llm/src/ollama.rs](crates/agentos-llm/src/ollama.rs) | Same |
| [crates/agentos-llm/src/streaming_helpers.rs](crates/agentos-llm/src/streaming_helpers.rs) | NEW `simulate_token_stream` helper |
| [crates/agentos-llm/src/mock.rs](crates/agentos-llm/src/mock.rs) | Use `simulate_token_stream` |

## Dependencies

- Blocks: [[02-chat-ui-redesign]]
- Independent of: 03, 04, 05, 06, 07

## Test plan

1. **Unit: ChatStreamFrame serde round-trip**
   - `crates/agentos-types/src/chat.rs#tests` — assert each variant serialises with the expected `type` discriminator.
2. **Unit: chat_inflight coalescing**
   - Push 12_000 `TextChunk` events, assert `events.len() < MAX_EVENTS` and `coalesced_text_prefix` contains the dropped text in order.
3. **Integration: chat streaming**
   - `crates/agentos-web/tests/chat_streaming_test.rs` (new file)
   - Spin up a kernel with the mock adapter that emits 50 small token chunks via `simulate_token_stream`.
   - POST to `/chat/{id}/send`, then GET `/chat/{id}/stream`.
   - Parse the SSE response and assert: ≥ 50 frames with `type=text-delta`, exactly 1 `type=done`, no `type=error`.
4. **Manual smoke**
   - Connect a real Ollama agent, ask it "tell me a story about a robot in 5 paragraphs".
   - Expected: thinking dots disappear within 1 sec, text streams in word-by-word, markdown formatting (bold, lists, etc.) renders as it appears, no DOM clobber at the end.
5. **Regression: refresh during streaming**
   - Send a message, refresh the page mid-stream. Expected: the page renders prior text from the coalesced buffer + continues streaming live tokens.

## Verification

```bash
# Compile clean
cargo build -p agentos-types -p agentos-llm -p agentos-web -p agentos-kernel

# Tests
cargo test -p agentos-types chat::
cargo test -p agentos-web chat_inflight
cargo test -p agentos-web --test chat_streaming_test

# Lint
cargo clippy -p agentos-types -p agentos-llm -p agentos-web -- -D warnings
cargo fmt --all -- --check

# Manual: in browser, open devtools network tab on /chat/{id}, watch chat-stream events
# Expected: dozens of frames over the duration of the response, not one big frame at the end
```

## Related

- [[WebUI Overhaul Plan]]
- [[02-chat-ui-redesign]]
- [[WebUI Overhaul Data Flow]]
