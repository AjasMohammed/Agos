use crate::state::AppState;
use agentos_kernel::kernel::ChatStreamEvent;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

/// Escapes HTML special characters to prevent XSS when embedding user content inline.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[derive(Deserialize)]
pub struct NewSessionForm {
    pub agent_name: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct SendForm {
    pub message: String,
}

/// GET /chat — session list + new session compose form.
pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Response {
    let sessions = {
        let store = Arc::clone(&state.chat_store);
        tokio::task::spawn_blocking(move || store.list_sessions())
            .await
            .unwrap_or_else(|_| Ok(vec![]))
            .unwrap_or_default()
    };

    let agents: Vec<_> = {
        let registry = state.kernel.agent_registry.read().await;
        registry
            .list_online()
            .iter()
            .map(|a| context! { name => a.name.clone(), model => a.model.clone() })
            .collect()
    };

    let sessions_ctx: Vec<_> = sessions
        .iter()
        .map(|s| {
            let preview = s
                .last_preview
                .as_deref()
                .map(|p| {
                    if p.chars().count() > 80 {
                        format!("{}…", p.chars().take(80).collect::<String>())
                    } else {
                        p.to_string()
                    }
                })
                .unwrap_or_default();
            context! {
                id => s.id.clone(),
                agent_name => s.agent_name.clone(),
                updated_at => s.updated_at.clone(),
                preview,
            }
        })
        .collect();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Chat",
        breadcrumbs => vec![context! { label => "Chat" }],
        sessions => sessions_ctx,
        agents,
        csrf_token,
    };
    super::render(&state.templates, "chat.html", ctx)
}

/// POST /chat/new — create a session and send the first message.
pub async fn new_session(
    State(state): State<AppState>,
    Form(form): Form<NewSessionForm>,
) -> Response {
    let message = form.message.trim().to_string();
    if message.is_empty() {
        return (StatusCode::BAD_REQUEST, "Message cannot be empty").into_response();
    }
    if message.len() > 32_768 {
        return (StatusCode::BAD_REQUEST, "Message too long (max 32 KB)").into_response();
    }
    let agent_name = form.agent_name.trim().to_string();
    if agent_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Select an agent").into_response();
    }
    if agent_name.len() > 256 {
        return (StatusCode::BAD_REQUEST, "Agent name too long").into_response();
    }

    // Validate the agent exists and is online before touching the database.
    {
        let registry = state.kernel.agent_registry.read().await;
        match registry.get_by_name(&agent_name) {
            Some(a) if a.status != agentos_types::AgentStatus::Offline => {}
            Some(_) => {
                return (StatusCode::BAD_REQUEST, "Agent is offline").into_response();
            }
            None => {
                return (StatusCode::BAD_REQUEST, "Agent not found").into_response();
            }
        }
    }

    // Create session and persist the first user message atomically.
    let session_id = {
        let store = Arc::clone(&state.chat_store);
        let agent = agent_name.clone();
        let msg = message.clone();
        match tokio::task::spawn_blocking(move || {
            store.create_session_with_first_message(&agent, &msg)
        })
        .await
        {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => {
                tracing::error!("Failed to create chat session: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to create session",
                )
                    .into_response();
            }
            Err(e) => {
                tracing::error!("spawn_blocking panicked: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
            }
        }
    };

    // Run inference directly against the agent's LLM (no task created).
    let result = match state
        .kernel
        .chat_infer_with_tools(&agent_name, &[], &message)
        .await
    {
        Ok(result) => {
            if !result.tool_calls.is_empty() {
                tracing::debug!(
                    iterations = result.iterations,
                    tool_calls = result.tool_calls.len(),
                    "Chat inference completed with tool calls"
                );
            }
            result
        }
        Err(e) => {
            tracing::error!("Chat inference failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                "Agent is unavailable. Check server logs.",
            )
                .into_response();
        }
    };

    // Save tool call records before the assistant message.
    if !result.tool_calls.is_empty() {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        let calls = result.tool_calls.clone();
        match tokio::task::spawn_blocking(move || store.add_tool_calls(&sid, &calls)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("Failed to save tool calls: {e}"),
            Err(e) => tracing::error!("spawn_blocking panicked saving tool calls: {e}"),
        }
    }

    // Save assistant response.
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    let response = result.answer.clone();
    if response.trim().is_empty() {
        tracing::warn!(
            target: "agentos::chat",
            session_id = %session_id,
            "Saving empty assistant response to chat store"
        );
    }
    tracing::info!(
        target: "agentos::chat",
        session_id = %session_id,
        answer_len = response.len(),
        iterations = result.iterations,
        tool_calls = result.tool_calls.len(),
        "Persisting chat assistant response"
    );
    match tokio::task::spawn_blocking(move || store.add_message(&sid, "assistant", &response)).await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::error!("Failed to save assistant response: {e}"),
        Err(e) => tracing::error!("spawn_blocking panicked saving assistant response: {e}"),
    }

    Redirect::to(&format!("/chat/{}", session_id)).into_response()
}

/// POST /chat/{session_id}/send — continue an existing session.
pub async fn send(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Form(form): Form<SendForm>,
) -> Response {
    // Reject non-UUID session IDs immediately — UUIDs are ASCII-only which also
    // makes subsequent byte-offset slicing safe.
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid session ID").into_response();
    }

    let message = form.message.trim().to_string();
    if message.is_empty() {
        return (StatusCode::BAD_REQUEST, "Message cannot be empty").into_response();
    }
    if message.len() > 32_768 {
        return (StatusCode::BAD_REQUEST, "Message too long (max 32 KB)").into_response();
    }

    // Verify the session exists (returns 404 if not).
    {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        match tokio::task::spawn_blocking(move || store.get_session(&sid)).await {
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "Session not found").into_response(),
            _ => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response(),
        }
    }

    // Persist user message before returning the partial. Required — not best-effort.
    {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        let msg = message.clone();
        match tokio::task::spawn_blocking(move || store.add_message(&sid, "user", &msg)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!("Failed to save user message: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to save message")
                    .into_response();
            }
            Err(e) => {
                tracing::error!("spawn_blocking panicked saving user message: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
            }
        }
    }

    // Return an HTMX partial that renders the user message and starts the SSE stream.
    // Inference happens asynchronously in the SSE endpoint.
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

/// GET /chat/{session_id}/stream — SSE endpoint that streams inference progress.
///
/// The browser connects here after `POST /send` returns the HTMX partial.
/// Sends typed events: `chat-thinking`, `chat-tool-start`, `chat-tool-result`, `chat-done`.
/// The `chat-done` event data is rendered HTML that HTMX swaps into the page.
pub async fn message_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, Response> {
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid session ID").into_response());
    }

    // Load session info.
    let session = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        match tokio::task::spawn_blocking(move || store.get_session(&sid)).await {
            Ok(Ok(Some(s))) => s,
            Ok(Ok(None)) => {
                return Err((StatusCode::NOT_FOUND, "Session not found").into_response())
            }
            _ => return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()),
        }
    };

    // Load history — only user/assistant roles are fed to the LLM.
    let all_messages: Vec<(String, String)> = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        tokio::task::spawn_blocking(move || store.get_messages(&sid))
            .await
            .unwrap_or_else(|_| Ok(vec![]))
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| (m.role, m.content))
            .collect()
    };

    // Find the index of the last user message — that's the one we're responding to.
    let last_user_idx = match all_messages.iter().rposition(|(role, _)| role == "user") {
        Some(idx) => idx,
        None => {
            return Err((StatusCode::BAD_REQUEST, "No user message to respond to").into_response())
        }
    };

    let last_user_msg = all_messages[last_user_idx].1.clone();

    // History fed to the LLM is everything except the last user message.
    let history: Vec<(String, String)> = all_messages
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != last_user_idx)
        .map(|(_, m)| m.clone())
        .collect();

    let (tx, rx) = tokio::sync::mpsc::channel::<ChatStreamEvent>(32);
    // Clone tx so the spawned task can emit an Error event on pre-loop failures
    // (e.g. agent not found, no LLM adapter) that the kernel method doesn't cover.
    let tx_err = tx.clone();

    let kernel = state.kernel.clone();
    let agent_name = session.agent_name.clone();
    let chat_store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();

    tokio::spawn(async move {
        match kernel
            .chat_infer_streaming(&agent_name, &history, &last_user_msg, tx)
            .await
        {
            Ok(result) => {
                // Done event has already been sent; drop the backup sender so the SSE
                // stream closes immediately rather than waiting for DB persistence.
                drop(tx_err);
                // Save tool calls before the assistant message.
                if !result.tool_calls.is_empty() {
                    let store = Arc::clone(&chat_store);
                    let calls = result.tool_calls.clone();
                    let s = sid.clone();
                    match tokio::task::spawn_blocking(move || store.add_tool_calls(&s, &calls))
                        .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::error!("Failed to save chat tool calls: {e}"),
                        Err(e) => tracing::error!("spawn_blocking panicked saving tool calls: {e}"),
                    }
                }
                // Save assistant response.
                let store = Arc::clone(&chat_store);
                let answer = result.answer.clone();
                if answer.trim().is_empty() {
                    tracing::warn!(
                        target: "agentos::chat",
                        session_id = %sid,
                        "Saving empty streaming assistant response to chat store"
                    );
                }
                tracing::info!(
                    target: "agentos::chat",
                    session_id = %sid,
                    answer_len = answer.len(),
                    iterations = result.iterations,
                    tool_calls = result.tool_calls.len(),
                    "Persisting streaming chat assistant response"
                );
                match tokio::task::spawn_blocking(move || {
                    store.add_message(&sid, "assistant", &answer)
                })
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::error!("Failed to save assistant message: {e}"),
                    Err(e) => tracing::error!("spawn_blocking panicked saving assistant: {e}"),
                }
            }
            Err(e) => {
                // Send the error so the client sees it rather than hanging on "Thinking..."
                let _ = tx_err
                    .send(ChatStreamEvent::Error { message: e.clone() })
                    .await;
                tracing::error!("Streaming chat inference failed: {e}");
            }
        }
    });

    // Convert the mpsc receiver to a futures Stream and map each event to an SSE Event.
    let agent_name_for_stream = session.agent_name.clone();
    let stream = ReceiverStream::new(rx).map(move |event| {
        let (event_name, data) = match &event {
            ChatStreamEvent::Thinking { .. } => (
                "chat-thinking",
                serde_json::to_string(&event).unwrap_or_default(),
            ),
            ChatStreamEvent::ToolStart { .. } => (
                "chat-tool-start",
                serde_json::to_string(&event).unwrap_or_default(),
            ),
            ChatStreamEvent::ToolResult { .. } => (
                "chat-tool-result",
                serde_json::to_string(&event).unwrap_or_default(),
            ),
            ChatStreamEvent::Done { answer, .. } => {
                let initial = agent_name_for_stream
                    .chars()
                    .next()
                    .unwrap_or('A')
                    .to_uppercase()
                    .to_string();
                let html = format!(
                    r#"<div class="chat-row chat-row-agent">
                        <div class="chat-agent-avatar" aria-hidden="true">{}</div>
                        <div class="chat-agent-column">
                            <div class="chat-agent-name muted">{}</div>
                            <div class="chat-bubble chat-bubble-agent">
                                <div class="chat-bubble-content-agent">{}</div>
                            </div>
                        </div>
                    </div>"#,
                    html_escape(&initial),
                    html_escape(&agent_name_for_stream),
                    html_escape(answer),
                );
                ("chat-done", html)
            }
            ChatStreamEvent::Error { message } => {
                let html = format!(
                    r#"<div class="chat-row chat-row-agent">
                        <div class="chat-agent-column">
                            <div class="chat-bubble chat-bubble-agent">
                                <div class="chat-bubble-content-agent" style="color:var(--pico-color-red-500)">
                                    Error: {}
                                </div>
                            </div>
                        </div>
                    </div>"#,
                    html_escape(message),
                );
                ("chat-done", html)
            }
        };
        Ok::<_, Infallible>(Event::default().event(event_name).data(data))
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// GET /chat/{session_id} — full message history for a session.
pub async fn conversation(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    jar: CookieJar,
) -> Response {
    // Reject non-UUID session IDs — UUIDs are ASCII-only so byte-offset slicing is safe.
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid session ID").into_response();
    }

    let session = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        match tokio::task::spawn_blocking(move || store.get_session(&sid)).await {
            Ok(Ok(Some(s))) => s,
            Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "Session not found").into_response(),
            _ => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response(),
        }
    };

    let messages: Vec<_> = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        tokio::task::spawn_blocking(move || store.get_messages(&sid))
            .await
            .unwrap_or_else(|_| Ok(vec![]))
            .unwrap_or_default()
            .into_iter()
            .map(|m| {
                context! {
                    role => m.role,
                    content => m.content,
                    created_at => m.created_at,
                    tool_name => m.tool_name,
                    tool_duration_ms => m.tool_duration_ms,
                }
            })
            .collect()
    };

    // session_id is a validated UUID (ASCII), so slicing at byte offset 8 is safe.
    let short_id = &session_id[..8];
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => format!("Chat — {}", short_id),
        breadcrumbs => vec![
            context! { label => "Chat", href => "/chat" },
            context! { label => short_id },
        ],
        session_id,
        agent_name => session.agent_name,
        messages,
        csrf_token,
    };
    super::render(&state.templates, "chat_conversation.html", ctx)
}
