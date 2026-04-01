//! Notification endpoints: list, get, respond, unread count.

use axum::extract::{Path, Query, State};
use axum::Json;
use std::sync::Arc;

use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::{NotificationFilter, NotificationResponseRequest};

/// `GET /v1/notifications` — List notifications with optional filtering.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Query(filter): Query<NotificationFilter>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let notifications = svc.list_notifications(filter).await?;
    Ok(Json(serde_json::json!({ "notifications": notifications })))
}

/// `GET /v1/notifications/unread` — Get count of unread notifications.
pub async fn unread_count(
    State(svc): State<Arc<dyn KernelService>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let count = svc.get_unread_count().await?;
    Ok(Json(serde_json::json!({ "unread_count": count })))
}

/// `GET /v1/notifications/{id}` — Get a single notification.
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let nid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid notification ID: {id}")))?;
    let notification = svc.get_notification(nid).await?;
    Ok(Json(serde_json::json!(notification)))
}

/// `POST /v1/notifications/{id}/respond` — Respond to a notification.
pub async fn respond(
    State(svc): State<Arc<dyn KernelService>>,
    Path(id): Path<String>,
    Json(req): Json<NotificationResponseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let nid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid notification ID: {id}")))?;
    svc.respond_to_notification(nid, req.text).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
