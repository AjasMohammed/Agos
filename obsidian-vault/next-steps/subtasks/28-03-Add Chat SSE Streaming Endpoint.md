---
title: Add Chat SSE Streaming Endpoint
tags:
  - web
  - llm
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 2d
priority: high
---

# Add Chat SSE Streaming Endpoint

> Add a `GET /chat/{session_id}/stream` SSE endpoint and a `Kernel::chat_infer_streaming()` method that streams tool activity and the final response to the browser in real time, replacing the blocking POST-redirect-GET flow.

---

## Why This Subtask

After subtasks 28-01 and 28-02, the tool execution loop works but the UI still blocks for the entire inference duration (potentially 10-30+ seconds with multiple tool rounds). This subtask adds real-time feedback via Server-Sent Events.

The codebase already has SSE infrastructure: `crates/agentos-web/src/handlers/events.rs` uses `axum::response::sse::{Event, Sse, KeepAlive, KeepAliveStream}` with `futures::stream::BoxStream`. The `LLMCore` trait provides `infer_stream()` via `mpsc::Sender<InferenceEvent>`. The existing SSE endpoints use a `stream::unfold` pattern.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| POST /chat/{id}/send | Blocks until inference + tool loop completes, then redirects | Saves user message, returns HTMX partial that starts SSE |
| SSE chat endpoint | Does not exist | `GET /chat/{id}/stream` returns `Sse<impl Stream<Item = Result<Event, Infallible>>>` |
| Kernel streaming method | Does not exist | `chat_infer_streaming(name, history, msg, tx: mpsc::Sender<ChatStreamEvent>)` |
| `ChatStreamEvent` | Does not exist | Enum: `Thinking`, `ToolStart`, `ToolResult`, `Token`, `Done`, `Error` |
| `async-stream` dependency | Not in `agentos-web/Cargo.toml` | Added for ergonomic async stream construction |

---

## What to Do

1. Open `crates/agentos-kernel/src/kernel.rs`. Add the `ChatStreamEvent` enum:

```rust
/// Events emitted during streaming chat inference.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum ChatStreamEvent {
    Thinking { iteration: u32 },
    ToolStart { tool_name: String, iteration: u32 },
    ToolResult {
        tool_name: String,
        result_preview: String,
        duration_ms: u64,
        success: bool,
    },
    Token { text: String },
    Done {
        answer: String,
        tool_calls: Vec<ChatToolCallRecord>,
        iterations: u32,
    },
    Error { message: String },
}
```

2. Add `chat_infer_streaming()` to `impl Kernel`. This method is structurally identical to `chat_infer_with_tools()` but sends events through an `mpsc::Sender<ChatStreamEvent>` instead of accumulating results:

```rust
pub async fn chat_infer_streaming(
    &self,
    agent_name: &str,
    history: &[(String, String)],
    new_message: &str,
    tx: tokio::sync::mpsc::Sender<ChatStreamEvent>,
) -> Result<ChatInferenceResult, String> {
    // Same agent/LLM lookup and context build as chat_infer_with_tools.
    // In the loop:
    //   - Send ChatStreamEvent::Thinking before each infer().
    //   - On tool call: send ToolStart, execute, send ToolResult.
    //   - On final answer: send Done.
    //   - On error: send Error.
    // Also accumulate tool_calls and return ChatInferenceResult (for persistence).
}
```

The key difference from `chat_infer_with_tools()` is the `tx.send(event).await` calls at each stage. The method still returns `ChatInferenceResult` so the handler can persist tool calls and the answer.

3. Add re-export to `crates/agentos-kernel/src/lib.rs`:

```rust
pub use kernel::ChatStreamEvent;
```

4. Open `crates/agentos-web/Cargo.toml`. Add:

```toml
async-stream = "0.3"
```

5. Open `crates/agentos-web/src/handlers/chat.rs`. Add the SSE endpoint:

```rust
use axum::response::sse::{Event, KeepAlive, Sse};
use std::convert::Infallible;

/// GET /chat/{session_id}/stream -- SSE endpoint for streaming chat inference.
pub async fn message_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, Response> {
    // Validate session_id is a UUID.
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid session ID").into_response());
    }

    // Load session.
    let session = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        match tokio::task::spawn_blocking(move || store.get_session(&sid)).await {
            Ok(Ok(Some(s))) => s,
            _ => return Err((StatusCode::NOT_FOUND, "Session not found").into_response()),
        }
    };

    // Load message history.
    let messages = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        tokio::task::spawn_blocking(move || store.get_messages(&sid))
            .await
            .unwrap_or_else(|_| Ok(vec![]))
            .unwrap_or_default()
    };

    // Build history pairs (user/assistant only -- exclude tool records).
    let history: Vec<(String, String)> = messages.iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();

    // Find the last user message (the one to respond to).
    let last_user_msg = history.iter().rev()
        .find(|(role, _)| role == "user")
        .map(|(_, content)| content.clone())
        .unwrap_or_default();

    // History for the LLM: everything except the last user message.
    let llm_history: Vec<(String, String)> = if history.len() > 1 {
        history[..history.len() - 1].to_vec()
    } else {
        vec![]
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<agentos_kernel::ChatStreamEvent>(32);

    // Spawn inference in a background task.
    let kernel = state.kernel.clone();
    let agent_name = session.agent_name.clone();
    let chat_store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();

    tokio::spawn(async move {
        match kernel.chat_infer_streaming(&agent_name, &llm_history, &last_user_msg, tx).await {
            Ok(result) => {
                // Persist tool calls and answer.
                if !result.tool_calls.is_empty() {
                    let store = Arc::clone(&chat_store);
                    let s = sid.clone();
                    let calls = result.tool_calls.clone();
                    let _ = tokio::task::spawn_blocking(move || store.add_tool_calls(&s, &calls)).await;
                }
                let store = Arc::clone(&chat_store);
                let s = sid.clone();
                let answer = result.answer.clone();
                let _ = tokio::task::spawn_blocking(move || store.add_message(&s, "assistant", &answer)).await;
            }
            Err(e) => {
                tracing::error!("Chat streaming inference failed: {e}");
            }
        }
    });

    // Convert receiver into SSE stream.
    let stream = async_stream::stream! {
        while let Some(event) = rx.recv().await {
            let event_name = match &event {
                agentos_kernel::ChatStreamEvent::Thinking { .. } => "chat-thinking",
                agentos_kernel::ChatStreamEvent::ToolStart { .. } => "chat-tool-start",
                agentos_kernel::ChatStreamEvent::ToolResult { .. } => "chat-tool-result",
                agentos_kernel::ChatStreamEvent::Token { .. } => "chat-token",
                agentos_kernel::ChatStreamEvent::Done { .. } => "chat-done",
                agentos_kernel::ChatStreamEvent::Error { .. } => "chat-error",
            };
            let data = serde_json::to_string(&event).unwrap_or_default();
            yield Ok::<_, Infallible>(Event::default().event(event_name).data(data));
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
```

6. Open `crates/agentos-web/src/router.rs`. Add the new route (after line 138):

```rust
.route("/chat/{session_id}/stream", axum::routing::get(chat::message_stream))
```

7. Update the `send()` handler. Instead of performing inference and redirecting, save the user message and return an HTML fragment that tells HTMX to connect to the SSE stream:

```rust
pub async fn send(/* existing params */) -> Response {
    // ... existing validation and message persistence ...

    // Return an HTML fragment that HTMX will swap in.
    // This fragment starts the SSE connection to the stream endpoint.
    let html = format!(
        r#"<div id="chat-stream-target"
             hx-ext="sse"
             sse-connect="/chat/{session_id}/stream"
             sse-swap="chat-done"
             hx-swap="innerHTML">
            <div class="chat-thinking">
                <div class="chat-thinking-dots"><span></span><span></span><span></span></div>
                <span class="muted">Thinking...</span>
            </div>
        </div>"#,
    );
    axum::response::Html(html).into_response()
}
```

The `send()` handler now returns `200 OK` with an HTML partial instead of `303 See Other`. The HTMX form targets a div that replaces with this partial, which immediately opens the SSE connection.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/kernel.rs` | Add `ChatStreamEvent`, `chat_infer_streaming()` |
| `crates/agentos-kernel/src/lib.rs` | Re-export `ChatStreamEvent` |
| `crates/agentos-web/Cargo.toml` | Add `async-stream = "0.3"` |
| `crates/agentos-web/src/handlers/chat.rs` | Add `message_stream()` handler; update `send()` to return HTMX partial |
| `crates/agentos-web/src/router.rs` | Register `/chat/{session_id}/stream` route |

---

## Prerequisites

- [[28-01-Add Chat Tool Execution Loop to Kernel]] -- provides the tool loop logic and types.
- [[28-02-Extend ChatStore Schema for Tool Metadata]] -- provides `add_tool_calls()` for persistence.

---

## Test Plan

- `cargo build -p agentos-kernel -p agentos-web` must compile.
- Add test `test_chat_stream_event_serialization`: Serialize each `ChatStreamEvent` variant to JSON. Verify the `"type"` discriminator field is present (from `#[serde(tag = "type")]`).
- Manual test: Open chat, send a message. Verify the thinking dots appear immediately. Verify the SSE connection is established (check browser DevTools Network tab for the stream request). Verify events arrive and the final answer renders.
- The SSE stream must close after the `Done` or `Error` event (channel sender is dropped when the spawned task completes).

---

## Verification

```bash
cargo build -p agentos-kernel -p agentos-web
cargo test -p agentos-kernel -- chat_stream --nocapture
cargo test -p agentos-web -- --nocapture
cargo clippy -p agentos-kernel -p agentos-web -- -D warnings
```
