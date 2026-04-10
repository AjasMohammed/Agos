/// Vault-backed OAuth2 token provider for MCP HTTP transports.
///
/// Reads OAuth credentials from the vault, transparently refreshes tokens
/// that are near expiry, and serializes concurrent refresh operations via a
/// `Mutex` so only one HTTP call is made even under concurrent tool calls.
use std::sync::Arc;

use agentos_mcp::transport::McpTransportError;
use agentos_mcp::OAuthTokenProvider;
use agentos_vault::{OAuthStore, SecretsVault};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use tokio::sync::Mutex;

/// How far ahead of token expiry to trigger a proactive refresh.
const REFRESH_AHEAD_SECS: i64 = 120; // 2 minutes

/// `OAuthTokenProvider` implementation backed by the AgentOS vault.
///
/// Token refresh calls are serialized via a `Mutex` to avoid thundering-herd
/// when multiple concurrent MCP tool calls discover the same expired token.
pub struct VaultOAuthProvider {
    connector_id: String,
    oauth_store: OAuthStore,
    http_client: reqwest::Client,
    /// Held while a refresh HTTP call is in flight. Prevents concurrent refreshes.
    refresh_lock: Mutex<()>,
}

impl VaultOAuthProvider {
    /// Build a provider from a connector ID and an already-opened vault.
    ///
    /// The vault's `oauth_store()` shares the vault's DB connection and
    /// master key — no additional credentials are required.
    pub fn new(connector_id: String, vault: &SecretsVault) -> Result<Self, McpTransportError> {
        let http_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| {
                McpTransportError::Connection(format!(
                    "Failed to build OAuth refresh HTTP client: {}",
                    e
                ))
            })?;
        Ok(Self {
            connector_id,
            oauth_store: vault.oauth_store(),
            http_client,
            refresh_lock: Mutex::new(()),
        })
    }
}

#[async_trait]
impl OAuthTokenProvider for VaultOAuthProvider {
    async fn get_token(&self) -> Result<String, McpTransportError> {
        let cred = self
            .oauth_store
            .get(&self.connector_id)
            .await
            .map_err(|e| {
                McpTransportError::Auth(format!(
                    "OAuth credential '{}' not found in vault: {}",
                    self.connector_id, e
                ))
            })?;

        // If the token expires within REFRESH_AHEAD_SECS, refresh proactively.
        let needs_refresh = cred
            .expires_at
            .is_some_and(|exp| (exp - Utc::now()) < Duration::seconds(REFRESH_AHEAD_SECS));

        if needs_refresh {
            if cred.refresh_token.is_some() {
                tracing::debug!(
                    connector = %self.connector_id,
                    "OAuth token near expiry — refreshing proactively"
                );
                return self.do_refresh().await;
            } else {
                tracing::warn!(
                    connector = %self.connector_id,
                    "OAuth token is near expiry but no refresh_token is stored — requests may fail with 401. Re-run 'agentos mcp oauth-store' with a new token."
                );
            }
        }

        Ok(cred.access_token.clone())
    }

    async fn force_refresh(&self) -> Result<String, McpTransportError> {
        self.do_refresh().await
    }
}

impl VaultOAuthProvider {
    /// Perform the token refresh: serialize via lock, call token endpoint,
    /// persist new credentials to vault, return fresh access token.
    async fn do_refresh(&self) -> Result<String, McpTransportError> {
        // Serialize concurrent refreshes — only the first caller does the HTTP
        // round-trip; subsequent callers will read the fresh token from the vault.
        let _guard = self.refresh_lock.lock().await;

        // Re-read after acquiring the lock — another caller may have already refreshed.
        let cred = self
            .oauth_store
            .get(&self.connector_id)
            .await
            .map_err(|e| McpTransportError::Auth(format!("Vault read failed: {}", e)))?;

        let refresh_token = cred.refresh_token.as_deref().ok_or_else(|| {
            McpTransportError::Auth(format!(
                "OAuth credential '{}' has no refresh_token; cannot refresh",
                self.connector_id
            ))
        })?;

        // Check if another task already refreshed while we waited for the lock.
        // `None` expiry means unknown — always proceed with the HTTP refresh
        // (this is the force_refresh / 401-recovery path; the token is known-invalid).
        let still_needs_refresh = cred
            .expires_at
            .is_none_or(|exp| (exp - Utc::now()) < Duration::seconds(REFRESH_AHEAD_SECS));
        if !still_needs_refresh {
            // Another concurrent caller already refreshed; return the fresh token.
            return Ok(cred.access_token.clone());
        }

        // Build the refresh form.
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", cred.client_id.as_str()),
        ];
        let secret_ref;
        if let Some(ref secret) = cred.client_secret {
            secret_ref = secret.clone();
            form.push(("client_secret", secret_ref.as_str()));
        }

        let resp = self
            .http_client
            .post(&cred.token_endpoint)
            .form(&form)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| McpTransportError::Auth(format!("Token refresh HTTP error: {}", e)))?;

        // Cap response size before reading body — covers both error and success paths.
        // Apply Content-Length pre-check first to reject oversized responses immediately.
        const MAX_TOKEN_RESPONSE_BYTES: usize = 1024 * 1024;
        let status = resp.status();
        if let Some(len) = resp.content_length() {
            if len > MAX_TOKEN_RESPONSE_BYTES as u64 {
                return Err(McpTransportError::Auth(format!(
                    "Token refresh response too large: {} bytes (limit: {} bytes)",
                    len, MAX_TOKEN_RESPONSE_BYTES
                )));
            }
        }
        let body_bytes = resp.bytes().await.map_err(|e| {
            McpTransportError::Auth(format!("Failed to read token refresh response: {}", e))
        })?;
        if body_bytes.len() > MAX_TOKEN_RESPONSE_BYTES {
            return Err(McpTransportError::Auth(format!(
                "Token refresh response too large: {} bytes (limit: {} bytes)",
                body_bytes.len(),
                MAX_TOKEN_RESPONSE_BYTES
            )));
        }

        if !status.is_success() {
            let body = String::from_utf8_lossy(&body_bytes);
            let body_excerpt = &body[..body.len().min(512)];
            return Err(McpTransportError::Auth(format!(
                "Token refresh failed: HTTP {} — {}",
                status, body_excerpt
            )));
        }

        #[derive(serde::Deserialize)]
        struct TokenResponse {
            access_token: String,
            #[serde(default)]
            refresh_token: Option<String>,
            #[serde(default)]
            expires_in: Option<i64>,
        }

        let token_resp: TokenResponse = serde_json::from_slice(&body_bytes).map_err(|e| {
            McpTransportError::Auth(format!("Failed to parse token refresh response: {}", e))
        })?;

        let new_expires_at = token_resp
            .expires_in
            .map(|secs| Utc::now() + Duration::seconds(secs));

        // Persist refreshed credentials back to vault.
        if let Err(e) = self
            .oauth_store
            .refresh(
                &self.connector_id,
                &token_resp.access_token,
                new_expires_at,
                token_resp.refresh_token.as_deref(),
            )
            .await
        {
            // Log but don't fail — we still have a valid token for this request.
            tracing::warn!(
                connector = %self.connector_id,
                error = %e,
                "Failed to persist refreshed OAuth token to vault"
            );
        }

        tracing::info!(
            connector = %self.connector_id,
            expires_in = ?token_resp.expires_in,
            "OAuth token refreshed for MCP server"
        );

        Ok(token_resp.access_token)
    }
}

// Re-export so callers can use the Arc-wrapped form without importing the struct directly.
pub type ArcVaultOAuthProvider = Arc<VaultOAuthProvider>;
