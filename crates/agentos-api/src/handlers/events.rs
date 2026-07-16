//! Event endpoints: list subscriptions, subscribe, unsubscribe, emit.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiEventSubscription, CreateSubscriptionRequest, EmitEventRequest};

/// `GET /api/v1/events/subscriptions` — List all event subscriptions.
#[utoipa::path(
    get,
    path = "/api/v1/events/subscriptions",
    tag = "events",
    operation_id = "events_list_subscriptions",
    responses(
        (status = 200, description = "List of subscriptions", body = crate::response::Envelope<Vec<ApiEventSubscription>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_subscriptions(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiEventSubscription>>>, ApiError> {
    require_permission(&key, "events:r")?;
    Ok(Json(Envelope::new(svc.list_event_subscriptions().await?)))
}

/// `POST /api/v1/events/subscriptions` — Create an event subscription.
#[utoipa::path(
    post,
    path = "/api/v1/events/subscriptions",
    tag = "events",
    operation_id = "events_create_subscription",
    request_body = CreateSubscriptionRequest,
    responses(
        (status = 200, description = "Subscription created", body = crate::response::Envelope<ApiEventSubscription>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_subscription(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<CreateSubscriptionRequest>,
) -> Result<Json<Envelope<ApiEventSubscription>>, ApiError> {
    require_permission(&key, "events:w")?;
    Ok(Json(Envelope::new(
        svc.create_event_subscription(req).await?,
    )))
}

/// `DELETE /api/v1/events/subscriptions/{id}` — Remove a subscription.
#[utoipa::path(
    delete,
    path = "/api/v1/events/subscriptions/{id}",
    tag = "events",
    operation_id = "events_delete_subscription",
    params(("id" = String, Path, description = "Subscription id")),
    responses(
        (status = 200, description = "Subscription removed", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Subscription not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_subscription(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "events:w")?;
    svc.delete_event_subscription(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "removed": id }))))
}

/// `POST /api/v1/events/subscriptions/{id}/enable` — Activate a subscription.
#[utoipa::path(
    post,
    path = "/api/v1/events/subscriptions/{id}/enable",
    tag = "events",
    operation_id = "events_enable_subscription",
    params(("id" = String, Path, description = "Subscription id")),
    responses(
        (status = 200, description = "Subscription enabled", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Subscription not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn enable_subscription(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "events:w")?;
    svc.enable_event_subscription(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "enabled": id }))))
}

/// `POST /api/v1/events/subscriptions/{id}/disable` — Pause a subscription.
#[utoipa::path(
    post,
    path = "/api/v1/events/subscriptions/{id}/disable",
    tag = "events",
    operation_id = "events_disable_subscription",
    params(("id" = String, Path, description = "Subscription id")),
    responses(
        (status = 200, description = "Subscription disabled", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Subscription not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn disable_subscription(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "events:w")?;
    svc.disable_event_subscription(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "disabled": id }))))
}

/// `POST /api/v1/events/emit` — Emit an event into the kernel event bus.
#[utoipa::path(
    post,
    path = "/api/v1/events/emit",
    tag = "events",
    operation_id = "events_emit",
    request_body = EmitEventRequest,
    responses(
        (status = 200, description = "Event emitted", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn emit(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<EmitEventRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "events:w")?;
    svc.emit_event(req).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "emitted": true }))))
}
