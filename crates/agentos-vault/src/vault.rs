use crate::crypto::{decrypt, encrypt};
use crate::master_key::{MasterKey, ZeroizingString};
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_types::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Log an audit entry, emitting a tracing::error if the write fails.
fn do_audit_log(audit: &AuditLog, entry: AuditEntry) {
    if let Err(e) = audit.append(entry) {
        tracing::error!(error = %e, "Failed to write vault audit log entry");
    }
}

/// An in-memory proxy token entry. Never written to disk.
struct ProxyTokenEntry {
    secret_name: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

pub struct SecretsVault {
    conn: Mutex<Connection>,
    master_key: MasterKey,
    audit: Arc<AuditLog>,
    /// In-memory proxy token store. Token handles are opaque to tools.
    proxy_tokens: Mutex<HashMap<String, ProxyTokenEntry>>,
    /// Emergency lockdown flag — when set, all proxy token operations are denied.
    locked_down: AtomicBool,
}

const VAULT_SENTINEL: &str = "AGENTOS_VAULT_OK";

impl SecretsVault {
    pub fn initialize(
        path: &Path,
        passphrase: &ZeroizingString,
        audit: Arc<AuditLog>,
    ) -> Result<Self, AgentOSError> {
        if Self::is_initialized(path) {
            return Err(AgentOSError::VaultError(
                "Vault is already initialized".to_string(),
            ));
        }

        let conn = Connection::open(path)
            .map_err(|e| AgentOSError::VaultError(format!("Vault DB open failed: {}", e)))?;

        // Restrict file permissions so only the owner can read/write the vault
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| AgentOSError::VaultError(format!("Failed to set vault permissions: {}", e)),
            )?;
        }

        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;

            CREATE TABLE IF NOT EXISTS secrets (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL UNIQUE,
                owner       TEXT NOT NULL,
                scope       TEXT NOT NULL,
                encrypted_value BLOB NOT NULL,
                created_at  TEXT NOT NULL,
                last_used_at TEXT
            );

            CREATE TABLE IF NOT EXISTS vault_meta (
                key   TEXT PRIMARY KEY,
                value BLOB NOT NULL
            );
            ",
        )
        .map_err(|e| AgentOSError::VaultError(format!("Vault schema init failed: {}", e)))?;

        let salt = MasterKey::generate_salt();
        let master_key = MasterKey::derive(passphrase, &salt)?;

        let encrypted_sentinel = encrypt(&master_key, VAULT_SENTINEL.as_bytes())?;

        conn.execute(
            "INSERT INTO vault_meta (key, value) VALUES ('argon2_salt', ?1)",
            params![salt.to_vec()],
        )
        .map_err(|e| AgentOSError::VaultError(format!("Failed to save salt: {}", e)))?;

        conn.execute(
            "INSERT INTO vault_meta (key, value) VALUES ('sentinel', ?1)",
            params![encrypted_sentinel],
        )
        .map_err(|e| AgentOSError::VaultError(format!("Failed to save sentinel: {}", e)))?;

        Ok(Self {
            conn: Mutex::new(conn),
            master_key,
            audit,
            proxy_tokens: Mutex::new(HashMap::new()),
            locked_down: AtomicBool::new(false),
        })
    }

    pub fn open(
        path: &Path,
        passphrase: &ZeroizingString,
        audit: Arc<AuditLog>,
    ) -> Result<Self, AgentOSError> {
        let conn = Connection::open(path)
            .map_err(|e| AgentOSError::VaultError(format!("Vault DB open failed: {}", e)))?;

        let salt_vec: Vec<u8> = conn
            .query_row(
                "SELECT value FROM vault_meta WHERE key = 'argon2_salt'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| {
                AgentOSError::VaultError("Vault not initialized or corrupt salt".to_string())
            })?;

        if salt_vec.len() != 32 {
            return Err(AgentOSError::VaultError(format!(
                "Corrupt salt: expected 32 bytes, got {}",
                salt_vec.len()
            )));
        }
        let mut salt = [0u8; 32];
        salt.copy_from_slice(&salt_vec);

        let master_key = MasterKey::derive(passphrase, &salt)?;

        let encrypted_sentinel: Vec<u8> = conn
            .query_row(
                "SELECT value FROM vault_meta WHERE key = 'sentinel'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| AgentOSError::VaultError("Vault sentinel missing".to_string()))?;

        // Attempt decryption of the sentinel to verify the passphrase
        let decrypted_sentinel = decrypt(&master_key, &encrypted_sentinel)
            .map_err(|_| AgentOSError::VaultError("Invalid passphrase".to_string()))?;

        if decrypted_sentinel != VAULT_SENTINEL.as_bytes() {
            return Err(AgentOSError::VaultError("Invalid passphrase".to_string()));
        }

        Ok(Self {
            conn: Mutex::new(conn),
            master_key,
            audit,
            proxy_tokens: Mutex::new(HashMap::new()),
            locked_down: AtomicBool::new(false),
        })
    }

    pub async fn set(
        &self,
        name: &str,
        value: &str,
        owner: SecretOwner,
        scope: SecretScope,
    ) -> Result<SecretID, AgentOSError> {
        let encrypted_value = encrypt(&self.master_key, value.as_bytes())?;

        let id = SecretID::new();
        let created_at = chrono::Utc::now().to_rfc3339();

        let owner_json = serde_json::to_string(&owner).map_err(|e| {
            AgentOSError::Serialization(format!("Failed to serialize owner: {}", e))
        })?;

        let scope_json = serde_json::to_string(&scope).map_err(|e| {
            AgentOSError::Serialization(format!("Failed to serialize scope: {}", e))
        })?;

        {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO secrets (id, name, owner, scope, encrypted_value, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(name) DO UPDATE SET
                    encrypted_value=excluded.encrypted_value,
                    owner=excluded.owner,
                    scope=excluded.scope",
                params![
                    id.to_string(),
                    name,
                    owner_json,
                    scope_json,
                    encrypted_value,
                    created_at
                ],
            )
            .map_err(|e| AgentOSError::VaultError(format!("Failed to insert secret: {}", e)))?;
        }

        do_audit_log(
            &self.audit,
            AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::SecretCreated,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "secret_name": name, "owner": owner }),
                severity: AuditSeverity::Security,
                reversible: false,
                rollback_ref: None,
            },
        );

        Ok(id)
    }

    pub async fn get(&self, name: &str) -> Result<ZeroizingString, AgentOSError> {
        let encrypted_value = {
            let conn = self.conn.lock().await;

            let encrypted_value: Vec<u8> = conn
                .query_row(
                    "SELECT encrypted_value FROM secrets WHERE name = ?1",
                    params![name],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| AgentOSError::VaultError(format!("DB error during get: {}", e)))?
                .ok_or_else(|| AgentOSError::SecretNotFound(name.to_string()))?;

            conn.execute(
                "UPDATE secrets SET last_used_at = ?1 WHERE name = ?2",
                params![chrono::Utc::now().to_rfc3339(), name],
            )
            .map_err(|e| AgentOSError::VaultError(format!("Failed to update last_used: {}", e)))?;

            encrypted_value
        };

        do_audit_log(
            &self.audit,
            AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::SecretAccessed,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "secret_name": name }),
                severity: AuditSeverity::Security,
                reversible: false,
                rollback_ref: None,
            },
        );

        let decrypted = decrypt(&self.master_key, &encrypted_value)?;
        let value_string = String::from_utf8(decrypted).map_err(|_| {
            AgentOSError::VaultError("Decrypted secret was not valid UTF-8".to_string())
        })?;

        Ok(ZeroizingString::new(value_string))
    }

    pub async fn list(&self) -> Result<Vec<SecretMetadata>, AgentOSError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT name, scope, created_at, last_used_at FROM secrets ORDER BY name ASC")
            .map_err(|e| AgentOSError::VaultError(format!("Failed to prepare list stmt: {}", e)))?;

        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let scope_json: String = row.get(1)?;

                // Corrupted scope JSON: treat as Kernel so it gets filtered out below,
                // preventing silent access widening to Global.
                let scope: SecretScope =
                    serde_json::from_str(&scope_json).unwrap_or(SecretScope::Kernel);

                let created_str: String = row.get(2)?;
                let created_at = chrono::DateTime::parse_from_rfc3339(&created_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_default();

                let last_used_str: Option<String> = row.get(3)?;
                let last_used_at = last_used_str.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                });

                Ok(SecretMetadata {
                    name,
                    scope,
                    created_at,
                    last_used_at,
                })
            })
            .map_err(|e| AgentOSError::VaultError(format!("Failed to map list rows: {}", e)))?;

        let mut results = Vec::new();
        for r in rows {
            let meta = r.map_err(|e| AgentOSError::VaultError(e.to_string()))?;
            // Kernel-scoped secrets are internal implementation details — never expose them
            // to the CLI or agents, as the name itself reveals the kernel's key structure.
            if !matches!(meta.scope, SecretScope::Kernel) {
                results.push(meta);
            }
        }

        Ok(results)
    }

    pub async fn revoke(&self, name: &str) -> Result<(), AgentOSError> {
        {
            let conn = self.conn.lock().await;

            // Fetch scope before deleting — kernel-scoped secrets are immutable from the CLI.
            let scope_json: Option<String> = conn
                .query_row(
                    "SELECT scope FROM secrets WHERE name = ?1",
                    params![name],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| AgentOSError::VaultError(format!("DB error during revoke: {}", e)))?;

            let scope_json =
                scope_json.ok_or_else(|| AgentOSError::SecretNotFound(name.to_string()))?;

            let scope: SecretScope = serde_json::from_str(&scope_json).map_err(|e| {
                AgentOSError::VaultError(format!("Corrupt scope for '{}': {}", name, e))
            })?;

            if matches!(scope, SecretScope::Kernel) {
                return Err(AgentOSError::VaultError(format!(
                    "Cannot revoke kernel-scoped secret '{}'",
                    name
                )));
            }

            conn.execute("DELETE FROM secrets WHERE name = ?1", params![name])
                .map_err(|e| AgentOSError::VaultError(format!("Failed to revoke secret: {}", e)))?;
        }

        do_audit_log(
            &self.audit,
            AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::SecretRevoked,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "secret_name": name }),
                severity: AuditSeverity::Security,
                reversible: false,
                rollback_ref: None,
            },
        );

        Ok(())
    }

    pub async fn rotate(&self, name: &str, new_value: &str) -> Result<(), AgentOSError> {
        let encrypted_value = encrypt(&self.master_key, new_value.as_bytes())?;

        {
            let conn = self.conn.lock().await;

            let (owner_json, scope_json): (String, String) = conn
                .query_row(
                    "SELECT owner, scope FROM secrets WHERE name = ?1",
                    params![name],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|e| AgentOSError::VaultError(format!("DB error during rotate: {}", e)))?
                .ok_or_else(|| AgentOSError::SecretNotFound(name.to_string()))?;

            // Kernel-scoped secrets cannot be rotated via the CLI / bus.
            let scope: SecretScope = serde_json::from_str(&scope_json).map_err(|e| {
                AgentOSError::VaultError(format!("Corrupt scope for '{}': {}", name, e))
            })?;
            if matches!(scope, SecretScope::Kernel) {
                return Err(AgentOSError::VaultError(format!(
                    "Cannot rotate kernel-scoped secret '{}'",
                    name
                )));
            }

            conn.execute(
                "INSERT INTO secrets (id, name, owner, scope, encrypted_value, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(name) DO UPDATE SET
                    encrypted_value=excluded.encrypted_value",
                params![
                    SecretID::new().to_string(),
                    name,
                    owner_json,
                    scope_json,
                    encrypted_value,
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .map_err(|e| AgentOSError::VaultError(format!("Failed to rotate secret: {}", e)))?;
        }

        do_audit_log(
            &self.audit,
            AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::SecretRotated,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "secret_name": name }),
                severity: AuditSeverity::Security,
                reversible: false,
                rollback_ref: None,
            },
        );

        Ok(())
    }

    /// Check whether the given `agent_id` is authorized to access a secret
    /// based on its `owner` and `scope` columns.
    ///
    /// Rules:
    /// - `SecretScope::Kernel` → only kernel code (no agent) may access; agents always denied.
    /// - `SecretScope::Global` → any agent may access.
    /// - `SecretScope::Agent(id)` → only `id` may access.
    /// - `SecretScope::Tool(id)` → the requesting agent must match the owner.
    async fn check_scope(&self, secret_name: &str, agent_id: AgentID) -> Result<(), AgentOSError> {
        let (owner_json, scope_json) = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT owner, scope FROM secrets WHERE name = ?1",
                params![secret_name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| AgentOSError::VaultError(format!("DB error during scope check: {}", e)))?
            .ok_or_else(|| AgentOSError::SecretNotFound(secret_name.to_string()))?
        };

        let scope: SecretScope = serde_json::from_str(&scope_json).map_err(|e| {
            AgentOSError::VaultError(format!("Corrupt scope for '{}': {}", secret_name, e))
        })?;
        let owner: SecretOwner = serde_json::from_str(&owner_json).map_err(|e| {
            AgentOSError::VaultError(format!("Corrupt owner for '{}': {}", secret_name, e))
        })?;

        match scope {
            SecretScope::Kernel => Err(AgentOSError::VaultError(format!(
                "Agent {} cannot access kernel-scoped secret '{}'",
                agent_id, secret_name
            ))),
            SecretScope::Global => Ok(()),
            SecretScope::Agent(scoped_id) => {
                if scoped_id == agent_id {
                    Ok(())
                } else {
                    Err(AgentOSError::VaultError(format!(
                        "Agent {} is not authorized to access secret '{}' (scoped to agent {})",
                        agent_id, secret_name, scoped_id
                    )))
                }
            }
            SecretScope::Tool(_) => match owner {
                SecretOwner::Agent(owner_id) if owner_id == agent_id => Ok(()),
                _ => Err(AgentOSError::VaultError(format!(
                    "Agent {} is not authorized to access tool-scoped secret '{}'",
                    agent_id, secret_name
                ))),
            },
        }
    }

    /// Issue a short-lived proxy token for a secret (Spec §3 zero-exposure architecture).
    pub async fn issue_proxy_token(
        &self,
        secret_name: &str,
        ttl_seconds: u64,
        agent_id: AgentID,
    ) -> Result<String, AgentOSError> {
        if self.locked_down.load(Ordering::SeqCst) {
            return Err(AgentOSError::VaultError(
                "Vault is in emergency lockdown — proxy token issuance denied".to_string(),
            ));
        }

        self.check_scope(secret_name, agent_id).await?;

        let token_id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let handle = format!("VAULT_PROXY:tok_{}", token_id);
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds as i64);

        self.proxy_tokens.lock().await.insert(
            handle.clone(),
            ProxyTokenEntry {
                secret_name: secret_name.to_string(),
                expires_at,
            },
        );

        do_audit_log(
            &self.audit,
            AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::SecretAccessed,
                agent_id: Some(agent_id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "secret_name": secret_name,
                    "action": "proxy_token_issued",
                    "ttl_seconds": ttl_seconds,
                }),
                severity: AuditSeverity::Security,
                reversible: false,
                rollback_ref: None,
            },
        );

        Ok(handle)
    }

    /// Resolve a proxy token to the underlying secret value.
    pub async fn resolve_proxy(&self, handle: &str) -> Result<ZeroizingString, AgentOSError> {
        if !handle.starts_with("VAULT_PROXY:") {
            return Err(AgentOSError::VaultError(
                "Not a vault proxy handle".to_string(),
            ));
        }

        let entry = {
            let mut tokens = self.proxy_tokens.lock().await;
            tokens.remove(handle).ok_or_else(|| {
                AgentOSError::VaultError(format!("Unknown or already-used proxy token: {}", handle))
            })?
        };

        if chrono::Utc::now() > entry.expires_at {
            return Err(AgentOSError::VaultError("Proxy token expired".to_string()));
        }

        self.get(&entry.secret_name).await
    }

    /// Remove all proxy tokens whose TTL has passed (call periodically).
    pub async fn sweep_expired_proxy_tokens(&self) {
        let now = chrono::Utc::now();
        self.proxy_tokens
            .lock()
            .await
            .retain(|_, entry| entry.expires_at > now);
    }

    /// Emergency lockdown: atomically revoke all active proxy tokens and
    /// prevent new ones from being issued until the vault is restarted.
    pub async fn lockdown(&self) {
        self.locked_down.store(true, Ordering::SeqCst);
        self.proxy_tokens.lock().await.clear();

        do_audit_log(
            &self.audit,
            AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::SecretRevoked,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "action": "emergency_lockdown",
                    "message": "All proxy tokens revoked, new issuance denied",
                }),
                severity: AuditSeverity::Security,
                reversible: false,
                rollback_ref: None,
            },
        );
    }

    /// Check if the vault is in emergency lockdown mode.
    pub fn is_locked_down(&self) -> bool {
        self.locked_down.load(Ordering::SeqCst)
    }

    pub fn is_initialized(path: &Path) -> bool {
        if !path.exists() {
            return false;
        }

        if let Ok(conn) = Connection::open(path) {
            let mut count: i32 = 0;
            if let Ok(c) = conn.query_row(
                "SELECT COUNT(*) FROM vault_meta WHERE key = 'sentinel'",
                [],
                |row| row.get(0),
            ) {
                count = c;
            }
            count > 0
        } else {
            false
        }
    }
}

/// Zero-exposure vault proxy: the only interface tools receive at execution time.
#[derive(Clone)]
pub struct ProxyVault {
    inner: Arc<SecretsVault>,
}

impl ProxyVault {
    pub fn new(vault: Arc<SecretsVault>) -> Self {
        Self { inner: vault }
    }

    pub async fn resolve(&self, handle: &str) -> Result<ZeroizingString, AgentOSError> {
        self.inner.resolve_proxy(handle).await
    }

    pub async fn get(
        &self,
        secret_name: &str,
        agent_id: AgentID,
    ) -> Result<ZeroizingString, AgentOSError> {
        let handle = self
            .inner
            .issue_proxy_token(secret_name, 5, agent_id)
            .await?;
        self.inner.resolve_proxy(&handle).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_vault(dir: &TempDir) -> SecretsVault {
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());
        let passphrase = ZeroizingString::new("test-passphrase-123".to_string());
        SecretsVault::initialize(&path, &passphrase, audit).unwrap()
    }

    #[tokio::test]
    async fn test_vault_initialize_and_set_get() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        vault
            .set(
                "OPENAI_KEY",
                "sk-test-12345",
                SecretOwner::Kernel,
                SecretScope::Global,
            )
            .await
            .unwrap();

        let retrieved = vault.get("OPENAI_KEY").await.unwrap();
        assert_eq!(retrieved.as_str(), "sk-test-12345");
    }

    #[test]
    fn test_vault_wrong_passphrase() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        let correct = ZeroizingString::new("correct-pass".to_string());
        SecretsVault::initialize(&path, &correct, audit.clone()).unwrap();

        let wrong = ZeroizingString::new("wrong-pass".to_string());
        let result = SecretsVault::open(&path, &wrong, audit);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_vault_list_never_exposes_values() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        vault
            .set("KEY1", "secret1", SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();
        vault
            .set("KEY2", "secret2", SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        let list = vault.list().await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn test_vault_revoke() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        vault
            .set("KEY1", "secret1", SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();
        vault.revoke("KEY1").await.unwrap();

        let result = vault.get("KEY1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_vault_rotate() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        vault
            .set(
                "KEY1",
                "old-value",
                SecretOwner::Kernel,
                SecretScope::Global,
            )
            .await
            .unwrap();
        vault.rotate("KEY1", "new-value").await.unwrap();

        let retrieved = vault.get("KEY1").await.unwrap();
        assert_eq!(retrieved.as_str(), "new-value");
    }

    #[tokio::test]
    async fn test_scope_enforcement_agent_cannot_access_other_agent_secret() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        let agent_a = agentos_types::AgentID::new();
        let agent_b = agentos_types::AgentID::new();

        vault
            .set(
                "AGENT_A_KEY",
                "secret-for-a",
                SecretOwner::Agent(agent_a),
                SecretScope::Agent(agent_a),
            )
            .await
            .unwrap();

        let result = vault.issue_proxy_token("AGENT_A_KEY", 60, agent_a).await;
        assert!(result.is_ok(), "Agent A should access its own secret");

        let result = vault.issue_proxy_token("AGENT_A_KEY", 60, agent_b).await;
        assert!(
            result.is_err(),
            "Agent B should NOT access Agent A's secret"
        );
        assert!(
            result.unwrap_err().to_string().contains("not authorized"),
            "Error should mention authorization"
        );
    }

    #[tokio::test]
    async fn test_scope_enforcement_global_secret_accessible_to_all() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        let agent_a = agentos_types::AgentID::new();
        let agent_b = agentos_types::AgentID::new();

        vault
            .set(
                "SHARED_KEY",
                "shared-secret",
                SecretOwner::Kernel,
                SecretScope::Global,
            )
            .await
            .unwrap();

        assert!(vault
            .issue_proxy_token("SHARED_KEY", 60, agent_a)
            .await
            .is_ok());
        assert!(vault
            .issue_proxy_token("SHARED_KEY", 60, agent_b)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_scope_enforcement_kernel_scoped_secret_blocks_agents() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        let agent = agentos_types::AgentID::new();

        vault
            .set(
                "__kernel_key",
                "kernel-only-value",
                SecretOwner::Kernel,
                SecretScope::Kernel,
            )
            .await
            .unwrap();

        let result = vault.issue_proxy_token("__kernel_key", 60, agent).await;
        assert!(
            result.is_err(),
            "Agent should not access kernel-scoped secret"
        );
        assert!(
            result.unwrap_err().to_string().contains("kernel-scoped"),
            "Error should mention kernel scope"
        );
    }

    #[tokio::test]
    async fn test_lockdown_prevents_proxy_tokens() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);
        let agent_id = agentos_types::AgentID::new();

        vault
            .set("KEY1", "value", SecretOwner::Kernel, SecretScope::Global)
            .await
            .unwrap();

        assert!(vault.issue_proxy_token("KEY1", 60, agent_id).await.is_ok());
        assert!(!vault.is_locked_down());

        vault.lockdown().await;
        assert!(vault.is_locked_down());

        let result = vault.issue_proxy_token("KEY1", 60, agent_id).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("lockdown"),
            "Error should mention lockdown"
        );
    }

    /// Kernel-scoped secrets must not be destroyable via revoke().
    #[tokio::test]
    async fn test_kernel_scoped_secret_cannot_be_revoked() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        vault
            .set(
                "__internal_hmac_key",
                "hmac-value",
                SecretOwner::Kernel,
                SecretScope::Kernel,
            )
            .await
            .unwrap();

        let result = vault.revoke("__internal_hmac_key").await;
        assert!(
            result.is_err(),
            "Kernel-scoped secret must not be revocable"
        );
        assert!(
            result.unwrap_err().to_string().contains("kernel-scoped"),
            "Error should mention kernel scope"
        );

        // Value must still be readable by kernel (via get())
        assert!(vault.get("__internal_hmac_key").await.is_ok());
    }

    /// Kernel-scoped secrets must not be replaceable via rotate().
    #[tokio::test]
    async fn test_kernel_scoped_secret_cannot_be_rotated() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        vault
            .set(
                "__internal_hmac_key",
                "original-value",
                SecretOwner::Kernel,
                SecretScope::Kernel,
            )
            .await
            .unwrap();

        let result = vault.rotate("__internal_hmac_key", "attacker-value").await;
        assert!(
            result.is_err(),
            "Kernel-scoped secret must not be rotatable"
        );
        assert!(
            result.unwrap_err().to_string().contains("kernel-scoped"),
            "Error should mention kernel scope"
        );

        // Confirm value is unchanged
        let val = vault.get("__internal_hmac_key").await.unwrap();
        assert_eq!(val.as_str(), "original-value");
    }

    /// rotate() on a non-existent secret must fail rather than silently create it.
    #[tokio::test]
    async fn test_rotate_nonexistent_secret_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        let result = vault.rotate("nonexistent-key", "some-value").await;
        assert!(result.is_err(), "rotate() on missing secret must fail");
        match result.unwrap_err() {
            AgentOSError::SecretNotFound(name) => assert_eq!(name, "nonexistent-key"),
            other => panic!("Expected SecretNotFound, got: {:?}", other),
        }
    }

    /// list() must never expose Kernel-scoped secrets.
    #[tokio::test]
    async fn test_list_hides_kernel_scoped_secrets() {
        let dir = TempDir::new().unwrap();
        let vault = make_vault(&dir);

        vault
            .set(
                "public-key",
                "value1",
                SecretOwner::Kernel,
                SecretScope::Global,
            )
            .await
            .unwrap();
        vault
            .set(
                "__internal_hmac_key",
                "hidden",
                SecretOwner::Kernel,
                SecretScope::Kernel,
            )
            .await
            .unwrap();

        let list = vault.list().await.unwrap();
        assert_eq!(
            list.len(),
            1,
            "Kernel-scoped secret must not appear in list()"
        );
        assert_eq!(list[0].name, "public-key");
        assert!(!list.iter().any(|m| m.name.contains("hmac")));
    }
}
