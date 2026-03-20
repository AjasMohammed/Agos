---
title: Chat SSE Streaming Endpoint
tags:
  - web
  - llm
  - v3
  - plan
date: 2026-03-18
status: planned
effort: 2d
priority: high
---

# Phase 03 -- Chat SSE Streaming Endpoint

> Replace the synchronous POST-redirect-GET chat flow with an SSE (Server-Sent Events) endpoint that streams tool activity and the final response incrementally to the browser.

---

## Why This Phase

The current chat flow blocks the user for 10-30 seconds while waiting for inference. With the tool execution loop from Phase 01, this delay can be even longer (multiple inference + tool execution rounds). SSE streaming allows the UI to:

- Show a "thinking" indicator immediately.
- Display tool calls as they happen (collapsible activity entries).
- Stream the final response token-by-token for a responsive feel.

The codebase already has working SSE infrastructure in `handlers/events.rs` using `axum::response::sse::{Event, Sse}` and `futures::stream`. The `LLMCore` trait has `infer_stream()` with `mpsc::Sender<InferenceEvent>`.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| POST /chat/{id}/send | Synchronous; blocks until inference completes; redirects to GET | Returns HTMX partial immediately; SSE stream starts |
| SSE endpoint | None for chat | `GET /chat/{id}/stream` returns SSE with tool + message events |
| Kernel method | `chat_infer_with_tools()` returns `ChatInferenceResult` (blocking) | New `chat_infer_streaming()` sends events via `mpsc::Sender<ChatStreamEvent>` |
| Event types | N/A | `chat-thinking`, `chat-tool-start`, `chat-tool-result`, `chat-token`, `chat-done`, `chat-error` |

---

## What to Do

### Step 1: Define `ChatStreamEvent`

In `crates/agentos-kernel/src/kernel.rs`, add:

```rust
/// Events emitted during streaming chat inference.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum ChatStreamEvent {
    /// Inference started -- LLM is thinking.
    Thinking { iteration: u32 },
    /// A tool call was detected; execution is starting.
    ToolStart { tool_name: String, iteration: u32 },
    /// A tool call completed.
    ToolResult {
        tool_name: String,
        result_preview: String,
        duration_ms: u64,
        success: bool,
    },
    /// A chunk of the final response text (for token-by-token streaming).
    Token { text: String },
    /// The complete final response (sent after all tokens or as a single event for non-streaming LLMs).
    Done {
        answer: String,
        tool_calls: Vec<ChatToolCallRecord>,
        iterations: u32,
    },
    /// An error occurred.
    Error { message: String },
}
```

### Step 2: Implement `chat_infer_streaming()`

Add a new method to `impl Kernel` that is similar to `chat_infer_with_tools()` but sends `ChatStreamEvent` values through an `mpsc::Sender` instead of accumulating results:

```rust
pub async fn chat_infer_streaming(
    &self,
    agent_name: &str,
    history: &[(String, String)],
    new_message: &str,
    tx: tokio::sync::mpsc::Sender<ChatStreamEvent>,
) -> Result<(), String> {
    // ... (same agent/LLM lookup and context build as chat_infer_with_tools) ...

    let mut tool_calls = Vec::new();
    let mut iterations = 0u32;

    loop {
        iterations += 1;
        if iterations > CHAT_MAX_TOOL_ITERATIONS {
            let _ = tx.send(ChatStreamEvent::Error {
                message: "Maximum tool call limit reached.".to_string(),
            }).await;
            break;
        }

        let _ = tx.send(ChatStreamEvent::Thinking { iteration: iterations }).await;

        let result = match llm.infer(&ctx).await {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(ChatStreamEvent::Error {
                    message: format!("Inference failed: {}", e),
                }).await;
                return Err(e.to_string());
            }
        };

        match crate::tool_call::parse_tool_call(&result.text) {
            Some(tool_call) => {
                let _ = tx.send(ChatStreamEvent::ToolStart {
                    tool_name: tool_call.tool_name.clone(),
                    iteration: iterations,
                }).await;

                // (same tool execution logic as chat_infer_with_tools)
                // ... execute tool, record result, push into context ...

                let _ = tx.send(ChatStreamEvent::ToolResult {
                    tool_name: tool_call.tool_name.clone(),
                    result_preview: truncate_preview(&tool_result_str, 200),
                    duration_ms,
                    success: !tool_result_str.contains("\"error\""),
                }).await;
            }
            None => {
                let _ = tx.send(ChatStreamEvent::Done {
                    answer: result.text.clone(),
                    tool_calls: tool_calls.clone(),
                    iterations,
                }).await;
                return Ok(());
            }
        }
    }

    Ok(())
}
```

### Step 3: Add the SSE endpoint handler

Open `crates/agentos-web/src/handlers/chat.rs`. Add a new handler:

```rust
/// GET /chat/{session_id}/stream -- SSE endpoint for streaming chat responses.
/// The browser connects to this after submitting a message via POST.
pub async fn message_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatStreamEvent>(32);

    // Load session info
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    let session = tokio::task::spawn_blocking(move || store.get_session(&sid))
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Load history
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    let history: Vec<(String, String)> = tokio::task::spawn_blocking(move || store.get_messages(&sid))
        .await
        .unwrap()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.role != "tool") // LLM only sees user/assistant
        .map(|m| (m.role, m.content))
        .collect();

    // Find the last user message (the one we're responding to)
    let last_user_msg = history.iter().rev()
        .find(|(role, _)| role == "user")
        .map(|(_, content)| content.clone())
        .unwrap_or_default();

    // Spawn the inference task
    let kernel = state.kernel.clone();
    let agent_name = session.agent_name.clone();
    let chat_store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();

    tokio::spawn(async move {
        let result = kernel.chat_infer_streaming(
            &agent_name,
            &history[..history.len().saturating_sub(1)], // exclude last user msg
            &last_user_msg,
            tx.clone(),
        ).await;

        // Save results to store
        if let Ok(()) = result {
            // Tool calls and answer are saved by the Done event handler
        }
    });

    // Convert mpsc::Receiver into an SSE stream
    let stream = async_stream::stream! {
        while let Some(event) = rx.recv().await {
            let event_name = match &event {
                ChatStreamEvent::Thinking { .. } => "chat-thinking",
                ChatStreamEvent::ToolStart { .. } => "chat-tool-start",
                ChatStreamEvent::ToolResult { .. } => "chat-tool-result",
                ChatStreamEvent::Token { .. } => "chat-token",
                ChatStreamEvent::Done { .. } => "chat-done",
                ChatStreamEvent::Error { .. } => "chat-error",
            };
            let data = serde_json::to_string(&event).unwrap_or_default();
            yield Ok::<_, Infallible>(Event::default().event(event_name).data(data));
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

### Step 4: Update POST /send to return an HTMX partial

Change the `send()` handler to, instead of doing inference and redirecting, just save the user message and return an HTML partial that starts the SSE connection:

```rust
pub async fn send(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Form(form): Form<SendForm>,
) -> Response {
    // ... (existing validation) ...
    // ... (save user message -- existing code) ...

    // Return HTMX partial that initiates SSE
    let html = format!(
        r#"<div class="chat-row chat-row-user">
            <div class="chat-bubble chat-bubble-user">
                <div class="chat-bubble-content">{}</div>
            </div>
        </div>
        <div id="chat-stream-target"
             hx-ext="sse"
             sse-connect="/chat/{}/stream"
             sse-swap="chat-done"
             hx-swap="outerHTML">
            <div class="chat-thinking">
                <div class="chat-thinking-dots"><span></span><span></span><span></span></div>
                <span class="muted">Thinking...</span>
            </div>
        </div>"#,
        html_escape(&message),
        session_id,
    );
    Html(html).into_response()
}
```

### Step 5: Register the new route

In `crates/agentos-web/src/router.rs`, add:

```rust
.route("/chat/{session_id}/stream", axum::routing::get(chat::message_stream))
```

### Step 6: Add `async-stream` dependency

In `crates/agentos-web/Cargo.toml`, add:

```toml
async-stream = "0.3"
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/kernel.rs` | Add `ChatStreamEvent`, `chat_infer_streaming()` |
| `crates/agentos-web/src/handlers/chat.rs` | Add `message_stream()` handler; update `send()` to return HTMX partial |
| `crates/agentos-web/src/router.rs` | Add `/chat/{session_id}/stream` route |
| `crates/agentos-web/Cargo.toml` | Add `async-stream = "0.3"` |

---

## Dependencies

- [[01-chat-tool-execution-loop]] -- provides the tool loop logic.
- [[02-chat-store-tool-metadata]] -- provides `add_tool_calls()` for persistence.

---

## Test Plan

- `cargo build -p agentos-web` must compile.
- Add test `test_chat_stream_events_sequence`: Create a mock LLM that returns a tool call then a final answer. Connect to the SSE endpoint. Verify the events arrive in order: `chat-thinking` -> `chat-tool-start` -> `chat-tool-result` -> `chat-thinking` -> `chat-done`.
- Add test `test_chat_stream_error_event`: Mock an LLM that fails. Verify a `chat-error` event is sent.
- Manual test: Open the chat page, send a message, verify the thinking indicator appears immediately and events stream in.

---

## Verification

```bash
cargo build -p agentos-kernel -p agentos-web
cargo test -p agentos-web -- chat --nocapture
cargo clippy -p agentos-kernel -p agentos-web -- -D warnings
```
