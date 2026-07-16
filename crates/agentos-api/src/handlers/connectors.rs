//! Connector (OAuth) endpoints: list, detail, disconnect.
//!
//! Note: OAuth `start`/`callback` flows are intentionally NOT exposed here — they
//! are cookie/redirect-shaped and live in `agentos-web`. This surface manages
//! existing connectors only.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiConnectorDetail, ApiConnectorSummary};

/// `GET /api/v1/connectors` — List registered connectors and connection status.
#[utoipa::path(
    get,
    path = "/api/v1/connectors",
    tag = "connectors",
    operation_id = "connectors_list",
    responses(
        (status = 200, description = "List of connectors", body = crate::response::Envelope<Vec<ApiConnectorSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiConnectorSummary>>>, ApiError> {
    require_permission(&key, "connectors:r")?;
    Ok(Json(Envelope::new(svc.list_connectors().await?)))
}

/// `GET /api/v1/connectors/{id}` — Connector detail.
#[utoipa::path(
    get,
    path = "/api/v1/connectors/{id}",
    tag = "connectors",
    operation_id = "connectors_detail",
    params(("id" = String, Path, description = "Connector id")),
    responses(
        (status = 200, description = "Connector detail", body = crate::response::Envelope<ApiConnectorDetail>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Connector not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<ApiConnectorDetail>>, ApiError> {
    require_permission(&key, "connectors:r")?;
    Ok(Json(Envelope::new(svc.get_connector(&id).await?)))
}

/// `POST /api/v1/connectors/{id}/disconnect` — Revoke OAuth credential + deregister.
#[utoipa::path(
    post,
    path = "/api/v1/connectors/{id}/disconnect",
    tag = "connectors",
    operation_id = "connectors_disconnect",
    params(("id" = String, Path, description = "Connector id")),
    responses(
        (status = 200, description = "Connector disconnected", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn disconnect(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "connectors:w")?;
    svc.disconnect_connector(&id).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "disconnected": id }),
    )))
}
