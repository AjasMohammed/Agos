use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use agentos_vault::{OAuthCredential, OAuthPendingFlow};

use crate::state::AppState;

/// OAuth provider configuration (loaded from config/oauth_providers.toml).
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthProviderConfig {
    pub authorize_url: String,
    pub token_url: String,
    /// Environment variable name for the client ID (read at boot).
    pub client_id_env: String,
    /// Vault secret key for the client_secret.
    pub client_secret_vault_key: String,
    #[serde(default)]
    pub default_scopes: Vec<String>,
}

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

/// GET /auth/:connector_id/start
///
/// Initiates the OAuth2 Authorization Code flow with PKCE:
/// 1. Look up provider config for the connector
/// 2. Generate cryptographic `state` and PKCE `code_verifier` / `code_challenge`
/// 3. Store pending flow in vault
/// 4. Redirect the operator's browser to the provider's authorization URL
pub async fn start_oauth(
    State(state): State<AppState>,
    Path(connector_id): Path<String>,
) -> Response {
    let providers = load_provider_configs();

    let provider = match providers.get(&connector_id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                format!("No OAuth provider configured for: {connector_id}"),
            )
                .into_response();
        }
    };

    // Read client_id from environment
    let client_id = match std::env::var(&provider.client_id_env) {
        Ok(id) if !id.is_empty() => id,
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Environment variable {} not set for {connector_id}",
                    provider.client_id_env
                ),
            )
                .into_response();
        }
    };

    // Generate PKCE code_verifier (43-128 chars, URL-safe per RFC 7636)
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);

    // Generate CSRF state (32 random bytes, hex-encoded)
    let oauth_state = generate_state();

    // Build absolute redirect URI (OAuth providers require a fully-qualified URL)
    let base_url = resolve_base_url();
    let redirect_uri = format!("{base_url}/auth/{connector_id}/callback");

    // Store pending flow in vault (encrypted, 10-minute TTL)
    let now = chrono::Utc::now();
    let flow = OAuthPendingFlow {
        connector_id: connector_id.clone(),
        state: oauth_state.clone(),
        code_verifier: Some(code_verifier),
        redirect_uri: redirect_uri.clone(),
        created_at: now,
        expires_at: now + chrono::Duration::minutes(10),
    };

    let oauth_store = state.kernel.vault.oauth_store();
    if let Err(e) = oauth_store.store_pending_flow(&flow).await {
        tracing::error!(error = %e, "Failed to store pending OAuth flow");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
    }

    // Build the authorization URL
    let scopes = provider.default_scopes.join(" ");
    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        provider.authorize_url,
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&scopes),
        urlencoding::encode(&oauth_state),
        urlencoding::encode(&code_challenge),
    );

    Redirect::temporary(&auth_url).into_response()
}

/// GET /auth/:connector_id/callback
///
/// Handles the OAuth2 provider callback after user authorization:
/// 1. Validate state parameter against pending flows
/// 2. Exchange authorization code for tokens
/// 3. Store credential in vault
/// 4. Register connector if not already registered
/// 5. Redirect to /connectors
pub async fn oauth_callback(
    State(state): State<AppState>,
    Path(connector_id): Path<String>,
    Query(params): Query<OAuthCallbackParams>,
) -> Response {
    // Check for provider-side errors
    if let Some(error) = &params.error {
        let desc = params
            .error_description
            .as_deref()
            .unwrap_or("no description");
        tracing::warn!(
            connector = %connector_id,
            error = %error,
            description = %desc,
            "OAuth provider returned an error"
        );
        return (
            StatusCode::BAD_REQUEST,
            format!("OAuth error: {error} — {desc}"),
        )
            .into_response();
    }

    let code = match &params.code {
        Some(c) => c.clone(),
        None => {
            return (StatusCode::BAD_REQUEST, "Missing authorization code").into_response();
        }
    };

    let oauth_state = match &params.state {
        Some(s) => s.clone(),
        None => {
            return (StatusCode::BAD_REQUEST, "Missing state parameter").into_response();
        }
    };

    // Complete the pending flow (validates state, retrieves code_verifier)
    let oauth_store = state.kernel.vault.oauth_store();
    let flow = match oauth_store.complete_pending_flow(&oauth_state).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "Invalid or expired OAuth state");
            return (StatusCode::FORBIDDEN, "Invalid or expired OAuth state").into_response();
        }
    };

    if flow.connector_id != connector_id {
        return (
            StatusCode::FORBIDDEN,
            "State parameter does not match connector",
        )
            .into_response();
    }

    // Load provider config for token exchange
    let providers = load_provider_configs();

    let provider = match providers.get(&connector_id) {
        Some(p) => p,
        None => {
            return (StatusCode::NOT_FOUND, "Provider config not found").into_response();
        }
    };

    let client_id = match std::env::var(&provider.client_id_env) {
        Ok(id) => id,
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Client ID not configured",
            )
                .into_response();
        }
    };

    // Get client_secret from vault — distinguish "not found" from vault errors
    let client_secret = match state
        .kernel
        .vault
        .get(&provider.client_secret_vault_key)
        .await
    {
        Ok(s) => Some(s.as_str().to_string()),
        Err(agentos_types::AgentOSError::SecretNotFound(_)) => None,
        Err(e) => {
            tracing::warn!(
                connector = %connector_id,
                error = %e,
                "Vault error reading client secret (proceeding without it)"
            );
            None
        }
    };

    // Exchange authorization code for tokens
    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code),
        ("redirect_uri".to_string(), flow.redirect_uri),
        ("client_id".to_string(), client_id.clone()),
    ];

    if let Some(ref verifier) = flow.code_verifier {
        form.push(("code_verifier".to_string(), verifier.clone()));
    }

    if let Some(ref secret) = client_secret {
        form.push(("client_secret".to_string(), secret.clone()));
    }

    let http_client = reqwest::Client::new();
    let token_resp = match http_client
        .post(&provider.token_url)
        .header("Accept", "application/json")
        .form(&form)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "Token exchange HTTP request failed");
            return (StatusCode::BAD_GATEWAY, "Token exchange failed").into_response();
        }
    };

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        // Truncate error body to avoid logging sensitive data
        let safe_body = if body.len() > 500 {
            &body[..500]
        } else {
            &body
        };
        tracing::error!(
            status = %status,
            body = %safe_body,
            "Token exchange returned error"
        );
        return (
            StatusCode::BAD_GATEWAY,
            format!("Token exchange failed: HTTP {status}"),
        )
            .into_response();
    }

    let token_data: TokenResponse = match token_resp.json().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "Failed to parse token response");
            return (StatusCode::BAD_GATEWAY, "Invalid token response").into_response();
        }
    };

    // Store the OAuth credential in the vault
    let expires_at = token_data
        .expires_in
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));

    let credential = OAuthCredential {
        connector_id: connector_id.clone(),
        provider: connector_id.clone(),
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        token_type: token_data.token_type.unwrap_or_else(|| "Bearer".into()),
        expires_at,
        scopes: provider.default_scopes.clone(),
        token_endpoint: provider.token_url.clone(),
        client_id,
        client_secret,
    };

    if let Err(e) = oauth_store
        .store(
            &credential,
            agentos_types::SecretOwner::Kernel,
            agentos_types::SecretScope::Global,
        )
        .await
    {
        tracing::error!(error = %e, "Failed to store OAuth credential");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to store tokens").into_response();
    }

    tracing::info!(
        connector = %connector_id,
        scopes = ?provider.default_scopes,
        "OAuth flow completed — credential stored"
    );

    Redirect::to("/connectors").into_response()
}

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

/// Generate a cryptographic PKCE code_verifier (32 random bytes → 43 base64url chars).
/// RFC 7636 §4.1 requires 43–128 characters.
fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64url_encode(&bytes)
}

/// Generate the S256 code_challenge from a code_verifier.
fn generate_code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url_encode(&digest)
}

/// Generate a random state parameter (32 bytes, hex-encoded).
fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Base64url-encode without padding (per RFC 7636).
fn base64url_encode(data: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.encode(data)
}

// ---------------------------------------------------------------------------
// Token response
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Provider config loader
// ---------------------------------------------------------------------------

/// Load OAuth provider configurations from config/oauth_providers.toml.
///
/// Returns an empty map if the file doesn't exist (no providers configured).
/// Logs a warning on parse errors and returns an empty map.
pub fn load_provider_configs() -> HashMap<String, OAuthProviderConfig> {
    let config_str = match std::fs::read_to_string("config/oauth_providers.toml") {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };

    match toml::from_str(&config_str) {
        Ok(configs) => configs,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse config/oauth_providers.toml");
            HashMap::new()
        }
    }
}
