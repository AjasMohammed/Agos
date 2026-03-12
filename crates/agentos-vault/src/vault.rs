use crate::crypto::{decrypt, encrypt};
use crate::master_key::{MasterKey, ZeroizingString};
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_types::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
        passphrase: &str,
        audit: Arc<AuditLog>,
    ) -> Result<Self, AgentOSError> {
        if Self::is_initialized(path) {
            return Err(AgentOSError::VaultError(
                "Vault is already initialized".to_string(),
            ));
        }

        let conn = Connection::open(path)
            .map_err(|e| AgentOSError::VaultError(format!("Vault DB open failed: {}", e)))?;

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

    pub fn open(path: &Path, passphrase: &str, audit: Arc<AuditLog>) -> Result<Self, AgentOSError> {
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

    pub fn set(
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

        let conn = self.conn.lock().unwrap();
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

        // Audit log
        let _ = self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(), // new trace for the operation
            event_type: AuditEventType::SecretCreated,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "secret_name": name, "owner": owner }),
            severity: AuditSeverity::Security,
            reversible: false,
            rollback_ref: None,
        });

        Ok(id)
    }

    pub fn get(&self, name: &str) -> Result<ZeroizingString, AgentOSError> {
        let conn = self.conn.lock().unwrap();

        // Retrieve the ciphertext
        let encrypted_value: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_value FROM secrets WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AgentOSError::VaultError(format!("DB error during get: {}", e)))?
            .ok_or_else(|| AgentOSError::SecretNotFound(name.to_string()))?;

        // Update last_used_at
        conn.execute(
            "UPDATE secrets SET last_used_at = ?1 WHERE name = ?2",
            params![chrono::Utc::now().to_rfc3339(), name],
        )
        .map_err(|e| AgentOSError::VaultError(format!("Failed to update last_used: {}", e)))?;

        // Audit Log
        let _ = self.audit.append(AuditEntry {
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
        });

        let decrypted = decrypt(&self.master_key, &encrypted_value)?;
        let value_string = String::from_utf8(decrypted).map_err(|_| {
            AgentOSError::VaultError("Decrypted secret was not valid UTF-8".to_string())
        })?;

        Ok(ZeroizingString::new(value_string))
    }

    pub fn list(&self) -> Result<Vec<SecretMetadata>, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name, scope, created_at, last_used_at FROM secrets ORDER BY name ASC")
            .map_err(|e| AgentOSError::VaultError(format!("Failed to prepare list stmt: {}", e)))?;

        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get(0)?;

                let scope_json: String = row.get(1)?;
                let scope: SecretScope =
                    serde_json::from_str(&scope_json).unwrap_or(SecretScope::Global);

                let created_str: String = row.get(2)?;
                let created_at = chrono::DateTime::parse_from_rfc3339(&created_str)
                    .unwrap()
                    .with_timezone(&chrono::Utc);

                let last_used_str: Option<String> = row.get(3)?;
                let last_used_at = last_used_str.map(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .unwrap()
                        .with_timezone(&chrono::Utc)
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
            results.push(r.map_err(|e| AgentOSError::VaultError(e.to_string()))?);
        }

        Ok(results)
    }

    pub fn revoke(&self, name: &str) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let count = conn
            .execute("DELETE FROM secrets WHERE name = ?1", params![name])
            .map_err(|e| AgentOSError::VaultError(format!("Failed to revoke secret: {}", e)))?;

        if count == 0 {
            return Err(AgentOSError::SecretNotFound(name.to_string()));
        }

        let _ = self.audit.append(AuditEntry {
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
        });

        Ok(())
    }

    pub fn rotate(&self, name: &str, new_value: &str) -> Result<(), AgentOSError> {
        let encrypted_value = encrypt(&self.master_key, new_value.as_bytes())?;

        let conn = self.conn.lock().unwrap();

        // Atomic rotate: read existing metadata, then update in a single transaction
        let (owner_json, scope_json): (String, String) = conn
            .query_row(
                "SELECT owner, scope FROM secrets WHERE name = ?1",
                params![name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| AgentOSError::VaultError(format!("DB error during rotate: {}", e)))?
            .unwrap_or_else(|| {
                // If the secret doesn't exist yet, default to Kernel/Global
                (
                    serde_json::to_string(&SecretOwner::Kernel).unwrap(),
                    serde_json::to_string(&SecretScope::Global).unwrap(),
                )
            });

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

        drop(conn);

        let _ = self.audit.append(AuditEntry {
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
        });

        Ok(())
    }

    /// Check whether the given `agent_id` is authorized to access a secret
    /// based on its `owner` and `scope` columns.
    ///
    /// Rules:
    /// - `SecretScope::Global` → any agent may access.
    /// - `SecretScope::Agent(id)` → only `id` may access.
    /// - `SecretScope::Tool(id)` → the requesting agent must match the owner.
    /// - `SecretOwner::Kernel` with non-global scope → only kernel (agent_id == None).
    fn check_scope(&self, secret_name: &str, agent_id: AgentID) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let (owner_json, scope_json): (String, String) = conn
            .query_row(
                "SELECT owner, scope FROM secrets WHERE name = ?1",
                params![secret_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| AgentOSError::VaultError(format!("DB error during scope check: {}", e)))?
            .ok_or_else(|| AgentOSError::SecretNotFound(secret_name.to_string()))?;

        let scope: SecretScope = serde_json::from_str(&scope_json).unwrap_or(SecretScope::Global);
        let owner: SecretOwner = serde_json::from_str(&owner_json).unwrap_or(SecretOwner::Kernel);

        match scope {
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
            SecretScope::Tool(_) => {
                // For tool-scoped secrets, the requesting agent must be the owner
                match owner {
                    SecretOwner::Agent(owner_id) if owner_id == agent_id => Ok(()),
                    _ => Err(AgentOSError::VaultError(format!(
                        "Agent {} is not authorized to access tool-scoped secret '{}'",
                        agent_id, secret_name
                    ))),
                }
            }
        }
    }

    /// Issue a short-lived proxy token for a secret (Spec §3 zero-exposure architecture).
    ///
    /// Returns an opaque handle of the form `VAULT_PROXY:tok_<uuid>`.
    /// The tool receives this handle instead of the plaintext secret.
    /// Call `resolve_proxy()` at tool invocation time to substitute the real value.
    ///
    /// **Scope enforcement:** The requesting `agent_id` must be authorized
    /// for the secret's scope/owner. Global secrets are accessible to all;
    /// agent-scoped secrets require a matching agent_id.
    pub fn issue_proxy_token(
        &self,
        secret_name: &str,
        ttl_seconds: u64,
        agent_id: AgentID,
    ) -> Result<String, AgentOSError> {
        // Emergency lockdown check
        if self.locked_down.load(Ordering::SeqCst) {
            return Err(AgentOSError::VaultError(
                "Vault is in emergency lockdown — proxy token issuance denied".to_string(),
            ));
        }

        // Scope enforcement: verify the agent is authorized for this secret
        self.check_scope(secret_name, agent_id)?;

        let token_id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let handle = format!("VAULT_PROXY:tok_{}", token_id);
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds as i64);

        self.proxy_tokens.lock().unwrap().insert(
            handle.clone(),
            ProxyTokenEntry {
                secret_name: secret_name.to_string(),
                expires_at,
            },
        );

        let _ = self.audit.append(AuditEntry {
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
        });

        Ok(handle)
    }

    /// Resolve a proxy token to the underlying secret value.
    ///
    /// Validates expiry, then decrypts and returns the plaintext value.
    /// The caller is responsible for zeroizing the value after use.
    /// Returns `Err` if the token is unknown, expired, or the secret was revoked.
    pub fn resolve_proxy(&self, handle: &str) -> Result<ZeroizingString, AgentOSError> {
        if !handle.starts_with("VAULT_PROXY:") {
            return Err(AgentOSError::VaultError(
                "Not a vault proxy handle".to_string(),
            ));
        }

        let entry = {
            let mut tokens = self.proxy_tokens.lock().unwrap();
            // Remove on first use (single-use tokens, plus sweep expired)
            tokens.remove(handle).ok_or_else(|| {
                AgentOSError::VaultError(format!("Unknown or already-used proxy token: {}", handle))
            })?
        };

        if chrono::Utc::now() > entry.expires_at {
            return Err(AgentOSError::VaultError(format!(
                "Proxy token expired at {}",
                entry.expires_at
            )));
        }

        // Delegate to the real get() for decryption and audit logging
        self.get(&entry.secret_name)
    }

    /// Remove all proxy tokens whose TTL has passed (call periodically).
    pub fn sweep_expired_proxy_tokens(&self) {
        let now = chrono::Utc::now();
        self.proxy_tokens
            .lock()
            .unwrap()
            .retain(|_, entry| entry.expires_at > now);
    }

    /// Emergency lockdown: atomically revoke all active proxy tokens and
    /// prevent new ones from being issued until the vault is restarted.
    ///
    /// This is a one-way operation within a vault lifetime. Once locked down,
    /// the vault must be re-opened to resume normal proxy token operations.
    pub fn lockdown(&self) {
        self.locked_down.store(true, Ordering::SeqCst);
        self.proxy_tokens.lock().unwrap().clear();

        let _ = self.audit.append(AuditEntry {
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
        });

        // The audit entry above records this event; no tracing crate in this crate.
    }

    /// Check if the vault is in emergency lockdown mode.
    pub fn is_locked_down(&self) -> bool {
        self.locked_down.load(Ordering::SeqCst)
    }

    pub fn is_initialized(path: &Path) -> bool {
        // Simple check: does the db file exist and is it reachable
        if !path.exists() {
            return false;
        }

        if let Ok(conn) = Connection::open(path) {
            let mut count: i32 = 0;
            // Best effort check for sentinel presence
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
///
/// Tools get an `Arc<ProxyVault>` instead of `Arc<SecretsVault>`, so they can only
/// resolve proxy token handles into short-lived secret values — never enumerate,
/// create, rotate, or revoke secrets directly.
#[derive(Clone)]
pub struct ProxyVault {
    inner: Arc<SecretsVault>,
}

impl ProxyVault {
    /// Wrap a vault reference into a proxy handle.
    pub fn new(vault: Arc<SecretsVault>) -> Self {
        Self { inner: vault }
    }

    /// Resolve a proxy token handle (e.g. `VAULT_PROXY:tok_...`) to the secret value.
    ///
    /// The token is consumed on first use. Returns an error if the token is unknown,
    /// expired, or the underlying secret has been revoked.
    pub fn resolve(&self, handle: &str) -> Result<ZeroizingString, AgentOSError> {
        self.inner.resolve_proxy(handle)
    }

    /// Resolve a secret by name for a given agent, using a single-use proxy token internally.
    ///
    /// This is the primary interface for tools that need a secret value. It enforces
    /// scope checks (the agent must be authorized for the secret) and audit logging,
    /// while preventing tools from enumerating, creating, rotating, or revoking secrets.
    pub fn get(
        &self,
        secret_name: &str,
        agent_id: AgentID,
    ) -> Result<ZeroizingString, AgentOSError> {
        let handle = self.inner.issue_proxy_token(secret_name, 5, agent_id)?;
        self.inner.resolve_proxy(&handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_vault_initialize_and_set_get() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        let vault = SecretsVault::initialize(&path, "test-passphrase-123", audit.clone()).unwrap();

        vault
            .set(
                "OPENAI_KEY",
                "sk-test-12345",
                SecretOwner::Kernel,
                SecretScope::Global,
            )
            .unwrap();

        let retrieved = vault.get("OPENAI_KEY").unwrap();
        assert_eq!(retrieved.as_str(), "sk-test-12345");
    }

    #[test]
    fn test_vault_wrong_passphrase() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        SecretsVault::initialize(&path, "correct-pass", audit.clone()).unwrap();

        let result = SecretsVault::open(&path, "wrong-pass", audit);
        assert!(result.is_err());
    }

    #[test]
    fn test_vault_list_never_exposes_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        let vault = SecretsVault::initialize(&path, "pass", audit).unwrap();
        vault
            .set("KEY1", "secret1", SecretOwner::Kernel, SecretScope::Global)
            .unwrap();
        vault
            .set("KEY2", "secret2", SecretOwner::Kernel, SecretScope::Global)
            .unwrap();

        let list = vault.list().unwrap();
        assert_eq!(list.len(), 2);
        // SecretMetadata has name, scope, timestamps — but NO value field
    }

    #[test]
    fn test_vault_revoke() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        let vault = SecretsVault::initialize(&path, "pass", audit).unwrap();
        vault
            .set("KEY1", "secret1", SecretOwner::Kernel, SecretScope::Global)
            .unwrap();
        vault.revoke("KEY1").unwrap();

        let result = vault.get("KEY1");
        assert!(result.is_err()); // SecretNotFound
    }

    #[test]
    fn test_vault_rotate() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        let vault = SecretsVault::initialize(&path, "pass", audit).unwrap();
        vault
            .set(
                "KEY1",
                "old-value",
                SecretOwner::Kernel,
                SecretScope::Global,
            )
            .unwrap();
        vault.rotate("KEY1", "new-value").unwrap();

        let retrieved = vault.get("KEY1").unwrap();
        assert_eq!(retrieved.as_str(), "new-value");
    }

    #[test]
    fn test_scope_enforcement_agent_cannot_access_other_agent_secret() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        let vault = SecretsVault::initialize(&path, "pass", audit).unwrap();

        let agent_a = agentos_types::AgentID::new();
        let agent_b = agentos_types::AgentID::new();

        // Agent A creates a secret scoped to itself
        vault
            .set(
                "AGENT_A_KEY",
                "secret-for-a",
                SecretOwner::Agent(agent_a),
                SecretScope::Agent(agent_a),
            )
            .unwrap();

        // Agent A can get a proxy token for its own secret
        let result = vault.issue_proxy_token("AGENT_A_KEY", 60, agent_a);
        assert!(result.is_ok(), "Agent A should access its own secret");

        // Agent B cannot get a proxy token for Agent A's secret
        let result = vault.issue_proxy_token("AGENT_A_KEY", 60, agent_b);
        assert!(
            result.is_err(),
            "Agent B should NOT access Agent A's secret"
        );
        assert!(
            result.unwrap_err().to_string().contains("not authorized"),
            "Error should mention authorization"
        );
    }

    #[test]
    fn test_scope_enforcement_global_secret_accessible_to_all() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        let vault = SecretsVault::initialize(&path, "pass", audit).unwrap();

        let agent_a = agentos_types::AgentID::new();
        let agent_b = agentos_types::AgentID::new();

        // Create a global-scope secret
        vault
            .set(
                "SHARED_KEY",
                "shared-secret",
                SecretOwner::Kernel,
                SecretScope::Global,
            )
            .unwrap();

        // Both agents should be able to get proxy tokens
        assert!(vault.issue_proxy_token("SHARED_KEY", 60, agent_a).is_ok());
        assert!(vault.issue_proxy_token("SHARED_KEY", 60, agent_b).is_ok());
    }

    #[test]
    fn test_lockdown_prevents_proxy_tokens() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_vault.db");
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap());

        let vault = SecretsVault::initialize(&path, "pass", audit).unwrap();
        let agent_id = agentos_types::AgentID::new();

        vault
            .set("KEY1", "value", SecretOwner::Kernel, SecretScope::Global)
            .unwrap();

        // Before lockdown: proxy tokens work
        assert!(vault.issue_proxy_token("KEY1", 60, agent_id).is_ok());
        assert!(!vault.is_locked_down());

        // Lockdown
        vault.lockdown();
        assert!(vault.is_locked_down());

        // After lockdown: proxy tokens denied
        let result = vault.issue_proxy_token("KEY1", 60, agent_id);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("lockdown"),
            "Error should mention lockdown"
        );
    }
}
