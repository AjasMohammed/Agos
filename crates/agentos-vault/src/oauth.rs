use crate::crypto::{decrypt, encrypt};
use crate::master_key::MasterKey;
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_types::*;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// OAuth2 credential stored in the vault (encrypted at rest).
///
/// The sensitive fields (`access_token`, `refresh_token`) are `String` and
/// will be zeroed via the manual `Drop` impl. We don't derive `Zeroize`
/// because `DateTime<Utc>` and `Vec<String>` don't implement it.
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub connector_id: String,
    pub provider: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
    pub token_endpoint: String,
    pub client_id: String,
    /// Client secret for confidential OAuth2 clients (encrypted at rest).
    /// Required by most providers for token refresh.
    #[serde(default)]
    pub client_secret: Option<String>,
}

impl std::fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("connector_id", &self.connector_id)
            .field("provider", &self.provider)
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("token_endpoint", &self.token_endpoint)
            .field("client_id", &self.client_id)
            .finish()
    }
}

impl Drop for OAuthCredential {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.access_token.zeroize();
        if let Some(ref mut rt) = self.refresh_token {
            rt.zeroize();
        }
        if let Some(ref mut cs) = self.client_secret {
            cs.zeroize();
        }
    }
}

/// Non-secret metadata returned by `list_oauth()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentialMeta {
    pub connector_id: String,
    pub provider: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub refreshed_at: Option<DateTime<Utc>>,
}

/// Tracks an in-progress OAuth2 authorization code flow (PKCE state).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthPendingFlow {
    pub connector_id: String,
    pub state: String,
    pub code_verifier: Option<String>,
    pub redirect_uri: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Schema creation
// ---------------------------------------------------------------------------

/// Create OAuth-related tables. Called from `SecretsVault::initialize` and
/// `SecretsVault::open` (idempotent via IF NOT EXISTS).
pub(crate) fn create_oauth_tables(conn: &Connection) -> Result<(), AgentOSError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS oauth_credentials (
            connector_id    TEXT PRIMARY KEY,
            provider        TEXT NOT NULL,
            encrypted_payload BLOB NOT NULL,
            expires_at      TEXT,
            owner           TEXT NOT NULL,
            scope           TEXT NOT NULL,
            created_at      TEXT NOT NULL,
            refreshed_at    TEXT
        );

        CREATE TABLE IF NOT EXISTS oauth_pending_flows (
            state           TEXT PRIMARY KEY,
            connector_id    TEXT NOT NULL,
            encrypted_verifier BLOB,
            redirect_uri    TEXT NOT NULL,
            created_at      TEXT NOT NULL,
            expires_at      TEXT NOT NULL
        );
        ",
    )
    .map_err(|e| AgentOSError::VaultError(format!("OAuth table creation failed: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// OAuthStore — all operations that touch the OAuth tables
// ---------------------------------------------------------------------------

/// Encapsulates all OAuth credential operations on the vault database.
/// Constructed by `SecretsVault` so it shares the same connection + key.
pub struct OAuthStore {
    conn: Arc<Mutex<Connection>>,
    master_key: Arc<MasterKey>,
    audit: Arc<AuditLog>,
}

impl OAuthStore {
    pub(crate) fn new(
        conn: Arc<Mutex<Connection>>,
        master_key: Arc<MasterKey>,
        audit: Arc<AuditLog>,
    ) -> Self {
        Self {
            conn,
            master_key,
            audit,
        }
    }

    /// Store an OAuth credential (encrypts the full payload at rest).
    ///
    /// Validates `token_endpoint` against SSRF (must be HTTPS, no private IPs).
    pub async fn store(
        &self,
        credential: &OAuthCredential,
        owner: SecretOwner,
        scope: SecretScope,
    ) -> Result<(), AgentOSError> {
        validate_token_endpoint(&credential.token_endpoint)?;

        let payload_json = serde_json::to_vec(credential).map_err(|e| {
            AgentOSError::Serialization(format!("Failed to serialize OAuth credential: {e}"))
        })?;
        let encrypted = encrypt(&self.master_key, &payload_json)?;

        let owner_json = serde_json::to_string(&owner)
            .map_err(|e| AgentOSError::Serialization(format!("Failed to serialize owner: {e}")))?;
        let scope_json = serde_json::to_string(&scope)
            .map_err(|e| AgentOSError::Serialization(format!("Failed to serialize scope: {e}")))?;
        let now = Utc::now();
        let expires_at_str = credential.expires_at.map(|dt| dt.to_rfc3339());

        {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO oauth_credentials
                     (connector_id, provider, encrypted_payload, expires_at, owner, scope, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(connector_id) DO UPDATE SET
                     provider=excluded.provider,
                     encrypted_payload=excluded.encrypted_payload,
                     expires_at=excluded.expires_at,
                     owner=excluded.owner,
                     scope=excluded.scope,
                     refreshed_at=?8",
                params![
                    credential.connector_id,
                    credential.provider,
                    encrypted,
                    expires_at_str,
                    owner_json,
                    scope_json,
                    now.to_rfc3339(),
                    now.to_rfc3339(),
                ],
            )
            .map_err(|e| AgentOSError::VaultError(format!("Failed to store OAuth credential: {e}")))?;
        }

        audit_log(
            &self.audit,
            AuditEventType::OAuthCredentialStored,
            serde_json::json!({
                "connector_id": credential.connector_id,
                "provider": credential.provider,
                "scopes": credential.scopes,
            }),
        );

        Ok(())
    }

    /// Retrieve a decrypted OAuth credential by connector_id.
    pub async fn get(&self, connector_id: &str) -> Result<OAuthCredential, AgentOSError> {
        let encrypted: Vec<u8> = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT encrypted_payload FROM oauth_credentials WHERE connector_id = ?1",
                params![connector_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AgentOSError::VaultError(format!("DB error during OAuth get: {e}")))?
            .ok_or_else(|| {
                AgentOSError::VaultError(format!("OAuth credential not found: {connector_id}"))
            })?
        };

        let decrypted = decrypt(&self.master_key, &encrypted)?;
        let credential: OAuthCredential = serde_json::from_slice(&decrypted).map_err(|e| {
            AgentOSError::VaultError(format!("Failed to deserialize OAuth credential: {e}"))
        })?;

        Ok(credential)
    }

    /// Delete an OAuth credential.
    pub async fn delete(&self, connector_id: &str) -> Result<(), AgentOSError> {
        let deleted = {
            let conn = self.conn.lock().await;
            conn.execute(
                "DELETE FROM oauth_credentials WHERE connector_id = ?1",
                params![connector_id],
            )
            .map_err(|e| {
                AgentOSError::VaultError(format!("Failed to delete OAuth credential: {e}"))
            })?
        };

        if deleted == 0 {
            return Err(AgentOSError::VaultError(format!(
                "OAuth credential not found: {connector_id}"
            )));
        }

        audit_log(
            &self.audit,
            AuditEventType::OAuthCredentialDeleted,
            serde_json::json!({ "connector_id": connector_id }),
        );

        Ok(())
    }

    /// List all OAuth credentials (metadata only — no secrets).
    pub async fn list(&self) -> Result<Vec<OAuthCredentialMeta>, AgentOSError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT connector_id, provider, expires_at, created_at, refreshed_at, encrypted_payload
                 FROM oauth_credentials ORDER BY connector_id",
            )
            .map_err(|e| AgentOSError::VaultError(format!("Failed to prepare OAuth list: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                let connector_id: String = row.get(0)?;
                let provider: String = row.get(1)?;
                let expires_at_str: Option<String> = row.get(2)?;
                let created_at_str: String = row.get(3)?;
                let refreshed_at_str: Option<String> = row.get(4)?;
                let encrypted: Vec<u8> = row.get(5)?;

                Ok((
                    connector_id,
                    provider,
                    expires_at_str,
                    created_at_str,
                    refreshed_at_str,
                    encrypted,
                ))
            })
            .map_err(|e| AgentOSError::VaultError(format!("Failed to query OAuth list: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            let (
                connector_id,
                provider,
                expires_at_str,
                created_at_str,
                refreshed_at_str,
                encrypted,
            ) = row.map_err(|e| AgentOSError::VaultError(e.to_string()))?;

            // Decrypt to extract scopes (metadata, not a secret)
            let scopes = match decrypt(&self.master_key, &encrypted) {
                Ok(decrypted) => serde_json::from_slice::<OAuthCredential>(&decrypted)
                    .map(|c| c.scopes.clone())
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            };

            let expires_at = expires_at_str.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            });
            let created_at = DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_default();
            let refreshed_at = refreshed_at_str.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            });

            results.push(OAuthCredentialMeta {
                connector_id,
                provider,
                scopes,
                expires_at,
                created_at,
                refreshed_at,
            });
        }

        Ok(results)
    }

    /// Update access token and expiry after a successful refresh.
    pub async fn refresh(
        &self,
        connector_id: &str,
        new_access_token: &str,
        new_expires_at: Option<DateTime<Utc>>,
        new_refresh_token: Option<&str>,
    ) -> Result<(), AgentOSError> {
        // Read → decrypt → update fields → re-encrypt → write
        let mut credential = self.get(connector_id).await?;
        credential.access_token = new_access_token.to_string();
        credential.expires_at = new_expires_at;
        if let Some(rt) = new_refresh_token {
            credential.refresh_token = Some(rt.to_string());
        }

        let payload_json = serde_json::to_vec(&credential).map_err(|e| {
            AgentOSError::Serialization(format!("Failed to serialize refreshed credential: {e}"))
        })?;
        let encrypted = encrypt(&self.master_key, &payload_json)?;

        let now = Utc::now();
        let expires_at_str = new_expires_at.map(|dt| dt.to_rfc3339());

        {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE oauth_credentials
                 SET encrypted_payload = ?1, expires_at = ?2, refreshed_at = ?3
                 WHERE connector_id = ?4",
                params![encrypted, expires_at_str, now.to_rfc3339(), connector_id],
            )
            .map_err(|e| AgentOSError::VaultError(format!("Failed to refresh OAuth token: {e}")))?;
        }

        audit_log(
            &self.audit,
            AuditEventType::OAuthTokenRefreshed,
            serde_json::json!({
                "connector_id": connector_id,
                "new_expires_at": expires_at_str,
            }),
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pending flows (PKCE state tracking)
    // -----------------------------------------------------------------------

    /// Store a pending OAuth flow for PKCE state tracking.
    pub async fn store_pending_flow(&self, flow: &OAuthPendingFlow) -> Result<(), AgentOSError> {
        let encrypted_verifier = match &flow.code_verifier {
            Some(v) => Some(encrypt(&self.master_key, v.as_bytes())?),
            None => None,
        };

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO oauth_pending_flows
                 (state, connector_id, encrypted_verifier, redirect_uri, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                flow.state,
                flow.connector_id,
                encrypted_verifier,
                flow.redirect_uri,
                flow.created_at.to_rfc3339(),
                flow.expires_at.to_rfc3339(),
            ],
        )
        .map_err(|e| {
            AgentOSError::VaultError(format!("Failed to store pending OAuth flow: {e}"))
        })?;

        audit_log(
            &self.audit,
            AuditEventType::OAuthFlowStarted,
            serde_json::json!({
                "connector_id": flow.connector_id,
                "state_prefix": &flow.state[..8.min(flow.state.len())],
            }),
        );

        Ok(())
    }

    /// Complete a pending flow: look up by `state`, decrypt the code_verifier,
    /// delete the row (single-use), and return the flow details.
    pub async fn complete_pending_flow(
        &self,
        state: &str,
    ) -> Result<OAuthPendingFlow, AgentOSError> {
        let conn = self.conn.lock().await;

        #[allow(clippy::type_complexity)]
        let row: Option<(String, Option<Vec<u8>>, String, String, String)> = conn
            .query_row(
                "SELECT connector_id, encrypted_verifier, redirect_uri, created_at, expires_at
                 FROM oauth_pending_flows WHERE state = ?1",
                params![state],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| {
                AgentOSError::VaultError(format!("DB error during flow completion: {e}"))
            })?;

        let (connector_id, encrypted_verifier, redirect_uri, created_at_str, expires_at_str) = row
            .ok_or_else(|| {
                AgentOSError::VaultError(format!(
                    "No pending OAuth flow for state: {}",
                    &state[..8.min(state.len())]
                ))
            })?;

        // Check expiry BEFORE deleting — an expired flow should still be cleaned up
        // but the caller should know it expired.
        let expires_at = DateTime::parse_from_rfc3339(&expires_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| {
                AgentOSError::VaultError(format!("Invalid expires_at in pending flow: {e}"))
            })?;

        let expired = Utc::now() > expires_at;

        // Single-use: always delete regardless of expiry
        conn.execute(
            "DELETE FROM oauth_pending_flows WHERE state = ?1",
            params![state],
        )
        .map_err(|e| AgentOSError::VaultError(format!("Failed to delete pending flow: {e}")))?;

        if expired {
            return Err(AgentOSError::VaultError(
                "Pending OAuth flow has expired".into(),
            ));
        }

        let code_verifier = match encrypted_verifier {
            Some(ev) => {
                let decrypted = decrypt(&self.master_key, &ev)?;
                Some(String::from_utf8(decrypted).map_err(|_| {
                    AgentOSError::VaultError("Code verifier was not valid UTF-8".into())
                })?)
            }
            None => None,
        };

        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_default();

        audit_log(
            &self.audit,
            AuditEventType::OAuthFlowCompleted,
            serde_json::json!({ "connector_id": connector_id }),
        );

        Ok(OAuthPendingFlow {
            connector_id,
            state: state.to_string(),
            code_verifier,
            redirect_uri,
            created_at,
            expires_at,
        })
    }

    /// Delete expired pending flows. Returns the number of rows deleted.
    pub async fn sweep_expired_flows(&self) -> Result<u64, AgentOSError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM oauth_pending_flows WHERE expires_at < ?1",
                params![now],
            )
            .map_err(|e| AgentOSError::VaultError(format!("Failed to sweep expired flows: {e}")))?;

        if deleted > 0 {
            tracing::debug!(deleted, "Swept expired OAuth pending flows");
        }

        Ok(deleted as u64)
    }

    /// List credentials whose access token expires within `within` duration.
    /// Used by the token refresh loop to find tokens that need refreshing.
    pub async fn expiring_within(
        &self,
        within: chrono::Duration,
    ) -> Result<Vec<OAuthCredential>, AgentOSError> {
        let deadline = (Utc::now() + within).to_rfc3339();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT encrypted_payload FROM oauth_credentials
                 WHERE expires_at IS NOT NULL AND expires_at < ?1",
            )
            .map_err(|e| {
                AgentOSError::VaultError(format!("Failed to prepare expiring query: {e}"))
            })?;

        let rows = stmt
            .query_map(params![deadline], |row| {
                let encrypted: Vec<u8> = row.get(0)?;
                Ok(encrypted)
            })
            .map_err(|e| {
                AgentOSError::VaultError(format!("Failed to query expiring creds: {e}"))
            })?;

        let mut results = Vec::new();
        for row in rows {
            let encrypted = row.map_err(|e| AgentOSError::VaultError(e.to_string()))?;
            if let Ok(decrypted) = decrypt(&self.master_key, &encrypted) {
                if let Ok(cred) = serde_json::from_slice::<OAuthCredential>(&decrypted) {
                    results.push(cred);
                }
            }
        }

        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Validate that a token endpoint URL is safe against SSRF attacks.
/// Requires HTTPS and rejects private/loopback addresses.
fn validate_token_endpoint(url: &str) -> Result<(), AgentOSError> {
    if !url.starts_with("https://") {
        return Err(AgentOSError::VaultError(
            "OAuth token_endpoint must use HTTPS".into(),
        ));
    }

    // Extract host portion from URL (between :// and next / or end).
    // Strip userinfo (user@host syntax) to prevent SSRF bypass via
    // crafted URLs like "https://evil@127.0.0.1/token".
    let host_port = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    // After stripping userinfo, take the port-less hostname.
    let host = host_port
        .rsplit_once('@')
        .map_or(host_port, |(_, h)| h)
        .split(':')
        .next()
        .unwrap_or("");

    let blocked_prefixes = [
        "localhost",
        "127.",
        "10.",
        "192.168.",
        "169.254.",
        "172.16.",
        "172.17.",
        "172.18.",
        "172.19.",
        "172.20.",
        "172.21.",
        "172.22.",
        "172.23.",
        "172.24.",
        "172.25.",
        "172.26.",
        "172.27.",
        "172.28.",
        "172.29.",
        "172.30.",
        "172.31.",
        "0.",
        "[::1]",
        "[fe80:",
        "[fc",
        "[fd",
    ];

    let host_lower = host.to_lowercase();
    for prefix in &blocked_prefixes {
        if host_lower.starts_with(prefix) {
            return Err(AgentOSError::VaultError(format!(
                "OAuth token_endpoint must not target private/loopback addresses (got '{host}')"
            )));
        }
    }

    Ok(())
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
        tracing::error!(error = %e, "Failed to write OAuth audit log entry");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::master_key::ZeroizingString;
    use agentos_audit::AuditLog;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn setup_db() -> (
        Arc<Mutex<Connection>>,
        Arc<MasterKey>,
        Arc<AuditLog>,
        TempDir,
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test_vault.db");
        let audit_path = tmp.path().join("test_audit.db");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        create_oauth_tables(&conn).unwrap();

        let passphrase = ZeroizingString::new("test-passphrase".into());
        let salt = MasterKey::generate_salt();
        let master_key = MasterKey::derive(&passphrase, &salt).unwrap();

        let audit = AuditLog::open(&audit_path).unwrap();

        (
            Arc::new(Mutex::new(conn)),
            Arc::new(master_key),
            Arc::new(audit),
            tmp,
        )
    }

    fn make_credential(connector_id: &str) -> OAuthCredential {
        OAuthCredential {
            connector_id: connector_id.to_string(),
            provider: "github".into(),
            access_token: "gho_abc123".into(),
            refresh_token: Some("ghr_refresh456".into()),
            token_type: "Bearer".into(),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            scopes: vec!["repo".into(), "read:org".into()],
            token_endpoint: "https://github.com/login/oauth/access_token".into(),
            client_id: "client_abc".into(),
            client_secret: None,
        }
    }

    #[tokio::test]
    async fn test_store_and_get() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        let cred = make_credential("github");
        store
            .store(&cred, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        let retrieved = store.get("github").await.unwrap();
        assert_eq!(retrieved.connector_id, "github");
        assert_eq!(retrieved.provider, "github");
        assert_eq!(retrieved.access_token, "gho_abc123");
        assert_eq!(retrieved.refresh_token.as_deref(), Some("ghr_refresh456"));
        assert_eq!(retrieved.scopes, vec!["repo", "read:org"]);
    }

    #[tokio::test]
    async fn test_store_encrypted_at_rest() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn.clone(), key, audit);

        let cred = make_credential("github");
        store
            .store(&cred, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        // Read raw blob from SQLite — should NOT be plaintext JSON
        let raw: Vec<u8> = {
            let db = conn.lock().await;
            db.query_row(
                "SELECT encrypted_payload FROM oauth_credentials WHERE connector_id = 'github'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        let raw_str = String::from_utf8_lossy(&raw);
        assert!(
            !raw_str.contains("gho_abc123"),
            "access_token found in plaintext!"
        );
        assert!(
            !raw_str.contains("ghr_refresh456"),
            "refresh_token found in plaintext!"
        );
    }

    #[tokio::test]
    async fn test_delete() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        let cred = make_credential("github");
        store
            .store(&cred, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        store.delete("github").await.unwrap();
        assert!(store.get("github").await.is_err());
    }

    #[tokio::test]
    async fn test_delete_not_found() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);
        assert!(store.delete("nonexistent").await.is_err());
    }

    #[tokio::test]
    async fn test_list() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        let cred1 = make_credential("github");
        let mut cred2 = make_credential("slack");
        cred2.provider = "slack".into();
        cred2.scopes = vec!["chat:write".into()];

        store
            .store(&cred1, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();
        store
            .store(&cred2, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        let list = store.list().await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].connector_id, "github");
        assert_eq!(list[1].connector_id, "slack");
        assert_eq!(list[0].scopes, vec!["repo", "read:org"]);
        assert_eq!(list[1].scopes, vec!["chat:write"]);
    }

    #[tokio::test]
    async fn test_refresh_updates_token() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        let cred = make_credential("github");
        store
            .store(&cred, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        let new_expiry = Utc::now() + chrono::Duration::hours(2);
        store
            .refresh("github", "gho_new_token_789", Some(new_expiry), None)
            .await
            .unwrap();

        let updated = store.get("github").await.unwrap();
        assert_eq!(updated.access_token, "gho_new_token_789");
        // Refresh token should remain unchanged
        assert_eq!(updated.refresh_token.as_deref(), Some("ghr_refresh456"));
    }

    #[tokio::test]
    async fn test_refresh_with_new_refresh_token() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        let cred = make_credential("github");
        store
            .store(&cred, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        store
            .refresh("github", "gho_new", None, Some("ghr_rotated"))
            .await
            .unwrap();

        let updated = store.get("github").await.unwrap();
        assert_eq!(updated.access_token, "gho_new");
        assert_eq!(updated.refresh_token.as_deref(), Some("ghr_rotated"));
    }

    #[tokio::test]
    async fn test_pending_flow_lifecycle() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        let flow = OAuthPendingFlow {
            connector_id: "github".into(),
            state: "random_state_abc123".into(),
            code_verifier: Some("verifier_xyz".into()),
            redirect_uri: "http://localhost:8080/auth/github/callback".into(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(10),
        };

        store.store_pending_flow(&flow).await.unwrap();

        let completed = store
            .complete_pending_flow("random_state_abc123")
            .await
            .unwrap();
        assert_eq!(completed.connector_id, "github");
        assert_eq!(completed.code_verifier.as_deref(), Some("verifier_xyz"));
        assert_eq!(
            completed.redirect_uri,
            "http://localhost:8080/auth/github/callback"
        );

        // Single-use: second completion should fail
        assert!(store
            .complete_pending_flow("random_state_abc123")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_pending_flow_expired() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        let flow = OAuthPendingFlow {
            connector_id: "github".into(),
            state: "expired_state".into(),
            code_verifier: None,
            redirect_uri: "http://localhost/callback".into(),
            created_at: Utc::now() - chrono::Duration::minutes(20),
            expires_at: Utc::now() - chrono::Duration::minutes(10),
        };

        store.store_pending_flow(&flow).await.unwrap();
        assert!(store.complete_pending_flow("expired_state").await.is_err());
    }

    #[tokio::test]
    async fn test_sweep_expired_flows() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        // Insert an expired flow
        let flow = OAuthPendingFlow {
            connector_id: "github".into(),
            state: "sweep_me".into(),
            code_verifier: None,
            redirect_uri: "http://localhost/callback".into(),
            created_at: Utc::now() - chrono::Duration::minutes(20),
            expires_at: Utc::now() - chrono::Duration::minutes(5),
        };
        store.store_pending_flow(&flow).await.unwrap();

        // Insert a non-expired flow
        let flow2 = OAuthPendingFlow {
            connector_id: "slack".into(),
            state: "keep_me".into(),
            code_verifier: None,
            redirect_uri: "http://localhost/callback".into(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(10),
        };
        store.store_pending_flow(&flow2).await.unwrap();

        let swept = store.sweep_expired_flows().await.unwrap();
        assert_eq!(swept, 1);

        // The non-expired flow should still be completable
        assert!(store.complete_pending_flow("keep_me").await.is_ok());
    }

    #[tokio::test]
    async fn test_expiring_within() {
        let (conn, key, audit, _tmp) = setup_db();
        let store = OAuthStore::new(conn, key, audit);

        // Credential expiring in 3 minutes
        let mut cred1 = make_credential("github");
        cred1.expires_at = Some(Utc::now() + chrono::Duration::minutes(3));
        store
            .store(&cred1, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        // Credential expiring in 2 hours
        let mut cred2 = make_credential("slack");
        cred2.expires_at = Some(Utc::now() + chrono::Duration::hours(2));
        store
            .store(&cred2, SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        // Only github should expire within 5 minutes
        let expiring = store
            .expiring_within(chrono::Duration::minutes(5))
            .await
            .unwrap();
        assert_eq!(expiring.len(), 1);
        assert_eq!(expiring[0].connector_id, "github");
    }
}
