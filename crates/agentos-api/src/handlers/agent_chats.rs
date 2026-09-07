//! Agent-conversation (multi-agent convo) endpoints: list, get, create+run, stop.
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
use crate::types::{ApiConvoDetail, ApiConvoSummary, CreateConvoRequest};

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

    // Run the orchestration loop in the background; the client polls GET {id}.
    let svc2 = svc.clone();
    let id = summary.id.clone();
    let topic = req.topic;
    let participants = req.participants;
    tokio::spawn(async move {
        svc2.run_agent_chat(&id, topic, participants, max_turns)
            .await;
    });

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
