use crate::crypto::{decrypt, encrypt};
use crate::master_key::{MasterKey, ZeroizingString};
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_types::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct SecretsVault {
    conn: Mutex<Connection>,
    master_key: MasterKey,
    audit: Arc<AuditLog>,
}

const VAULT_SENTINEL: &str = "AGENTOS_VAULT_OK";

impl SecretsVault {
    pub fn initialize(path: &Path, passphrase: &str, audit: Arc<AuditLog>) -> Result<Self, AgentOSError> {
        if Self::is_initialized(path) {
            return Err(AgentOSError::VaultError("Vault is already initialized".to_string()));
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
            .map_err(|_| AgentOSError::VaultError("Vault not initialized or corrupt salt".to_string()))?;

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

        let owner_json = serde_json::to_string(&owner)
            .map_err(|e| AgentOSError::Serialization(format!("Failed to serialize owner: {}", e)))?;

        let scope_json = serde_json::to_string(&scope)
            .map_err(|e| AgentOSError::Serialization(format!("Failed to serialize scope: {}", e)))?;

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
        });

        let decrypted = decrypt(&self.master_key, &encrypted_value)?;
        let value_string = String::from_utf8(decrypted)
            .map_err(|_| AgentOSError::VaultError("Decrypted secret was not valid UTF-8".to_string()))?;

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
                let scope: SecretScope = serde_json::from_str(&scope_json).unwrap_or(SecretScope::Global);

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
        });

        Ok(())
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

        vault.set("OPENAI_KEY", "sk-test-12345", SecretOwner::Kernel, SecretScope::Global).unwrap();

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
        vault.set("KEY1", "secret1", SecretOwner::Kernel, SecretScope::Global).unwrap();
        vault.set("KEY2", "secret2", SecretOwner::Kernel, SecretScope::Global).unwrap();

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
        vault.set("KEY1", "secret1", SecretOwner::Kernel, SecretScope::Global).unwrap();
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
        vault.set("KEY1", "old-value", SecretOwner::Kernel, SecretScope::Global).unwrap();
        vault.rotate("KEY1", "new-value").unwrap();

        let retrieved = vault.get("KEY1").unwrap();
        assert_eq!(retrieved.as_str(), "new-value");
    }
}
