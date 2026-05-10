use crate::oauth::OAuthStore;
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_types::TraceID;
use chrono::{Duration, Utc};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// Background service that proactively refreshes OAuth tokens before they expire.
pub struct TokenRefreshLoop {
    oauth_store: Arc<OAuthStore>,
    audit: Arc<AuditLog>,
    cancel: CancellationToken,
    http_client: reqwest::Client,
    /// How far ahead of expiry to trigger a refresh (default: 5 minutes).
    refresh_ahead: Duration,
    /// How often to check for expiring tokens (default: 60 seconds).
    check_interval: std::time::Duration,
}

impl TokenRefreshLoop {
    pub fn new(
        oauth_store: Arc<OAuthStore>,
        audit: Arc<AuditLog>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            oauth_store,
            audit,
            cancel,
            http_client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            refresh_ahead: Duration::minutes(5),
            check_interval: std::time::Duration::from_secs(60),
        }
    }

    /// Override the check interval (useful for testing).
    #[cfg(test)]
    pub fn with_check_interval(mut self, interval: std::time::Duration) -> Self {
        self.check_interval = interval;
        self
    }

    /// Spawn the refresh loop as a background tokio task.
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!("OAuth token refresh loop started");
            loop {
                tokio::select! {
                    _ = self.cancel.cancelled() => {
                        tracing::info!("OAuth token refresh loop shutting down");
                        break;
                    }
                    _ = tokio::time::sleep(self.check_interval) => {
                        if let Err(e) = self.refresh_expiring_tokens().await {
                            tracing::warn!(error = %e, "Token refresh cycle failed");
                        }
                    }
                }
            }
        })
    }

    async fn refresh_expiring_tokens(&self) -> Result<(), agentos_types::AgentOSError> {
        let expiring = self.oauth_store.expiring_within(self.refresh_ahead).await?;

        for cred in expiring {
            let refresh_token = match &cred.refresh_token {
                Some(rt) => rt.clone(),
                None => {
                    tracing::debug!(
                        connector = %cred.connector_id,
                        "Token expiring but no refresh_token — skipping"
                    );
                    continue;
                }
            };

            match self
                .do_token_refresh(
                    &cred.token_endpoint,
                    &cred.client_id,
                    cred.client_secret.as_deref(),
                    &refresh_token,
                )
                .await
            {
                Ok(response) => {
                    let new_expires_at = response
                        .expires_in
                        .map(|secs| Utc::now() + Duration::seconds(secs));

                    if let Err(e) = self
                        .oauth_store
                        .refresh(
                            &cred.connector_id,
                            &response.access_token,
                            new_expires_at,
                            response.refresh_token.as_ref().map(|t| t.as_str()),
                        )
                        .await
                    {
                        tracing::error!(
                            connector = %cred.connector_id,
                            error = %e,
                            "Failed to persist refreshed token"
                        );
                    } else {
                        tracing::info!(
                            connector = %cred.connector_id,
                            expires_in = ?response.expires_in,
                            "OAuth token refreshed"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        connector = %cred.connector_id,
                        error = %e,
                        "OAuth token refresh failed"
                    );

                    audit_log(
                        &self.audit,
                        AuditEventType::OAuthTokenExpired,
                        serde_json::json!({
                            "connector_id": cred.connector_id,
                            "reason": e.to_string(),
                        }),
                    );
                }
            }
        }

        Ok(())
    }

    async fn do_token_refresh(
        &self,
        token_endpoint: &str,
        client_id: &str,
        client_secret: Option<&str>,
        refresh_token: &str,
    ) -> Result<TokenResponse, agentos_types::AgentOSError> {
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ];
        if let Some(secret) = client_secret {
            form.push(("client_secret", secret));
        }

        let resp = self
            .http_client
            .post(token_endpoint)
            .form(&form)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                agentos_types::AgentOSError::VaultError(format!("Token refresh HTTP error: {e}"))
            })?;

        const MAX_TOKEN_RESPONSE_BYTES: usize = 1024 * 1024;
        let status = resp.status();
        // Pre-check Content-Length to reject oversized responses before buffering.
        if let Some(len) = resp.content_length() {
            if len > MAX_TOKEN_RESPONSE_BYTES as u64 {
                return Err(agentos_types::AgentOSError::VaultError(format!(
                    "Token refresh response too large: {} bytes (limit: {} bytes)",
                    len, MAX_TOKEN_RESPONSE_BYTES
                )));
            }
        }
        let body_bytes = resp.bytes().await.unwrap_or_default();
        if body_bytes.len() > MAX_TOKEN_RESPONSE_BYTES {
            return Err(agentos_types::AgentOSError::VaultError(format!(
                "Token refresh response too large: {} bytes",
                body_bytes.len()
            )));
        }

        if !status.is_success() {
            let body = String::from_utf8_lossy(&body_bytes);
            return Err(agentos_types::AgentOSError::VaultError(format!(
                "Token refresh failed: HTTP {status} — {body}"
            )));
        }

        let token_response: TokenResponse = serde_json::from_slice(&body_bytes).map_err(|e| {
            agentos_types::AgentOSError::VaultError(format!(
                "Failed to parse token refresh response: {e}"
            ))
        })?;

        Ok(token_response)
    }
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: Zeroizing<String>,
    #[serde(default)]
    refresh_token: Option<Zeroizing<String>>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[allow(dead_code)]
    #[serde(default)]
    token_type: Option<String>,
}

fn audit_log(audit: &AuditLog, event_type: AuditEventType, details: serde_json::Value) {
    if let Err(e) = audit.append(AuditEntry {
        timestamp: Utc::now(),
        trace_id: TraceID::new(),
        event_type,
        agent_id: None,
        task_id: None,
        tool_id: None,
        details,
        severity: AuditSeverity::Security,
        reversible: false,
        rollback_ref: None,
    }) {
        tracing::error!(error = %e, "Failed to write OAuth audit entry");
    }
}
