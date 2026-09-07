//! OAuth2 Authorization-Code + PKCE flow for connectors.
//!
//! Shared by the REST API (`agentos-api`) and the legacy HTMX UI
//! (`agentos-web`): both are thin HTTP shells over [`Kernel::oauth_begin`] and
//! [`Kernel::oauth_complete`]. Pending-flow state (CSRF `state`, PKCE verifier)
//! lives in the vault's OAuth store with a 10-minute TTL.

use std::collections::HashMap;
use std::path::PathBuf;

use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_types::TraceID;
use agentos_vault::{OAuthCredential, OAuthPendingFlow};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::kernel::Kernel;

/// One `[provider]` block of `oauth_providers.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthProviderConfig {
    pub authorize_url: String,
    pub token_url: String,
    /// Environment variable holding the client ID.
    pub client_id_env: String,
    /// Vault secret key holding the client secret.
    pub client_secret_vault_key: String,
    #[serde(default)]
    pub default_scopes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthFlowError {
    #[error("No OAuth provider configured for '{0}' — add a [{0}] block to oauth_providers.toml")]
    NoProvider(String),
    /// Provider block exists but is unusable (client id env var missing, bad URL).
    #[error("{0}")]
    Config(String),
    #[error("Invalid or expired OAuth state")]
    InvalidState,
    /// Provider-side failure (token exchange).
    #[error("{0}")]
    Exchange(String),
    #[error("{0}")]
    Store(String),
}

/// Candidate locations for `oauth_providers.toml`, most specific first:
/// next to the active config file (`$AGENTOS_CONFIG`'s directory, i.e.
/// `~/.agentos/config/…` on a real install), then the repo-relative path.
pub fn provider_config_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cfg) = std::env::var("AGENTOS_CONFIG") {
        let cfg = PathBuf::from(cfg);
        if let Some(dir) = cfg.parent() {
            // `~/.agentos/config.toml` → `~/.agentos/config/oauth_providers.toml`
            out.push(dir.join("config").join("oauth_providers.toml"));
            // `config/default.toml` → `config/oauth_providers.toml`
            out.push(dir.join("oauth_providers.toml"));
        }
    }
    out.push(PathBuf::from("config/oauth_providers.toml"));
    out
}

/// Load provider blocks from the first `oauth_providers.toml` that exists.
/// Missing file → empty map; parse error → warning + empty map.
pub async fn load_provider_configs() -> HashMap<String, OAuthProviderConfig> {
    for path in provider_config_paths() {
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        return match toml::from_str(&text) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Failed to parse oauth_providers.toml");
                HashMap::new()
            }
        };
    }
    HashMap::new()
}

fn base64url(data: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.encode(data)
}

/// RFC 7636 §4.1: 32 random bytes → 43 base64url chars.
fn code_verifier() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    base64url(&b)
}

/// S256 challenge for a verifier.
pub fn code_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn csrf_state() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

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

impl Kernel {
    /// Start the flow: mint state + PKCE verifier, persist the pending flow,
    /// and return the provider's authorization URL to send the browser to.
    /// `redirect_uri` must be the absolute URL of the caller's callback route.
    pub async fn oauth_begin(
        &self,
        connector_id: &str,
        redirect_uri: &str,
    ) -> Result<String, OAuthFlowError> {
        let providers = load_provider_configs().await;
        let provider = providers
            .get(connector_id)
            .ok_or_else(|| OAuthFlowError::NoProvider(connector_id.to_string()))?;

        let client_id = match std::env::var(&provider.client_id_env) {
            Ok(id) if !id.is_empty() => id,
            _ => {
                return Err(OAuthFlowError::Config(format!(
                    "Environment variable {} is not set for connector '{connector_id}'",
                    provider.client_id_env
                )))
            }
        };

        let verifier = code_verifier();
        let challenge = code_challenge(&verifier);
        let state = csrf_state();
        let now = chrono::Utc::now();
        let flow = OAuthPendingFlow {
            connector_id: connector_id.to_string(),
            state: state.clone(),
            code_verifier: Some(verifier),
            redirect_uri: redirect_uri.to_string(),
            created_at: now,
            expires_at: now + chrono::Duration::minutes(10),
        };
        self.vault
            .oauth_store()
            .store_pending_flow(&flow)
            .await
            .map_err(|e| OAuthFlowError::Store(e.to_string()))?;

        let mut url = url::Url::parse(&provider.authorize_url).map_err(|e| {
            OAuthFlowError::Config(format!(
                "authorize_url for '{connector_id}' is invalid: {e}"
            ))
        })?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &provider.default_scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");

        let _ = self.audit.append(AuditEntry {
            timestamp: now,
            trace_id: TraceID::new(),
            event_type: AuditEventType::OAuthFlowStarted,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "connector_id": connector_id }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });
        Ok(url.into())
    }

    /// Finish the flow from the provider callback: validate `state`, exchange
    /// the code (with the stored PKCE verifier), and store the credential.
    pub async fn oauth_complete(
        &self,
        connector_id: &str,
        code: &str,
        state: &str,
    ) -> Result<(), OAuthFlowError> {
        let store = self.vault.oauth_store();
        let flow = store
            .complete_pending_flow(state)
            .await
            .map_err(|_| OAuthFlowError::InvalidState)?;
        if flow.connector_id != connector_id {
            return Err(OAuthFlowError::InvalidState);
        }

        let providers = load_provider_configs().await;
        let provider = providers
            .get(connector_id)
            .ok_or_else(|| OAuthFlowError::NoProvider(connector_id.to_string()))?;
        let client_id = std::env::var(&provider.client_id_env)
            .map_err(|_| OAuthFlowError::Config("Client ID not configured".into()))?;

        // Missing secret is fine for public (PKCE-only) clients; other vault
        // errors are logged and treated the same way.
        let client_secret = match self.vault.get(&provider.client_secret_vault_key).await {
            Ok(s) => Some(s.as_str().to_string()),
            Err(agentos_types::AgentOSError::SecretNotFound(_)) => None,
            Err(e) => {
                tracing::warn!(connector = %connector_id, error = %e, "Vault error reading client secret");
                None
            }
        };

        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", flow.redirect_uri.clone()),
            ("client_id", client_id.clone()),
        ];
        if let Some(v) = &flow.code_verifier {
            form.push(("code_verifier", v.clone()));
        }
        if let Some(s) = &client_secret {
            form.push(("client_secret", s.clone()));
        }

        let resp = reqwest::Client::new()
            .post(&provider.token_url)
            .header("Accept", "application/json")
            .form(&form)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| OAuthFlowError::Exchange(format!("Token exchange failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body: String = resp
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            tracing::error!(%status, body = %body, "Token exchange returned error");
            return Err(OAuthFlowError::Exchange(format!(
                "Token exchange failed: HTTP {status}"
            )));
        }
        let token: TokenResponse = resp
            .json()
            .await
            .map_err(|e| OAuthFlowError::Exchange(format!("Invalid token response: {e}")))?;

        let credential = OAuthCredential {
            connector_id: connector_id.to_string(),
            provider: connector_id.to_string(),
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            token_type: token.token_type.unwrap_or_else(|| "Bearer".into()),
            expires_at: token
                .expires_in
                .map(|s| chrono::Utc::now() + chrono::Duration::seconds(s)),
            scopes: provider.default_scopes.clone(),
            token_endpoint: provider.token_url.clone(),
            client_id,
            client_secret,
        };
        store
            .store(
                &credential,
                agentos_types::SecretOwner::Kernel,
                agentos_types::SecretScope::Global,
            )
            .await
            .map_err(|e| OAuthFlowError::Store(format!("Failed to store tokens: {e}")))?;

        let _ = self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::OAuthFlowCompleted,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "connector_id": connector_id,
                "scopes": provider.default_scopes,
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });
        tracing::info!(connector = %connector_id, "OAuth flow completed — credential stored");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_s256_of_verifier() {
        // RFC 7636 appendix B vector.
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge(v),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_length_is_rfc_compliant() {
        let v = code_verifier();
        assert!((43..=128).contains(&v.len()));
    }

    #[tokio::test]
    async fn missing_provider_file_is_empty() {
        std::env::set_var("AGENTOS_CONFIG", "/nonexistent/agentos/config.toml");
        let cwd = std::env::current_dir().unwrap();
        // The repo-relative fallback resolves against cwd; run from a temp dir so
        // the checked-in (all-commented) file cannot leak in either way.
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let map = load_provider_configs().await;
        std::env::set_current_dir(cwd).unwrap();
        assert!(map.is_empty());
    }
}
