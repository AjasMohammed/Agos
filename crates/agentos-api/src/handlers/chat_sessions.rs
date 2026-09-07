//! Chat-session management endpoints (Phase 02 Conversational).
//!
//! CRUD + fork + export + messages for persisted chat sessions. Message *send*
//! and streaming are intentionally NOT here — this surface is read/manage only.
//! Sending lives behind the OpenAI-compatible `POST /api/v1/chat/completions`
//! and the (deferred) session-scoped send/SSE endpoints.

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::{Envelope, ListEnvelope};
use crate::service::KernelService;
use crate::types::{
    ApiChatMessage, ApiChatSessionDetail, ApiChatSessionSummary, CreateChatSessionRequest,
    ExportQuery, ForkChatSessionRequest, ForkChatSessionResponse, RenameChatSessionRequest,
};

/// `GET /api/v1/chat/sessions` — List chat sessions (most-recent first).
#[utoipa::path(
    get,
    path = "/api/v1/chat/sessions",
    tag = "chat-sessions",
    operation_id = "chat_sessions_list",
    responses(
        (status = 200, description = "List of chat sessions", body = crate::response::ListEnvelope<crate::types::ApiChatSessionSummary>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<ListEnvelope<ApiChatSessionSummary>>, ApiError> {
    require_permission(&key, "chat:r")?;
    let sessions = svc.list_chat_sessions().await?;
    let total = sessions.len() as u64;
    Ok(Json(ListEnvelope::new(sessions, total)))
}

/// `POST /api/v1/chat/sessions` — Create a new chat session.
#[utoipa::path(
    post,
    path = "/api/v1/chat/sessions",
    tag = "chat-sessions",
    operation_id = "chat_sessions_create",
    request_body = CreateChatSessionRequest,
    responses(
        (status = 200, description = "Created session", body = crate::response::Envelope<crate::types::ApiChatSessionDetail>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<CreateChatSessionRequest>,
) -> Result<Json<Envelope<ApiChatSessionDetail>>, ApiError> {
    require_permission(&key, "chat:w")?;
    let detail = svc.create_chat_session(req).await?;
    Ok(Json(Envelope::new(detail)))
}

/// `GET /api/v1/chat/sessions/{id}` — Get a session with its message timeline.
#[utoipa::path(
    get,
    path = "/api/v1/chat/sessions/{id}",
    tag = "chat-sessions",
    operation_id = "chat_sessions_get",
    params(("id" = String, Path, description = "Chat session id (UUID)")),
    responses(
        (status = 200, description = "Session detail", body = crate::response::Envelope<crate::types::ApiChatSessionDetail>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Session not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<ApiChatSessionDetail>>, ApiError> {
    require_permission(&key, "chat:r")?;
    let detail = svc.get_chat_session(&id).await?;
    Ok(Json(Envelope::new(detail)))
}

/// `PATCH /api/v1/chat/sessions/{id}` — Rename a session (or clear the title).
#[utoipa::path(
    patch,
    path = "/api/v1/chat/sessions/{id}",
    tag = "chat-sessions",
    operation_id = "chat_sessions_rename",
    params(("id" = String, Path, description = "Chat session id (UUID)")),
    request_body = RenameChatSessionRequest,
    responses(
        (status = 200, description = "Session renamed", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Session not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn rename(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<RenameChatSessionRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "chat:w")?;
    svc.rename_chat_session(&id, req.title).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "ok": true }))))
}

/// `DELETE /api/v1/chat/sessions/{id}` — Delete a session and its messages.
#[utoipa::path(
    delete,
    path = "/api/v1/chat/sessions/{id}",
    tag = "chat-sessions",
    operation_id = "chat_sessions_delete",
    params(("id" = String, Path, description = "Chat session id (UUID)")),
    responses(
        (status = 200, description = "Session deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Session not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "chat:w")?;
    svc.delete_chat_session(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": id }))))
}

/// `POST /api/v1/chat/sessions/{id}/fork` — Fork a session into a new copy.
#[utoipa::path(
    post,
    path = "/api/v1/chat/sessions/{id}/fork",
    tag = "chat-sessions",
    operation_id = "chat_sessions_fork",
    params(("id" = String, Path, description = "Source chat session id (UUID)")),
    request_body = ForkChatSessionRequest,
    responses(
        (status = 200, description = "Forked session id", body = crate::response::Envelope<crate::types::ForkChatSessionResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Session not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn fork(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<ForkChatSessionRequest>,
) -> Result<Json<Envelope<ForkChatSessionResponse>>, ApiError> {
    require_permission(&key, "chat:w")?;
    let new_id = svc.fork_chat_session(&id, req.title).await?;
    Ok(Json(Envelope::new(ForkChatSessionResponse { id: new_id })))
}

/// `GET /api/v1/chat/sessions/{id}/messages` — List a session's messages.
#[utoipa::path(
    get,
    path = "/api/v1/chat/sessions/{id}/messages",
    tag = "chat-sessions",
    operation_id = "chat_sessions_messages",
    params(("id" = String, Path, description = "Chat session id (UUID)")),
    responses(
        (status = 200, description = "Session messages (oldest-first)", body = crate::response::ListEnvelope<crate::types::ApiChatMessage>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Session not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn messages(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<ListEnvelope<ApiChatMessage>>, ApiError> {
    require_permission(&key, "chat:r")?;
    let msgs = svc.get_chat_messages(&id).await?;
    let total = msgs.len() as u64;
    Ok(Json(ListEnvelope::new(msgs, total)))
}

/// `GET /api/v1/chat/sessions/{id}/export` — Export a session as JSON or markdown.
///
/// Returns raw bytes (NOT the `{ data }` envelope) with a download-safe
/// `Content-Type` and `Content-Disposition`. `?format=json` (default) or
/// `?format=markdown`.
#[utoipa::path(
    get,
    path = "/api/v1/chat/sessions/{id}/export",
    tag = "chat-sessions",
    operation_id = "chat_sessions_export",
    params(
        ("id" = String, Path, description = "Chat session id (UUID)"),
        ExportQuery
    ),
    responses(
        (status = 200, description = "Exported session bytes", content_type = "application/json"),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Session not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn export(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    require_permission(&key, "chat:r")?;
    let (bytes, content_type, filename) = svc
        .export_chat_session(&id, q.format.as_deref().unwrap_or("json"))
        .await?;
    // Strip quotes/CRLF from filename to prevent header injection.
    let safe_filename = filename.replace(['"', '\r', '\n'], "");
    let disposition = format!("attachment; filename=\"{safe_filename}\"");
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response())
}

/// `POST /api/v1/chat/sessions/{id}/messages` — send a user message and get the
/// assistant reply (non-streaming). Both turns are persisted to the session.
#[utoipa::path(
    post, path = "/api/v1/chat/sessions/{id}/messages", tag = "chat-sessions",
    operation_id = "chat_sessions_send",
    params(("id" = String, Path, description = "Chat session ID")),
    request_body = crate::types::SendChatMessageRequest,
    responses(
        (status = 200, description = "Assistant reply", body = crate::response::Envelope<ApiChatMessage>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Session not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn send(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<crate::types::SendChatMessageRequest>,
) -> Result<Json<Envelope<ApiChatMessage>>, ApiError> {
    require_permission(&key, "chat:w")?;
    let reply = svc.send_chat_message(&id, req.text).await?;
    Ok(Json(Envelope::new(reply)))
}

/// `POST /api/v1/chat/sessions/{id}/messages/stream` — send a user message and
/// stream the assistant reply as Server-Sent Events (`thinking`/`chunk`/
/// `tool_start`/`tool_result`/`done`/`error`). Both turns are persisted.
#[utoipa::path(
    post, path = "/api/v1/chat/sessions/{id}/messages/stream", tag = "chat-sessions",
    operation_id = "chat_sessions_send_stream",
    params(("id" = String, Path, description = "Chat session ID")),
    request_body = crate::types::SendChatMessageRequest,
    responses(
        (status = 200, description = "SSE stream of chat events (text/event-stream)"),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Session not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn send_stream(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<crate::types::SendChatMessageRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    require_permission(&key, "chat:w")?;

    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<agentos_kernel::ChatStreamEvent>(64);
    let svc2 = svc.clone();
    let id2 = id.clone();
    tokio::spawn(async move {
        if let Err(e) = svc2
            .stream_chat_message(&id2, req.text, out_tx.clone())
            .await
        {
            let _ = out_tx
                .send(agentos_kernel::ChatStreamEvent::Error {
                    message: e.to_string(),
                })
                .await;
        }
    });

    let stream = ReceiverStream::new(out_rx).map(|ev| {
        let name = match &ev {
            agentos_kernel::ChatStreamEvent::Thinking { .. } => "thinking",
            agentos_kernel::ChatStreamEvent::TextChunk { .. } => "chunk",
            agentos_kernel::ChatStreamEvent::ToolStart { .. } => "tool_start",
            agentos_kernel::ChatStreamEvent::ToolResult { .. } => "tool_result",
            agentos_kernel::ChatStreamEvent::Done { .. } => "done",
            agentos_kernel::ChatStreamEvent::Error { .. } => "error",
        };
        Ok(Event::default()
            .event(name)
            .data(serde_json::to_string(&ev).unwrap_or_default()))
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
