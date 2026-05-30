//! Notification endpoints: list, get, respond, unread count.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{NotificationFilter, NotificationResponseRequest};

/// `GET /api/v1/notifications` — List notifications with optional filtering.
#[utoipa::path(
    get,
    path = "/api/v1/notifications",
    tag = "notifications",
    operation_id = "notifications_list",
    security(("bearer_auth" = [])),
    params(crate::types::NotificationFilter),
    responses(
        (status = 200, description = "List of notifications", body = crate::response::Envelope<Vec<crate::types::NotificationSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(filter): Query<NotificationFilter>,
) -> Result<Json<Envelope<Vec<crate::types::NotificationSummary>>>, ApiError> {
    require_permission(&key, "notifications:r")?;
    let notifications = svc.list_notifications(filter).await?;
    Ok(Json(Envelope::new(notifications)))
}

/// `GET /api/v1/notifications/unread` — Get count of unread notifications.
#[utoipa::path(
    get,
    path = "/api/v1/notifications/unread",
    tag = "notifications",
    operation_id = "notifications_unread_count",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Unread notification count", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn unread_count(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "notifications:r")?;
    let count = svc.get_unread_count().await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "unread_count": count }),
    )))
}

/// `GET /api/v1/notifications/{id}` — Get a single notification.
#[utoipa::path(
    get,
    path = "/api/v1/notifications/{id}",
    tag = "notifications",
    operation_id = "notifications_get",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Notification ID")),
    responses(
        (status = 200, description = "Notification", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Notification not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "notifications:r")?;
    let nid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid notification ID: {id}")))?;
    let notification = svc.get_notification(nid).await?;
    Ok(Json(Envelope::new(serde_json::json!(notification))))
}

/// `POST /api/v1/notifications/{id}/respond` — Respond to a notification.
#[utoipa::path(
    post,
    path = "/api/v1/notifications/{id}/respond",
    tag = "notifications",
    operation_id = "notifications_respond",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Notification ID")),
    request_body = NotificationResponseRequest,
    responses(
        (status = 200, description = "Response recorded", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Notification not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn respond(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<NotificationResponseRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "notifications:w")?;
    let nid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid notification ID: {id}")))?;
    svc.respond_to_notification(nid, req.text).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "ok": true }))))
}

/// `DELETE /api/v1/notifications/read` — Clear all read notifications.
#[utoipa::path(
    delete,
    path = "/api/v1/notifications/read",
    tag = "notifications",
    operation_id = "notifications_clear_read",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Read notifications cleared", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn clear_read(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "notifications:w")?;
    let deleted = svc.clear_read_notifications().await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "deleted": deleted }),
    )))
}

/// `DELETE /api/v1/notifications/{id}` — Dismiss a single notification.
#[utoipa::path(
    delete,
    path = "/api/v1/notifications/{id}",
    tag = "notifications",
    operation_id = "notifications_dismiss",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Notification ID")),
    responses(
        (status = 200, description = "Notification dismissed", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Notification not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn dismiss(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "notifications:w")?;
    let nid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid notification ID: {id}")))?;
    let deleted = svc.dismiss_notification(nid).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "deleted": deleted }),
    )))
}
