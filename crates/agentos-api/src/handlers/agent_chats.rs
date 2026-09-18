//! Agent-conversation (multi-agent convo) endpoints: list, get, create+run, stop,
//! continue, and operator messages.
//!
//! Creating a conversation persists it and spawns a background turn-by-turn
//! orchestration loop (round-robin participants); clients poll `GET {id}` for
//! progress. Token-by-token streaming of each turn is a future enhancement.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::{Envelope, ListEnvelope};
use crate::service::KernelService;
use crate::types::{
    ApiConvoDetail, ApiConvoSummary, ContinueConvoRequest, CreateConvoRequest,
    PostConvoMessageRequest,
};

/// `GET /api/v1/agent-chats` — List multi-agent conversations (most-recent first).
#[utoipa::path(
    get,
    path = "/api/v1/agent-chats",
    tag = "agent-chats",
    operation_id = "agent_chats_list",
    responses(
        (status = 200, description = "List of conversations", body = crate::response::ListEnvelope<crate::types::ApiConvoSummary>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<ListEnvelope<ApiConvoSummary>>, ApiError> {
    require_permission(&key, "chat:r")?;
    let convos = svc.list_convos().await?;
    let total = convos.len() as u64;
    Ok(Json(ListEnvelope::new(convos, total)))
}

/// `GET /api/v1/agent-chats/{id}` — Get a conversation with its turn timeline.
#[utoipa::path(
    get,
    path = "/api/v1/agent-chats/{id}",
    tag = "agent-chats",
    operation_id = "agent_chats_get",
    params(("id" = String, Path, description = "Conversation id (UUID)")),
    responses(
        (status = 200, description = "Conversation detail", body = crate::response::Envelope<crate::types::ApiConvoDetail>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Conversation not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<ApiConvoDetail>>, ApiError> {
    require_permission(&key, "chat:r")?;
    let detail = svc.get_convo(&id).await?;
    Ok(Json(Envelope::new(detail)))
}

/// `POST /api/v1/agent-chats` — Create a conversation and start its orchestration
/// loop in the background. Returns the created conversation; poll `GET {id}` for
/// turns + status (`active` → `complete`/`stopped`/`error`).
#[utoipa::path(
    post, path = "/api/v1/agent-chats", tag = "agent-chats", operation_id = "agent_chats_create",
    request_body = CreateConvoRequest,
    responses(
        (status = 200, description = "Conversation created (running)", body = crate::response::Envelope<crate::types::ApiConvoSummary>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<CreateConvoRequest>,
) -> Result<Json<Envelope<ApiConvoSummary>>, ApiError> {
    require_permission(&key, "chat:w")?;
    let max_turns = req.max_turns.unwrap_or(8).clamp(2, 50);
    let summary = svc
        .create_agent_chat(req.topic.clone(), req.participants.clone(), max_turns)
        .await?;

    // The service trims the topic; `spawn_run` runs with the text it stored.
    spawn_run(&svc, &summary, max_turns);

    Ok(Json(Envelope::new(summary)))
}

/// `POST /api/v1/agent-chats/{id}/stop` — Stop a running conversation after its
/// current turn.
#[utoipa::path(
    post, path = "/api/v1/agent-chats/{id}/stop", tag = "agent-chats",
    operation_id = "agent_chats_stop",
    params(("id" = String, Path, description = "Conversation id (UUID)")),
    responses(
        (status = 200, description = "Stop requested", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn stop(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "chat:w")?;
    svc.stop_agent_chat(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "stopped": id }))))
}

/// Run a reopen `op` detached from the request, then start the loop if it
/// reopened the conversation. Awaited inline, a client disconnecting mid-claim
/// would drop the future after `running` was committed but before the loop
/// started — a conversation marked live that nothing runs.
async fn detached<F, Fut>(svc: &Arc<dyn KernelService>, op: F) -> Result<ApiConvoSummary, ApiError>
where
    F: FnOnce(Arc<dyn KernelService>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(ApiConvoSummary, Option<u32>), ApiError>>
        + Send
        + 'static,
{
    let svc = svc.clone();
    tokio::spawn(async move {
        let (summary, resume) = op(svc.clone()).await?;
        if let Some(ceiling) = resume {
            spawn_run(&svc, &summary, ceiling);
        }
        Ok(summary)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
}

/// Start the orchestration loop in the background; the client polls `GET {id}`.
fn spawn_run(svc: &Arc<dyn KernelService>, summary: &ApiConvoSummary, max_turns: u32) {
    let svc = svc.clone();
    let id = summary.id.clone();
    let topic = summary.topic.clone();
    let participants = summary.participants.clone();
    tokio::spawn(async move {
        svc.run_agent_chat(&id, topic, participants, max_turns)
            .await;
    });
}

/// `POST /api/v1/agent-chats/{id}/continue` — Resume a finished conversation in
/// place for more turns, picking up after its last speaker.
#[utoipa::path(
    post, path = "/api/v1/agent-chats/{id}/continue", tag = "agent-chats",
    operation_id = "agent_chats_continue",
    params(("id" = String, Path, description = "Conversation id (UUID)")),
    request_body = ContinueConvoRequest,
    responses(
        (status = 200, description = "Conversation resumed (running)", body = crate::response::Envelope<crate::types::ApiConvoSummary>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Conversation not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Conversation is still running", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn continue_chat(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<ContinueConvoRequest>,
) -> Result<Json<Envelope<ApiConvoSummary>>, ApiError> {
    require_permission(&key, "chat:w")?;
    let turns = req.turns.unwrap_or(8).clamp(1, 50);
    let summary = detached(&svc, move |svc| async move {
        let (summary, ceiling) = svc.continue_agent_chat(&id, turns).await?;
        Ok((summary, Some(ceiling)))
    })
    .await?;
    Ok(Json(Envelope::new(summary)))
}

/// `POST /api/v1/agent-chats/{id}/messages` — Post an operator message. A running
/// conversation answers it on its next turn; a finished one resumes for one round.
#[utoipa::path(
    post, path = "/api/v1/agent-chats/{id}/messages", tag = "agent-chats",
    operation_id = "agent_chats_post_message",
    params(("id" = String, Path, description = "Conversation id (UUID)")),
    request_body = PostConvoMessageRequest,
    responses(
        (status = 200, description = "Message stored", body = crate::response::Envelope<crate::types::ApiConvoSummary>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Conversation not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn post_message(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<PostConvoMessageRequest>,
) -> Result<Json<Envelope<ApiConvoSummary>>, ApiError> {
    require_permission(&key, "chat:w")?;
    let summary = detached(&svc, move |svc| async move {
        svc.post_agent_chat_message(&id, req.content).await
    })
    .await?;
    Ok(Json(Envelope::new(summary)))
}
