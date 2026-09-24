//! Connector (OAuth) endpoints: list, detail, disconnect, OAuth start/callback.

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

/// Where the OAuth flow's URLs point. Built once in `build_router`.
#[derive(Debug, Clone)]
pub struct OAuthRedirects {
    /// Public base of this API (`AGENTOS_BASE_URL` or `http://localhost:<port>`).
    pub api_base: String,
    /// First `[api] cors_allowed_origins` entry — where the callback sends the browser.
    pub panel_origin: Option<String>,
}

/// `POST /api/v1/connectors` — Register a connector from a manifest TOML.
#[utoipa::path(
    post,
    path = "/api/v1/connectors",
    tag = "connectors",
    operation_id = "connectors_add",
    security(("bearer_auth" = [])),
    request_body = crate::types::AddConnectorRequest,
    responses(
        (status = 200, description = "Registered", body = crate::response::Envelope<ApiConnectorDetail>),
        (status = 400, description = "Invalid manifest", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 409, description = "Already registered", body = crate::error::ApiErrorBody)
    )
)]
pub async fn add(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<crate::types::AddConnectorRequest>,
) -> Result<Json<Envelope<ApiConnectorDetail>>, ApiError> {
    require_permission(&key, "connectors:w")?;
    Ok(Json(Envelope::new(
        svc.add_connector(&req.manifest_toml).await?,
    )))
}

/// `DELETE /api/v1/connectors/{id}` — Remove manifest + credential.
#[utoipa::path(
    delete,
    path = "/api/v1/connectors/{id}",
    tag = "connectors",
    operation_id = "connectors_remove",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Connector id")),
    responses(
        (status = 200, description = "Removed", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn remove(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "connectors:w")?;
    svc.remove_connector(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "removed": id }))))
}

/// `POST /api/v1/connectors/{id}/oauth/start` — Begin OAuth; returns the URL to open.
#[utoipa::path(
    post,
    path = "/api/v1/connectors/{id}/oauth/start",
    tag = "connectors",
    operation_id = "connectors_oauth_start",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Connector / provider id")),
    responses(
        (status = 200, description = "Authorize URL", body = crate::response::Envelope<crate::types::OAuthStartResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "No provider configured", body = crate::error::ApiErrorBody),
        (status = 409, description = "Provider misconfigured (client id env unset)", body = crate::error::ApiErrorBody)
    )
)]
pub async fn oauth_start(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Extension(redirects): Extension<Arc<OAuthRedirects>>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<crate::types::OAuthStartResponse>>, ApiError> {
    require_permission(&key, "connectors:w")?;
    let redirect_uri = format!(
        "{}/api/v1/connectors/{id}/oauth/callback",
        redirects.api_base
    );
    let authorize_url = svc.start_connector_oauth(&id, &redirect_uri).await?;
    Ok(Json(Envelope::new(crate::types::OAuthStartResponse {
        authorize_url,
    })))
}

/// `GET /api/v1/connectors/{id}/oauth/callback` — Provider redirect target (public).
///
/// No bearer: the browser arrives here from the provider. Safety is the
/// vault-validated `state`. On completion the browser is sent to the panel
/// origin from config — never to a caller-supplied URL.
#[utoipa::path(
    get,
    path = "/api/v1/connectors/{id}/oauth/callback",
    tag = "connectors",
    operation_id = "connectors_oauth_callback",
    params(("id" = String, Path, description = "Connector / provider id"), crate::types::OAuthCallbackQuery),
    responses(
        (status = 302, description = "Redirect to the panel with `?oauth=ok|error`"),
        (status = 200, description = "Plain-text result when no panel origin is configured")
    )
)]
pub async fn oauth_callback(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(redirects): Extension<Arc<OAuthRedirects>>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<crate::types::OAuthCallbackQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let outcome: Result<(), String> = if let Some(err) = q.error.as_deref() {
        Err(format!(
            "{err}: {}",
            q.error_description.as_deref().unwrap_or("no description")
        ))
    } else {
        match (q.code.as_deref(), q.state.as_deref()) {
            (Some(code), Some(state)) => svc
                .complete_connector_oauth(&id, code, state)
                .await
                .map_err(|e| e.to_string()),
            _ => Err("Missing authorization code or state".to_string()),
        }
    };
    let Some(origin) = redirects.panel_origin.as_deref() else {
        return match outcome {
            Ok(()) => (
                axum::http::StatusCode::OK,
                format!("Connector '{id}' connected."),
            )
                .into_response(),
            Err(m) => (
                axum::http::StatusCode::BAD_REQUEST,
                format!("OAuth failed: {m}"),
            )
                .into_response(),
        };
    };
    let mut url = format!("{origin}/connectors?id={}", urlencode(&id));
    match outcome {
        Ok(()) => url.push_str("&oauth=ok"),
        Err(m) => {
            url.push_str("&oauth=error&message=");
            url.push_str(&urlencode(&m));
        }
    }
    axum::response::Redirect::to(&url).into_response()
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// `POST /api/v1/connectors/{id}/credential` — Store an OAuth token by hand.
#[utoipa::path(
    post,
    path = "/api/v1/connectors/{id}/credential",
    tag = "connectors",
    operation_id = "connectors_store_credential",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Connector id")),
    request_body = crate::types::StoreCredentialRequest,
    responses(
        (status = 200, description = "Stored", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn store_credential(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<crate::types::StoreCredentialRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "connectors:w")?;
    svc.store_connector_credential(&id, req).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "stored": id }))))
}

/// `PUT /api/v1/connectors/{id}` — Replace a connector manifest in place.
#[utoipa::path(
    put,
    path = "/api/v1/connectors/{id}",
    tag = "connectors",
    operation_id = "connectors_update",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Connector id")),
    request_body = crate::types::AddConnectorRequest,
    responses(
        (status = 200, description = "Updated", body = crate::response::Envelope<ApiConnectorDetail>),
        (status = 400, description = "Invalid manifest or id mismatch", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn update(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<crate::types::AddConnectorRequest>,
) -> Result<Json<Envelope<ApiConnectorDetail>>, ApiError> {
    require_permission(&key, "connectors:w")?;
    Ok(Json(Envelope::new(
        svc.update_connector(&id, &req.manifest_toml).await?,
    )))
}
