//! Notification endpoints: list, get, respond, unread count.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::{NotificationFilter, NotificationResponseRequest};

/// `GET /api/v1/notifications` — List notifications with optional filtering.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(filter): Query<NotificationFilter>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "notifications:r")?;
    let notifications = svc.list_notifications(filter).await?;
    Ok(Json(serde_json::json!({ "data": notifications })))
}

/// `GET /api/v1/notifications/unread` — Get count of unread notifications.
pub async fn unread_count(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "notifications:r")?;
    let count = svc.get_unread_count().await?;
    Ok(Json(
        serde_json::json!({ "data": { "unread_count": count } }),
    ))
}

/// `GET /api/v1/notifications/{id}` — Get a single notification.
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "notifications:r")?;
    let nid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid notification ID: {id}")))?;
    let notification = svc.get_notification(nid).await?;
    Ok(Json(serde_json::json!({ "data": notification })))
}

/// `POST /api/v1/notifications/{id}/respond` — Respond to a notification.
pub async fn respond(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<NotificationResponseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "notifications:w")?;
    let nid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid notification ID: {id}")))?;
    svc.respond_to_notification(nid, req.text).await?;
    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

/// `DELETE /api/v1/notifications/read` — Clear all read notifications.
pub async fn clear_read(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "notifications:w")?;
    let deleted = svc.clear_read_notifications().await?;
    Ok(Json(serde_json::json!({ "data": { "deleted": deleted } })))
}

/// `DELETE /api/v1/notifications/{id}` — Dismiss a single notification.
pub async fn dismiss(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "notifications:w")?;
    let nid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid notification ID: {id}")))?;
    let deleted = svc.dismiss_notification(nid).await?;
    Ok(Json(serde_json::json!({ "data": { "deleted": deleted } })))
}
