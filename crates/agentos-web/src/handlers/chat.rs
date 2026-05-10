use crate::auth::file_owner_principal;
use crate::auth::AuthToken;
use crate::chat_inflight::InFlightInference;
use crate::handlers::files;
use crate::state::AppState;
use agentos_kernel::kernel::ChatStreamEvent;
use agentos_types::ContentPart;
use axum::extract::{Extension, Form, Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
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
    #[serde(default)]
    pub file_ids: Option<String>,
}

#[derive(Deserialize)]
pub struct SendForm {
    pub message: String,
    #[serde(default)]
    pub file_ids: Option<String>,
}

#[derive(Deserialize)]
pub struct RenameSessionForm {
    pub title: Option<String>,
}

#[derive(Deserialize)]
pub struct ForkSessionForm {
    pub title: Option<String>,
}

fn spawn_streaming_inference(
    state: &AppState,
    session_id: String,
    agent_name: String,
    history: Vec<(String, String)>,
    user_msg: String,
    user_parts: Option<Vec<ContentPart>>,
    inflight_handle: Arc<InFlightInference>,
) {
    let mut total_len = 0;
    for (idx, (role, text)) in history.iter().enumerate() {
        total_len += text.len();
        tracing::info!(target: "agentos::chat::debug", "History [{}] {}: {} chars", idx, role, text.len());
    }
    tracing::info!(target: "agentos::chat::debug", "New User Msg: {} chars", user_msg.len());
    tracing::info!(target: "agentos::chat::debug", "Total Context Est: {} chars", total_len + user_msg.len());
    tracing::info!(target: "agentos::chat::debug", "\n=== FULL LLM INPUT DUMP ===\nHistory: {:#?}\nNew Message: {}\n===========================\n", history, user_msg);

    let kernel = state.kernel.clone();
    let chat_store = Arc::clone(&state.chat_store);
    let inflight_map = Arc::clone(&state.inflight_chat);

    let inflight_for_task = Arc::clone(&inflight_handle);
    let task = tokio::spawn(async move {
        let (kernel_tx, mut kernel_rx) = tokio::sync::mpsc::channel::<ChatStreamEvent>(64);
        let forwarder = {
            let inflight = Arc::clone(&inflight_for_task);
            tokio::spawn(async move {
                while let Some(event) = kernel_rx.recv().await {
                    inflight.push(event).await;
                }
            })
        };

        let result = kernel
            .chat_infer_streaming(
                &agent_name,
                &history,
                &user_msg,
                user_parts,
                kernel_tx,
                Some(&session_id),
            )
            .await;
        let _ = forwarder.await;

        match result {
            Ok(inf) => {
                if !inf.tool_calls.is_empty() {
                    let store = Arc::clone(&chat_store);
                    let sid = session_id.clone();
                    let calls = inf.tool_calls.clone();
                    match tokio::task::spawn_blocking(move || store.add_tool_calls(&sid, &calls))
                        .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::error!("Failed to save chat tool calls: {e}"),
                        Err(e) => tracing::error!("spawn_blocking panicked saving tool calls: {e}"),
                    }
                }

                let store = Arc::clone(&chat_store);
                let sid = session_id.clone();
                let answer = inf.answer.clone();
                let tokens_used = inf.tokens_used;
                let cost_usd = inf.cost_usd;
                match tokio::task::spawn_blocking(move || {
                    store.add_assistant_message(
                        &sid,
                        &answer,
                        Some(tokens_used),
                        if cost_usd.is_finite() && cost_usd > 0.0 {
                            Some(cost_usd)
                        } else {
                            None
                        },
                    )
                })
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::error!("Failed to save assistant message: {e}"),
                    Err(e) => tracing::error!("spawn_blocking panicked saving assistant: {e}"),
                }
                tracing::info!(
                    target: "agentos::chat",
                    session_id = %session_id,
                    answer_len = inf.answer.len(),
                    iterations = inf.iterations,
                    tool_calls = inf.tool_calls.len(),
                    "Streaming chat completed and persisted"
                );
            }
            Err(e) => {
                inflight_for_task
                    .push(ChatStreamEvent::Error { message: e.clone() })
                    .await;
                tracing::error!("Streaming chat inference failed: {e}");
            }
        }

        inflight_for_task.mark_done().await;
        inflight_map.schedule_cleanup(session_id);
    });
    inflight_handle.set_task_handle(task);
}

/// Build LLM user content: optional attached files, then @mentions, then message body.
/// Returns display text for logging/history and optional multimodal parts for the kernel.
async fn expand_user_message_for_llm(
    content: &str,
    file_ids: Option<&str>,
    state: &AppState,
    owner_principal: &str,
    session_id: Option<&str>,
    agent_name: &str,
) -> (String, Option<Vec<ContentPart>>) {
    let supports_images = {
        let reg = state.kernel.agent_registry.read().await;
        match reg.get_by_name(agent_name) {
            Some(agent) => {
                let aid = agent.id;
                drop(reg);
                state
                    .kernel
                    .active_llms
                    .read()
                    .await
                    .get(&aid)
                    .map(|llm| llm.supports_images())
                    .unwrap_or(false)
            }
            None => false,
        }
    };

    let with_mentions =
        files::resolve_at_mentions(content, state, owner_principal, session_id).await;

    let file_parts = match file_ids {
        Some(ids) if !ids.trim().is_empty() => {
            files::resolve_file_ids_to_context(ids, state, owner_principal, supports_images).await
        }
        _ => Vec::new(),
    };

    if file_parts.is_empty() {
        return (with_mentions, None);
    }

    let mut parts: Vec<ContentPart> = vec![ContentPart::Text {
        text: with_mentions,
    }];
    parts.extend(file_parts);

    let display = parts_display_for_chat_log(&parts);
    (display, Some(parts))
}

fn parts_display_for_chat_log(parts: &[ContentPart]) -> String {
    let mut s = String::new();
    for p in parts {
        match p {
            ContentPart::Text { text } => s.push_str(text),
            ContentPart::Image { .. } => {
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push_str("[image attachment]\n");
            }
        }
    }
    s
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

    let agents: Vec<_> = match state.service.list_agents().await {
        Ok(list) => list
            .iter()
            .map(|a| context! { name => a.name.clone(), model => a.model.clone() })
            .collect(),
        Err(e) => {
            tracing::error!("Failed to list agents for chat: {e}");
            vec![]
        }
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
                title => s.title.clone(),
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

/// POST /chat/{session_id}/rename — rename a session title.
pub async fn rename_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Form(form): Form<RenameSessionForm>,
) -> Response {
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid session ID").into_response();
    }
    let title = form.title.as_deref().map(str::trim).unwrap_or("");
    if title.len() > 120 {
        return (StatusCode::BAD_REQUEST, "Title too long (max 120 chars)").into_response();
    }
    let title_opt = if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    };
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    match tokio::task::spawn_blocking(move || store.rename_session(&sid, title_opt.as_deref()))
        .await
    {
        Ok(Ok(())) => Redirect::to(&format!("/chat/{}", session_id)).into_response(),
        Ok(Err(rusqlite::Error::QueryReturnedNoRows)) => {
            (StatusCode::NOT_FOUND, "Session not found").into_response()
        }
        Ok(Err(e)) => {
            tracing::error!(session_id = %session_id, error = %e, "Failed to rename session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to rename session",
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(session_id = %session_id, error = %e, "Rename session task failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}

/// POST /chat/{session_id}/delete — delete a chat session.
pub async fn delete_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid session ID").into_response();
    }
    let is_htmx = headers.get("HX-Request").and_then(|v| v.to_str().ok()) == Some("true");
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    match tokio::task::spawn_blocking(move || store.delete_session(&sid)).await {
        Ok(Ok(())) => {
            state.kernel.forget_chat_session_dedup(&session_id).await;
            if is_htmx {
                StatusCode::OK.into_response()
            } else {
                Redirect::to("/chat").into_response()
            }
        }
        Ok(Err(rusqlite::Error::QueryReturnedNoRows)) => {
            if is_htmx {
                // Already deleted (double-click) — row is already gone, treat as success.
                StatusCode::OK.into_response()
            } else {
                (StatusCode::NOT_FOUND, "Session not found").into_response()
            }
        }
        Ok(Err(e)) => {
            tracing::error!(session_id = %session_id, error = %e, "Failed to delete session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to delete session",
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(session_id = %session_id, error = %e, "Delete session task failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}

/// POST /chat/{session_id}/fork — duplicate a session into a new one.
pub async fn fork_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Form(form): Form<ForkSessionForm>,
) -> Response {
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid session ID").into_response();
    }
    let title = form.title.as_deref().map(str::trim).unwrap_or("");
    if title.len() > 120 {
        return (StatusCode::BAD_REQUEST, "Title too long (max 120 chars)").into_response();
    }
    let title_opt = if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    };
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    match tokio::task::spawn_blocking(move || store.fork_session(&sid, title_opt.as_deref())).await
    {
        Ok(Ok(new_id)) => Redirect::to(&format!("/chat/{}", new_id)).into_response(),
        Ok(Err(rusqlite::Error::QueryReturnedNoRows)) => {
            (StatusCode::NOT_FOUND, "Session not found").into_response()
        }
        Ok(Err(e)) => {
            tracing::error!(session_id = %session_id, error = %e, "Failed to fork session");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fork session").into_response()
        }
        Err(e) => {
            tracing::error!(session_id = %session_id, error = %e, "Fork session task failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}

/// GET /chat/{session_id}/export — export a session as markdown.
pub async fn export_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid session ID").into_response();
    }
    let (session, messages) = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        match tokio::task::spawn_blocking(move || {
            let session = store.get_session(&sid)?;
            let messages = store.get_messages(&sid)?;
            Ok::<_, rusqlite::Error>((session, messages))
        })
        .await
        {
            Ok(Ok((Some(s), m))) => (s, m),
            Ok(Ok((None, _))) => {
                return (StatusCode::NOT_FOUND, "Session not found").into_response()
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %session_id, error = %e, "Failed to export session");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to export session",
                )
                    .into_response();
            }
            Err(e) => {
                tracing::error!(session_id = %session_id, error = %e, "Export session task failed");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
            }
        }
    };

    let mut out = String::new();
    let title = session
        .title
        .clone()
        .unwrap_or_else(|| format!("Chat with {}", session.agent_name));
    out.push_str(&format!("# {}\n\n", title));
    for msg in messages {
        match msg.role.as_str() {
            "user" => out.push_str("## You\n\n"),
            "assistant" => out.push_str(&format!("## {}\n\n", session.agent_name)),
            "tool" => {
                let tool_name = msg.tool_name.unwrap_or_else(|| "tool".to_string());
                out.push_str(&format!("### Tool: {}\n\n", tool_name));
                if let Some(payload) = msg.tool_payload_json {
                    out.push_str("#### Input\n\n```json\n");
                    out.push_str(&payload);
                    out.push_str("\n```\n\n");
                }
                if let Some(result) = msg.tool_result_json {
                    out.push_str("#### Result\n\n```json\n");
                    out.push_str(&result);
                    out.push_str("\n```\n\n");
                }
            }
            _ => out.push_str("## Message\n\n"),
        }
        if msg.role != "tool" {
            out.push_str(&msg.content);
            out.push_str("\n\n");
        }
    }

    let mut response = out.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    let filename = format!("chat-{}.md", &session_id[..8]);
    let disposition = format!("attachment; filename=\"{}\"", filename);
    if let Ok(h) = axum::http::HeaderValue::from_str(&disposition) {
        response.headers_mut().insert(CONTENT_DISPOSITION, h);
    }
    response
}

/// POST /chat/new — create a session and send the first message.
pub async fn new_session(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
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
        let agents = match state.service.list_agents().await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("Failed to list agents for validation: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
            }
        };
        match agents.iter().find(|a| a.name == agent_name) {
            Some(a) if a.status != "offline" => {}
            Some(_) => {
                return (StatusCode::BAD_REQUEST, "Agent is offline").into_response();
            }
            None => {
                return (StatusCode::BAD_REQUEST, "Agent not found").into_response();
            }
        }
    }

    let principal = file_owner_principal(&jar, &headers, &auth);
    let file_ids_opt = form
        .file_ids
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Resolve @mentions and attached file IDs into LLM context.
    // The original message is what gets stored in the DB; the expanded version goes to inference.
    let (llm_message, user_parts) = expand_user_message_for_llm(
        &message,
        file_ids_opt,
        &state,
        &principal,
        None,
        &agent_name,
    )
    .await;

    // Create session and persist the first user message atomically.
    let session_id = {
        let store = Arc::clone(&state.chat_store);
        let agent = agent_name.clone();
        let msg = message.clone();
        let fid = file_ids_opt.map(|s| s.to_string());
        match tokio::task::spawn_blocking(move || {
            store.create_session_with_first_message(&agent, &msg, fid.as_deref())
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

    let inflight = match state.inflight_chat.try_start(&session_id) {
        Some(h) => h,
        None => {
            return (
                StatusCode::CONFLICT,
                "A reply is already being generated for this session",
            )
                .into_response();
        }
    };

    // First message has no prior user/assistant history (excluding the just-persisted user turn).
    spawn_streaming_inference(
        &state,
        session_id.clone(),
        agent_name.clone(),
        Vec::new(),
        llm_message,
        user_parts,
        inflight,
    );

    Redirect::to(&format!("/chat/{}", session_id)).into_response()
}

/// POST /chat/{session_id}/send — continue an existing session.
///
/// Reserves an in-flight inference slot for the session, persists the user message,
/// then spawns a detached inference task that streams events into the in-flight buffer.
/// The HTMX partial returned by this handler opens an EventSource to `/stream` which
/// attaches to that buffer. A refresh of the page before or during streaming no longer
/// orphans the user message — the spawned task runs to completion independently, and
/// a reconnect can replay the events from cursor 0.
pub async fn send(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    Form(form): Form<SendForm>,
) -> Response {
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

    let principal = file_owner_principal(&jar, &headers, &auth);
    let file_ids_opt = form
        .file_ids
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let session = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        match tokio::task::spawn_blocking(move || store.get_session(&sid)).await {
            Ok(Ok(Some(s))) => s,
            Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "Session not found").into_response(),
            _ => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response(),
        }
    };

    let (llm_message, user_parts) = expand_user_message_for_llm(
        &message,
        file_ids_opt,
        &state,
        &principal,
        Some(&session_id),
        &session.agent_name,
    )
    .await;

    // Reserve the slot BEFORE persisting so two concurrent /send requests for the same
    // session cannot both kick off an inference or double-persist a user message.
    let inflight = match state.inflight_chat.try_start(&session_id) {
        Some(h) => h,
        None => {
            return (
                StatusCode::CONFLICT,
                "A reply is already being generated for this session",
            )
                .into_response();
        }
    };

    // Load prior messages for LLM context (everything before the one we're about to
    // insert). Only user/assistant roles feed the model. Re-expand prior user attachments.
    let prior_msgs = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        match tokio::task::spawn_blocking(move || store.get_messages(&sid)).await {
            Ok(Ok(msgs)) => msgs,
            _ => {
                state.inflight_chat.abandon(&session_id);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load history")
                    .into_response();
            }
        }
    };

    // Replay non-meta tool calls into the LLM history so the model remembers
    // what it has already done in prior turns. Without this, follow-up
    // messages start blind and the model rediscovers the same tool via
    // `agent-manual`/`describe-tool` every turn (see logs 2026-05-08T08:04).
    // Meta tools are skipped — they're discovery scaffolding, not actions.
    const MAX_SUMMARIES_PER_TURN: usize = 5;
    const MAX_SUMMARY_BYTES_PER_TURN: usize = 2048;
    const REDACTED_TOOL_NAMES: &[&str] = &[
        "vault-set",
        "vault-get",
        "vault-list",
        "secrets-set",
        "secrets-get",
        "secrets-list",
        "mcp-oauth-store",
        // Env / shell tools commonly carry credentials in args.
        "shell-exec",
        "proc-spawn",
        "host-package-install",
        // Network tools whose args / results frequently contain auth headers,
        // signed URLs, or response bodies returning bearer tokens.
        "net-http",
        "web-fetch",
        "http-client",
    ];
    // Whole-key match (case-insensitive). Substring matching previously caught
    // `auth` inside `author` and similar false positives. JSON keys are usually
    // snake_case; we also tokenize on `-`, `_` and dot to handle headers like
    // `Authorization` and dotted paths like `headers.authorization`.
    const SENSITIVE_KEY_TOKENS: &[&str] = &[
        "token",
        "secret",
        "secrets",
        "api_key",
        "apikey",
        "password",
        "passwd",
        "passphrase",
        "authorization",
        "cookie",
        "credential",
        "credentials",
        "private_key",
        "privatekey",
        "bearer",
        "access_key",
        "access_token",
        "refresh_token",
        "client_secret",
        "x-api-key",
        "set-cookie",
    ];
    fn truncate_str(s: &str, max_chars: usize) -> String {
        if s.chars().count() <= max_chars {
            return s.to_string();
        }
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
    fn key_is_sensitive(key: &str, tokens: &[&str]) -> bool {
        let lower = key.to_ascii_lowercase();
        if tokens.iter().any(|t| t.eq_ignore_ascii_case(&lower)) {
            return true;
        }
        // Tokenize on common separators and check exact-match against any
        // token. Catches `headers.Authorization`, `auth_token`, `x-api-key`.
        for piece in lower.split(['.', '_', '-', ' ']) {
            if tokens.iter().any(|t| t.eq_ignore_ascii_case(piece)) {
                return true;
            }
        }
        false
    }
    fn redact_json_in_place(v: &mut serde_json::Value, sensitive_tokens: &[&str]) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    if key_is_sensitive(k, sensitive_tokens) {
                        *val = serde_json::Value::String("[redacted]".to_string());
                    } else {
                        redact_json_in_place(val, sensitive_tokens);
                    }
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr.iter_mut() {
                    redact_json_in_place(item, sensitive_tokens);
                }
            }
            _ => {}
        }
    }
    fn render_redacted(raw: &str, sensitive_tokens: &[&str]) -> String {
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(mut v) => {
                redact_json_in_place(&mut v, sensitive_tokens);
                v.to_string()
            }
            Err(_) => raw.replace('\n', "\\n").replace('\r', "\\r"),
        }
    }

    let mut history: Vec<(String, String)> = Vec::new();
    let mut pending_tool_summaries: Vec<String> = Vec::new();
    let mut pending_tool_summaries_bytes: usize = 0;
    // Most recent meta-tool (agent-manual / search-tools / describe-tool / list-tools)
    // result *within the current block*. Keeping just the latest one is enough to
    // remind the model what it just discovered without bloating context with stale
    // exploration. Reset on each flush.
    let mut pending_meta_summary: Option<String> = None;
    const MAX_META_RESULT_CHARS: usize = 600;
    let flush_to_assistant = |history: &mut Vec<(String, String)>,
                              pending: &mut Vec<String>,
                              pending_bytes: &mut usize,
                              meta_pending: &mut Option<String>,
                              assistant_content: Option<String>| {
        let mut all: Vec<String> = Vec::with_capacity(pending.len() + 1);
        all.append(pending);
        if let Some(meta) = meta_pending.take() {
            all.push(meta);
        }
        *pending_bytes = 0;
        if all.is_empty() {
            if let Some(content) = assistant_content {
                history.push(("assistant".to_string(), content));
            }
            return;
        }
        let prefix = all.join("\n");
        let merged = match assistant_content {
            Some(content) if content.is_empty() => prefix,
            Some(content) => format!("{prefix}\n\n{content}"),
            None => prefix,
        };
        history.push(("assistant".to_string(), merged));
    };
    for m in prior_msgs {
        match m.role.as_str() {
            "tool" => {
                let name = match m.tool_name.as_deref() {
                    Some(n) if !n.is_empty() => n,
                    _ => continue,
                };
                let args_raw = m.tool_payload_json.as_deref().unwrap_or("{}");
                let result_raw = m.tool_result_json.as_deref().unwrap_or("");
                let (args_rendered, result_rendered) = if REDACTED_TOOL_NAMES.contains(&name) {
                    ("[redacted]".to_string(), "[redacted]".to_string())
                } else {
                    (
                        render_redacted(args_raw, SENSITIVE_KEY_TOKENS),
                        render_redacted(result_raw, SENSITIVE_KEY_TOKENS),
                    )
                };
                let status = if m.tool_success.unwrap_or(true) {
                    "ok"
                } else {
                    "FAILED"
                };
                if agentos_tools::META_TOOL_NAMES.contains(&name) {
                    // Keep ONLY the most recent meta-tool result per block. The
                    // result_short slice is capped tighter than non-meta calls
                    // because the model just needs the discovered tool list,
                    // not the whole catalogue. Without this, follow-up turns
                    // see no record of what the prior turn already discovered
                    // and rerun `agent-manual`/`search-tools` from scratch.
                    let args_short = truncate_str(&args_rendered, 120);
                    let result_short = truncate_str(&result_rendered, MAX_META_RESULT_CHARS);
                    pending_meta_summary = Some(format!(
                        "[Prior discovery: {name} ({status}) args=`{args_short}` result=`{result_short}`]"
                    ));
                    continue;
                }
                if pending_tool_summaries.len() >= MAX_SUMMARIES_PER_TURN
                    || pending_tool_summaries_bytes >= MAX_SUMMARY_BYTES_PER_TURN
                {
                    continue;
                }
                let args_short = truncate_str(&args_rendered, 200);
                let result_short = truncate_str(&result_rendered, 400);
                let line = format!(
                    "[Prior tool call: {name} ({status}) args=`{args_short}` result=`{result_short}`]"
                );
                pending_tool_summaries_bytes += line.len();
                pending_tool_summaries.push(line);
            }
            "user" => {
                if !pending_tool_summaries.is_empty() || pending_meta_summary.is_some() {
                    flush_to_assistant(
                        &mut history,
                        &mut pending_tool_summaries,
                        &mut pending_tool_summaries_bytes,
                        &mut pending_meta_summary,
                        None,
                    );
                }
                let text = expand_user_message_for_llm(
                    &m.content,
                    m.file_ids.as_deref(),
                    &state,
                    &principal,
                    Some(&session_id),
                    &session.agent_name,
                )
                .await
                .0;
                history.push(("user".to_string(), text));
            }
            "assistant" => {
                flush_to_assistant(
                    &mut history,
                    &mut pending_tool_summaries,
                    &mut pending_tool_summaries_bytes,
                    &mut pending_meta_summary,
                    Some(m.content),
                );
            }
            _ => continue,
        }
    }
    if !pending_tool_summaries.is_empty() || pending_meta_summary.is_some() {
        flush_to_assistant(
            &mut history,
            &mut pending_tool_summaries,
            &mut pending_tool_summaries_bytes,
            &mut pending_meta_summary,
            None,
        );
    }

    // Persist the user message now that the slot is reserved.
    {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        let msg = message.clone();
        let fid = file_ids_opt.map(|s| s.to_string());
        match tokio::task::spawn_blocking(move || {
            store.add_message(&sid, "user", &msg, fid.as_deref())
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!("Failed to save user message: {e}");
                state.inflight_chat.abandon(&session_id);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to save message")
                    .into_response();
            }
            Err(e) => {
                tracing::error!("spawn_blocking panicked saving user message: {e}");
                state.inflight_chat.abandon(&session_id);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
            }
        }
    }

    // Spawn the detached inference task. It owns `inflight` and will run to completion
    // regardless of whether a browser is currently subscribed via /stream.
    spawn_streaming_inference(
        &state,
        session_id.clone(),
        session.agent_name.clone(),
        history,
        llm_message,
        user_parts,
        Arc::clone(&inflight),
    );

    // Return the HTMX partial: user bubble + streaming target. The shared script at
    // /static/js/chat-stream.js reads the data-* attributes and attaches to /stream.
    let agent_initial = session
        .agent_name
        .chars()
        .next()
        .unwrap_or('A')
        .to_uppercase()
        .to_string();
    // Each /send partial gets a unique target so multiple turns on the same page do
    // not collide on shared element IDs. chat-stream.js uses data-role selectors to
    // pick up targets that have not yet been attached.
    let html = format!(
        r#"<div class="chat-row chat-row-user">
    <div class="chat-bubble chat-bubble-user">
        <div class="chat-speaker-tag">You</div>
        <div class="chat-bubble-content">{user_msg}</div>
    </div>
</div>
<div data-role="chat-stream-target"
     data-session-id="{session_id}"
     data-agent-name="{agent_name}"
     data-agent-initial="{agent_initial}">
    <div class="chat-thinking" data-role="chat-thinking-indicator">
        <div class="chat-thinking-dots"><span></span><span></span><span></span></div>
        <span class="muted">Thinking...</span>
    </div>
    <div data-role="chat-stream-response" class="chat-row chat-row-agent" style="display:none;">
        <div class="chat-agent-avatar" aria-hidden="true">{agent_initial}</div>
        <div class="chat-agent-column">
            <div class="chat-agent-name">{agent_name}</div>
            <div class="chat-bubble chat-bubble-agent">
                <div data-role="chat-stream-content" class="chat-stream-content chat-streaming"></div>
                <div class="chat-bubble-meta chat-bubble-meta-left chat-stream-meta" data-role="chat-stream-meta" style="display:none;">
                    <span data-role="chat-stream-time">Streaming...</span>
                    <span data-role="chat-stream-tokens"></span>
                    <span data-role="chat-stream-cost"></span>
                    <button class="chat-msg-action" type="button"
                            data-role="chat-stream-stop"
                            data-stop-url="/chat/{session_id}/stop">Stop</button>
                    <button class="chat-msg-action" type="button" data-role="chat-stream-copy">Copy</button>
                </div>
            </div>
        </div>
    </div>
</div>"#,
        user_msg = html_escape(&message),
        session_id = html_escape(&session_id),
        agent_name = html_escape(&session.agent_name),
        agent_initial = html_escape(&agent_initial),
    );
    Html(html).into_response()
}

/// POST /chat/{session_id}/stop — abort the in-flight streaming response for a chat.
pub async fn stop(State(state): State<AppState>, Path(session_id): Path<String>) -> Response {
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid session ID").into_response();
    }

    let Some(inflight) = state.inflight_chat.get(&session_id) else {
        return (StatusCode::CONFLICT, "No response is currently streaming").into_response();
    };

    let stopped_message = "_Stopped by user._";
    let stopped = inflight.cancel(stopped_message).await;
    if stopped {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            store.add_assistant_message(&sid, stopped_message, None, None)
        })
        .await
        .unwrap_or_else(|join_err| {
            tracing::error!(error = %join_err, "Stopping chat failed while persisting stop marker");
            Ok(())
        }) {
            tracing::error!(session_id = %session_id, error = %e, "Failed to persist chat stop marker");
        }
        state.inflight_chat.schedule_cleanup(session_id);
    }

    let mut response = StatusCode::OK.into_response();
    response.headers_mut().insert(
        "HX-Trigger",
        axum::http::HeaderValue::from_static(
            r#"{"showToast":{"message":"Chat stopped","type":"info"}}"#,
        ),
    );
    response
}

/// GET /chat/{session_id}/stream — SSE endpoint that attaches to an in-flight inference.
///
/// Does NOT run inference itself — that is owned by the task spawned from `send`. This
/// handler looks up the session's in-flight entry, replays buffered events from cursor
/// 0 (so a refresh sees the full stream, not just whatever comes after connection), and
/// then blocks until the task pushes more events or marks itself done.
///
/// Returns 410 Gone when no entry exists — that's the signal to the client that the
/// inference is not running and the page should show whatever is already in the DB.
pub async fn message_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, Response> {
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid session ID").into_response());
    }

    let inflight: Arc<InFlightInference> = match state.inflight_chat.get(&session_id) {
        Some(h) => h,
        None => {
            return Err(
                (StatusCode::GONE, "No in-flight inference for this session").into_response(),
            );
        }
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<ChatStreamEvent>(64);
    {
        let inflight = Arc::clone(&inflight);
        tokio::spawn(async move {
            inflight.subscribe_events(tx).await;
        });
    }

    use agentos_types::ChatStreamFrame;

    let stream = ReceiverStream::new(rx).map(move |event| {
        let frame = match event {
            ChatStreamEvent::Thinking { iteration } => ChatStreamFrame::Thinking { iteration },
            ChatStreamEvent::TextChunk { text } => ChatStreamFrame::TextDelta { text },
            ChatStreamEvent::ToolStart {
                tool_name,
                iteration,
            } => ChatStreamFrame::ToolStart {
                tool_name,
                iteration,
            },
            ChatStreamEvent::ToolResult {
                tool_name,
                result_preview,
                duration_ms,
                success,
            } => ChatStreamFrame::ToolResult {
                tool_name,
                result_preview,
                duration_ms,
                success,
            },
            ChatStreamEvent::Done {
                answer,
                iterations,
                tokens_used,
                cost_usd,
                ..
            } => ChatStreamFrame::Done {
                answer,
                iterations,
                tokens_used: Some(tokens_used),
                cost_usd: if cost_usd.is_finite() && cost_usd > 0.0 {
                    Some(cost_usd)
                } else {
                    None
                },
            },
            ChatStreamEvent::Error { message } => ChatStreamFrame::Error { message },
        };
        let data = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, Infallible>(Event::default().event("chat-stream").data(data))
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

    use crate::chat_store::TimelineEntry;

    let timeline_entries: Vec<_> = {
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        tokio::task::spawn_blocking(move || store.get_timeline(&sid))
            .await
            .unwrap_or_else(|_| Ok(vec![]))
            .unwrap_or_default()
    };

    // If an in-flight inference exists, always render the client-side stream target.
    // The in-memory stream buffer is the source of truth while generation is running;
    // the persisted timeline can temporarily lag or have a shape that is not simply
    // "last row is user", especially around tool calls and reconnects.
    let needs_stream_reconnect = state
        .inflight_chat
        .get(&session_id)
        .map(|inflight| !inflight.is_done())
        .unwrap_or(false);

    let total_tokens_used: u64 = timeline_entries
        .iter()
        .map(|e| match e {
            TimelineEntry::Assistant { tokens_used, .. } => tokens_used.unwrap_or(0),
            _ => 0,
        })
        .sum();
    let total_tool_calls = timeline_entries
        .iter()
        .filter(|e| matches!(e, TimelineEntry::Tool { .. }))
        .count();
    let total_cost_usd: f64 = timeline_entries
        .iter()
        .map(|e| match e {
            TimelineEntry::Assistant { cost_usd, .. } => cost_usd.unwrap_or(0.0),
            _ => 0.0,
        })
        .sum();

    let messages: Vec<_> = timeline_entries
        .into_iter()
        .map(|e| match e {
            TimelineEntry::User {
                id,
                content,
                created_at,
            } => context! {
                role => "user",
                id,
                content,
                created_at,
            },
            TimelineEntry::Assistant {
                id,
                content,
                created_at,
                tokens_used,
                cost_usd,
            } => context! {
                role => "assistant",
                id,
                content,
                created_at,
                tokens_used,
                cost_usd => cost_usd.map(|v| format!("{:.6}", v)),
            },
            TimelineEntry::Tool {
                id,
                tool_name,
                tool_intent_type,
                tool_payload_json,
                tool_result_json,
                tool_success,
                tool_duration_ms,
                created_at,
            } => context! {
                role => "tool",
                id,
                tool_name,
                tool_intent_type,
                tool_payload_json,
                tool_result_json,
                tool_success,
                tool_duration_ms,
                created_at,
            },
        })
        .collect();

    // session_id is a validated UUID (ASCII), so slicing at byte offset 8 is safe.
    let short_id = &session_id[..8];
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let agent_initial = session
        .agent_name
        .chars()
        .next()
        .unwrap_or('A')
        .to_uppercase()
        .to_string();
    let ctx = context! {
        page_title => format!("Chat — {}", short_id),
        breadcrumbs => vec![
            context! { label => "Chat", href => "/chat" },
            context! { label => short_id },
        ],
        session_id,
        agent_name => session.agent_name,
        session_title => session.title.clone(),
        agent_initial,
        messages,
        total_tokens_used,
        total_tool_calls,
        total_cost_usd => if total_cost_usd > 0.0 { format!("{:.6}", total_cost_usd) } else { String::new() },
        needs_stream_reconnect,
        csrf_token,
    };
    super::render(&state.templates, "chat_conversation.html", ctx)
}
