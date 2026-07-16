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
