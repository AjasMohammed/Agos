//! Channel endpoints: list, detail, disconnect.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::ApiChannelSummary;

/// `GET /api/v1/channels` — List connected channels.
#[utoipa::path(
    get,
    path = "/api/v1/channels",
    tag = "channels",
    operation_id = "channels_list",
    responses(
        (status = 200, description = "List of channels", body = crate::response::Envelope<Vec<ApiChannelSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiChannelSummary>>>, ApiError> {
    require_permission(&key, "channels:r")?;
    Ok(Json(Envelope::new(svc.list_channels().await?)))
}

/// `GET /api/v1/channels/{id}` — Channel detail.
#[utoipa::path(
    get,
    path = "/api/v1/channels/{id}",
    tag = "channels",
    operation_id = "channels_detail",
    params(("id" = String, Path, description = "Channel instance id")),
    responses(
        (status = 200, description = "Channel detail", body = crate::response::Envelope<ApiChannelSummary>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Channel not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<ApiChannelSummary>>, ApiError> {
    require_permission(&key, "channels:r")?;
    Ok(Json(Envelope::new(svc.get_channel(&id).await?)))
}

/// `POST /api/v1/channels/{id}/disconnect` — Deregister a channel.
#[utoipa::path(
    post,
    path = "/api/v1/channels/{id}/disconnect",
    tag = "channels",
    operation_id = "channels_disconnect",
    params(("id" = String, Path, description = "Channel instance id")),
    responses(
        (status = 200, description = "Channel disconnected", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Invalid channel id", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Channel not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn disconnect(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "channels:w")?;
    svc.disconnect_channel(&id).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "disconnected": id }),
    )))
}

/// `POST /api/v1/channels` — Connect a channel (mirrors `agentos channel connect`).
#[utoipa::path(
    post,
    path = "/api/v1/channels",
    tag = "channels",
    operation_id = "channels_connect",
    security(("bearer_auth" = [])),
    request_body = crate::types::ConnectChannelRequest,
    responses(
        (status = 200, description = "Channel connected", body = crate::response::Envelope<ApiChannelSummary>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 409, description = "Adapter failed to start", body = crate::error::ApiErrorBody)
    )
)]
pub async fn connect(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<crate::types::ConnectChannelRequest>,
) -> Result<Json<Envelope<ApiChannelSummary>>, ApiError> {
    require_permission(&key, "channels:w")?;
    Ok(Json(Envelope::new(svc.connect_channel(req).await?)))
}

/// `POST /api/v1/channels/{id}/test` — Deliver a test notification.
#[utoipa::path(
    post,
    path = "/api/v1/channels/{id}/test",
    tag = "channels",
    operation_id = "channels_test",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Channel instance id")),
    responses(
        (status = 200, description = "Test message delivered", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Delivery failed", body = crate::error::ApiErrorBody)
    )
)]
pub async fn test(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "channels:w")?;
    svc.test_channel(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "delivered": id }))))
}

/// `PUT /api/v1/channels/{id}/agent` — Set/clear the default chat agent.
#[utoipa::path(
    put,
    path = "/api/v1/channels/{id}/agent",
    tag = "channels",
    operation_id = "channels_set_agent",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Channel instance id")),
    request_body = crate::types::SetChannelAgentRequest,
    responses(
        (status = 200, description = "Updated", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Channel or agent not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn set_agent(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<crate::types::SetChannelAgentRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "channels:w")?;
    svc.set_channel_agent(&id, req.agent_name.clone()).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "channel_id": id, "active_agent_name": req.agent_name }),
    )))
}

/// `GET /api/v1/channels/pairings` — DM pairing allowlist (approved + pending).
#[utoipa::path(
    get,
    path = "/api/v1/channels/pairings",
    tag = "channels",
    operation_id = "channels_pairings",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pairings", body = crate::response::Envelope<crate::types::ApiPairings>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn pairings(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<crate::types::ApiPairings>>, ApiError> {
    require_permission(&key, "channels:r")?;
    Ok(Json(Envelope::new(svc.list_pairings().await?)))
}

/// `POST /api/v1/channels/pairings/{code}/approve` — Approve a pairing code.
#[utoipa::path(
    post,
    path = "/api/v1/channels/pairings/{code}/approve",
    tag = "channels",
    operation_id = "channels_approve_pairing",
    security(("bearer_auth" = [])),
    params(("code" = String, Path, description = "6-character pairing code")),
    responses(
        (status = 200, description = "Approved", body = crate::response::Envelope<crate::types::ApiPairingEntry>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Unknown or expired code", body = crate::error::ApiErrorBody)
    )
)]
pub async fn approve_pairing(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(code): Path<String>,
) -> Result<Json<Envelope<crate::types::ApiPairingEntry>>, ApiError> {
    require_permission(&key, "channels:w")?;
    Ok(Json(Envelope::new(svc.approve_pairing(&code).await?)))
}

/// `POST /api/v1/channels/{id}/pairings/{sender_id}/approve` — Approve a
/// pending request by sender, without the code.
///
/// The code is withheld from `GET /channels/pairings` on purpose, which leaves
/// the self-pair case with no panel route (the operator is the sender, and the
/// code only reaches the kernel log). This approves the row the list already
/// returns. `channels:w` gated — it takes no secret, so it is deliberately not
/// reachable from the inbound `/pair` path.
#[utoipa::path(
    post,
    path = "/api/v1/channels/{id}/pairings/{sender_id}/approve",
    tag = "channels",
    operation_id = "channels_approve_pending_pairing",
    security(("bearer_auth" = [])),
    params(
        ("id" = String, Path, description = "Channel instance id"),
        ("sender_id" = String, Path, description = "External sender id of the pending request")
    ),
    responses(
        (status = 200, description = "Approved", body = crate::response::Envelope<crate::types::ApiPairingEntry>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "No pending request for this sender, or it expired", body = crate::error::ApiErrorBody)
    )
)]
pub async fn approve_pending_pairing(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path((id, sender_id)): Path<(String, String)>,
) -> Result<Json<Envelope<crate::types::ApiPairingEntry>>, ApiError> {
    require_permission(&key, "channels:w")?;
    Ok(Json(Envelope::new(
        svc.approve_pending_pairing(&id, &sender_id).await?,
    )))
}

/// `DELETE /api/v1/channels/{id}/pairings/{sender_id}` — Revoke an approved sender.
#[utoipa::path(
    delete,
    path = "/api/v1/channels/{id}/pairings/{sender_id}",
    tag = "channels",
    operation_id = "channels_revoke_pairing",
    security(("bearer_auth" = [])),
    params(
        ("id" = String, Path, description = "Channel instance id"),
        ("sender_id" = String, Path, description = "External sender id")
    ),
    responses(
        (status = 200, description = "Revoked", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn revoke_pairing(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path((id, sender_id)): Path<(String, String)>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "channels:w")?;
    svc.revoke_pairing(&id, &sender_id).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "channel_id": id, "sender_id": sender_id, "revoked": true }),
    )))
}

/// `PUT /api/v1/channels/{id}` — Edit a connected channel and rebuild its adapter.
#[utoipa::path(
    put,
    path = "/api/v1/channels/{id}",
    tag = "channels",
    operation_id = "channels_update",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Channel instance id")),
    request_body = crate::types::UpdateChannelRequest,
    responses(
        (status = 200, description = "Channel updated", body = crate::response::Envelope<ApiChannelSummary>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Channel not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Adapter failed to start", body = crate::error::ApiErrorBody)
    )
)]
pub async fn update(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<crate::types::UpdateChannelRequest>,
) -> Result<Json<Envelope<ApiChannelSummary>>, ApiError> {
    require_permission(&key, "channels:w")?;
    Ok(Json(Envelope::new(svc.update_channel(&id, req).await?)))
}
