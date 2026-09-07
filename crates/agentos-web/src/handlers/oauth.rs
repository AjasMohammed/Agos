//! OAuth2 connector flow — thin HTTP shell over the kernel's
//! [`agentos_kernel::oauth_flow`] (which owns PKCE, state, token exchange).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use agentos_kernel::oauth_flow::OAuthFlowError;
pub use agentos_kernel::oauth_flow::{load_provider_configs, OAuthProviderConfig};

use crate::state::AppState;

/// Query params received on the OAuth callback.
#[derive(Debug, Deserialize)]
pub struct OAuthCallbackParams {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Resolve the external base URL for constructing OAuth redirect URIs.
///
/// Checks `AGENTOS_BASE_URL` env var first, then falls back to `http://localhost:<port>`.
fn resolve_base_url() -> String {
    std::env::var("AGENTOS_BASE_URL").unwrap_or_else(|_| {
        let port = std::env::var("AGENTOS_WEB_PORT").unwrap_or_else(|_| "3000".into());
        format!("http://localhost:{port}")
    })
}

fn status_for(e: &OAuthFlowError) -> StatusCode {
    match e {
        OAuthFlowError::NoProvider(_) => StatusCode::NOT_FOUND,
        OAuthFlowError::InvalidState => StatusCode::FORBIDDEN,
        OAuthFlowError::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
        OAuthFlowError::Exchange(_) => StatusCode::BAD_GATEWAY,
        OAuthFlowError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// GET /auth/:connector_id/start — redirect the operator to the provider.
pub async fn start_oauth(
    State(state): State<AppState>,
    Path(connector_id): Path<String>,
) -> Response {
    let redirect_uri = format!("{}/auth/{connector_id}/callback", resolve_base_url());
    match state.kernel.oauth_begin(&connector_id, &redirect_uri).await {
        Ok(url) => Redirect::temporary(&url).into_response(),
        Err(e) => (status_for(&e), e.to_string()).into_response(),
    }
}

/// GET /auth/:connector_id/callback — finish the flow and go to /connectors.
pub async fn oauth_callback(
    State(state): State<AppState>,
    Path(connector_id): Path<String>,
    Query(params): Query<OAuthCallbackParams>,
) -> Response {
    if let Some(error) = &params.error {
        let desc = params
            .error_description
            .as_deref()
            .unwrap_or("no description");
        tracing::warn!(connector = %connector_id, error = %error, description = %desc, "OAuth provider returned an error");
        return (
            StatusCode::BAD_REQUEST,
            format!("OAuth error: {error} — {desc}"),
        )
            .into_response();
    }
    let (Some(code), Some(oauth_state)) = (&params.code, &params.state) else {
        return (
            StatusCode::BAD_REQUEST,
            "Missing authorization code or state",
        )
            .into_response();
    };
    match state
        .kernel
        .oauth_complete(&connector_id, code, oauth_state)
        .await
    {
        Ok(()) => Redirect::to("/connectors").into_response(),
        Err(e) => (status_for(&e), e.to_string()).into_response(),
    }
}
