//! API key store — create, validate, revoke, and list API keys.
//!
//! Keys have the format `agos_<64-hex-chars>` and are validated via HMAC-SHA256
//! against a server-side secret. The store is backed by an in-memory `HashMap`
//! for fast reads, with write-through persistence to a SQLite database so keys
//! survive kernel restarts.
//!
//! The HMAC signing secret is stored in the `meta` table of the same database
//! and loaded on startup, so HMAC verification remains consistent across restarts.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rand::Rng;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;

/// Prefix for all AgentOS API keys.
const KEY_PREFIX: &str = "agos_";

/// Metadata about an issued API key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Public, non-secret identifier for this key (random 128-bit hex).
    ///
    /// Used to reference the key from management APIs (`DELETE /keys/{id}`).
    /// Distinct from the internal `lookup` (which is derived from the raw key
    /// material and must never be exposed).
    pub id: String,
    /// Display name for the key (e.g. "CI pipeline", "dev laptop").
    pub name: String,
    /// HMAC digest of the full key string. Never exposed via the API.
    #[serde(skip_serializing)]
    pub key_hash: Vec<u8>,
    /// Permission scopes granted to this key.
    pub permissions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

/// A freshly issued API key — the raw secret (shown to the caller exactly once)
/// plus its public identifier and metadata.
pub struct IssuedKey {
    /// The full `agos_<hex>` secret. Returned once and never stored in plaintext.
    pub api_key: String,
    /// The public, non-secret key id (see [`ApiKeyRecord::id`]).
    pub key_id: String,
    /// The persisted record (key material excluded from serialization).
    pub record: ApiKeyRecord,
}

/// Thread-safe API key store backed by an in-memory `HashMap` with optional
/// write-through SQLite persistence.
#[derive(Clone)]
pub struct ApiKeyStore {
    inner: Arc<RwLock<StoreInner>>,
    /// `Some` when the store was opened with `ApiKeyStore::open()` and
    /// mutations are persisted to disk. `None` for pure in-memory stores
    /// (used in tests and as the `Default` implementation).
    db: Option<Arc<Mutex<Connection>>>,
}

struct StoreInner {
    /// HMAC signing secret (server-side, never exposed). Zeroed on drop.
    secret: Zeroizing<Vec<u8>>,
    /// Map from key lookup prefix (first 16 hex chars after `agos_`) to record.
    keys: HashMap<String, ApiKeyRecord>,
}

impl Default for ApiKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiKeyStore {
    /// Create a new **in-memory** store with a random 32-byte HMAC secret.
    ///
    /// Keys created in this store are lost on process exit. For persistent
    /// storage across restarts, use [`ApiKeyStore::open`].
    pub fn new() -> Self {
        let mut secret = Zeroizing::new(vec![0u8; 32]);
        rand::thread_rng().fill(&mut secret[..]);
        Self {
            inner: Arc::new(RwLock::new(StoreInner {
                secret,
                keys: HashMap::new(),
            })),
            db: None,
        }
    }

    /// Create a new in-memory store with a caller-supplied secret (useful for tests).
    pub fn with_secret(secret: Vec<u8>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(StoreInner {
                secret: Zeroizing::new(secret),
                keys: HashMap::new(),
            })),
            db: None,
        }
    }

    /// Open (or create) a SQLite-backed key store at `path`.
    ///
    /// On first open the database is initialised and a random HMAC secret is
    /// generated and stored. On subsequent opens the existing secret and all
    /// active (non-revoked) keys are loaded into memory.
    pub async fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let path = path.to_path_buf();
        // Use spawn_blocking — rusqlite is synchronous and Connection is Send.
        tokio::task::spawn_blocking(move || {
            // Ensure the parent directory exists.
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        rusqlite::Error::SqliteFailure(
                            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                            Some(format!("cannot create parent dir: {e}")),
                        )
                    })?;
                }
            }

            let conn = Connection::open(&path)?;
            Self::init_schema(&conn)?;

            // Load or generate the HMAC signing secret.
            let secret = match Self::load_secret(&conn)? {
                Some(s) => s,
                None => {
                    let mut s = vec![0u8; 32];
                    rand::thread_rng().fill(&mut s[..]);
                    Self::store_secret(&conn, &s)?;
                    s
                }
            };

            // Load all active keys into the in-memory map.
            let keys = Self::load_keys(&conn)?;

            Ok(ApiKeyStore {
                inner: Arc::new(RwLock::new(StoreInner {
                    secret: Zeroizing::new(secret),
                    keys,
                })),
                db: Some(Arc::new(Mutex::new(conn))),
            })
        })
        .await
        .expect("spawn_blocking task panicked")
    }

    /// Issue a new API key. Returns the full key string that must be given to
    /// the caller — it is **not** stored in plaintext after this call.
    ///
    /// Back-compat thin wrapper over [`ApiKeyStore::issue`]; callers that need
    /// the public key id should use `issue` directly.
    pub async fn create_key(
        &self,
        name: String,
        permissions: Vec<String>,
        expires_at: Option<DateTime<Utc>>,
    ) -> String {
        self.issue(name, permissions, expires_at).await.api_key
    }

    /// Issue a new API key, returning the raw secret (shown once), its public
    /// id, and the persisted record. The raw key is **not** stored in plaintext.
    pub async fn issue(
        &self,
        name: String,
        permissions: Vec<String>,
        expires_at: Option<DateTime<Utc>>,
    ) -> IssuedKey {
        // Generate 32 random bytes → 64 hex chars of key material.
        let mut raw = [0u8; 32];
        rand::thread_rng().fill(&mut raw);
        let hex_part = hex::encode(raw);
        let full_key = format!("{KEY_PREFIX}{hex_part}");

        // Lookup prefix: first 16 hex chars (derived from secret — internal only).
        let lookup = hex_part[..16].to_string();

        // Public id: an independent random 128-bit value (NOT derived from the
        // key material, so it leaks nothing about the secret).
        let mut id_bytes = [0u8; 16];
        rand::thread_rng().fill(&mut id_bytes);
        let key_id = hex::encode(id_bytes);

        let (hash, record) = {
            let mut inner = self.inner.write().await;
            let hash = Self::hmac_key(&inner.secret, &full_key);
            let record = ApiKeyRecord {
                id: key_id.clone(),
                name: name.clone(),
                key_hash: hash.clone(),
                permissions: permissions.clone(),
                created_at: Utc::now(),
                last_used_at: None,
                expires_at,
                revoked: false,
            };
            inner.keys.insert(lookup.clone(), record.clone());
            (hash, record)
        };

        // Persist to DB outside the in-memory lock.
        if let Some(ref db_arc) = self.db {
            let db = Arc::clone(db_arc);
            let lookup_db = lookup;
            let id_db = key_id.clone();
            let name_db = name;
            let permissions_db = permissions;
            let created_at = record.created_at;
            let expires_at = record.expires_at;
            tokio::task::spawn_blocking(move || {
                match db.lock() {
                    Ok(conn) => {
                        if let Err(e) = conn.execute(
                            "INSERT OR REPLACE INTO api_keys \
                             (lookup, id, name, key_hash, permissions, created_at, expires_at, revoked) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                            params![
                                lookup_db,
                                id_db,
                                name_db,
                                hash,
                                serde_json::to_string(&permissions_db).unwrap_or_default(),
                                created_at.to_rfc3339(),
                                expires_at.map(|d| d.to_rfc3339()),
                            ],
                        ) {
                            tracing::error!("api_key issue DB write: {e}");
                        }
                    }
                    Err(e) => tracing::error!("api_key DB lock poisoned: {e}"),
                }
            });
        }

        IssuedKey {
            api_key: full_key,
            key_id,
            record,
        }
    }

    /// Validate an API key. Returns the record if valid.
    pub async fn validate(&self, key: &str) -> Option<ApiKeyRecord> {
        if !key.starts_with(KEY_PREFIX) {
            return None;
        }
        let hex_part = &key[KEY_PREFIX.len()..];
        if hex_part.len() < 16 {
            return None;
        }
        let lookup = &hex_part[..16];

        let mut inner = self.inner.write().await;

        // Clone the secret before borrowing keys mutably.
        let secret = inner.secret.clone();

        let record = inner.keys.get_mut(lookup)?;

        if record.revoked {
            return None;
        }

        // Check expiry.
        if let Some(exp) = record.expires_at {
            if Utc::now() > exp {
                return None;
            }
        }

        // Verify HMAC (constant-time comparison via `subtle` crate).
        let expected = Self::hmac_key(&secret, key);
        if !bool::from(expected.ct_eq(&record.key_hash)) {
            return None;
        }

        // Update last-used timestamp.
        let now = Utc::now();
        record.last_used_at = Some(now);
        let result = Some(record.clone());
        drop(inner);

        // Persist last_used_at update in the background (non-critical).
        if let Some(ref db_arc) = self.db {
            let db = Arc::clone(db_arc);
            let lookup_str = lookup.to_string();
            tokio::task::spawn_blocking(move || {
                if let Ok(conn) = db.lock() {
                    if let Err(e) = conn.execute(
                        "UPDATE api_keys SET last_used_at = ?1 WHERE lookup = ?2",
                        params![now.to_rfc3339(), lookup_str],
                    ) {
                        tracing::error!("api_key last_used_at DB update: {e}");
                    }
                }
            });
        }

        result
    }

    /// Revoke a key. Pass the full key string (`agos_<hex>`).
    pub async fn revoke(&self, key: &str) -> bool {
        if !key.starts_with(KEY_PREFIX) {
            return false;
        }
        let hex_part = &key[KEY_PREFIX.len()..];
        if hex_part.len() < 16 {
            return false;
        }
        let lookup = &hex_part[..16];

        let revoked = {
            let mut inner = self.inner.write().await;
            if let Some(record) = inner.keys.get_mut(lookup) {
                record.revoked = true;
                true
            } else {
                false
            }
        };

        if revoked {
            if let Some(ref db_arc) = self.db {
                let db = Arc::clone(db_arc);
                let lookup_str = lookup.to_string();
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = db.lock() {
                        if let Err(e) = conn.execute(
                            "UPDATE api_keys SET revoked = 1 WHERE lookup = ?1",
                            params![lookup_str],
                        ) {
                            tracing::error!("api_key revoke DB update: {e}");
                        }
                    }
                });
            }
        }

        revoked
    }

    /// Revoke all active keys with the given display name. Used at startup to
    /// replace stale bootstrap keys so each restart only ever has one active
    /// bootstrap key in the DB.
    pub async fn revoke_by_name(&self, name: &str) {
        let lookups: Vec<String> = {
            let mut inner = self.inner.write().await;
            let to_revoke: Vec<String> = inner
                .keys
                .iter()
                .filter(|(_, r)| !r.revoked && r.name == name)
                .map(|(k, _)| k.clone())
                .collect();
            for lookup in &to_revoke {
                if let Some(r) = inner.keys.get_mut(lookup) {
                    r.revoked = true;
                }
            }
            to_revoke
        };

        if !lookups.is_empty() {
            if let Some(ref db_arc) = self.db {
                let db = Arc::clone(db_arc);
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = db.lock() {
                        for lookup in &lookups {
                            if let Err(e) = conn.execute(
                                "UPDATE api_keys SET revoked = 1 WHERE lookup = ?1",
                                params![lookup],
                            ) {
                                tracing::error!("api_key revoke_by_name DB update: {e}");
                            }
                        }
                    }
                });
            }
        }
    }

    /// Revoke a key by its public id (see [`ApiKeyRecord::id`]). Returns true if
    /// a matching key was found and revoked.
    pub async fn revoke_by_id(&self, key_id: &str) -> bool {
        let revoked = {
            let mut inner = self.inner.write().await;
            match inner.keys.values_mut().find(|r| r.id == key_id) {
                Some(record) => {
                    record.revoked = true;
                    true
                }
                None => false,
            }
        };

        if revoked {
            if let Some(ref db_arc) = self.db {
                let db = Arc::clone(db_arc);
                let id_str = key_id.to_string();
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = db.lock() {
                        if let Err(e) = conn.execute(
                            "UPDATE api_keys SET revoked = 1 WHERE id = ?1",
                            params![id_str],
                        ) {
                            tracing::error!("api_key revoke_by_id DB update: {e}");
                        }
                    }
                });
            }
        }

        revoked
    }

    /// Look up a single key's metadata by its public id (key material excluded).
    /// Returns revoked keys too, so callers can report status.
    pub async fn get_by_id(&self, key_id: &str) -> Option<crate::types::ApiKeyMeta> {
        let inner = self.inner.read().await;
        inner
            .keys
            .values()
            .find(|r| r.id == key_id)
            .map(Self::meta_of)
    }

    /// List all (non-revoked) keys with their metadata (key material excluded).
    pub async fn list(&self) -> Vec<crate::types::ApiKeyInfo> {
        let inner = self.inner.read().await;
        inner
            .keys
            .values()
            .filter(|r| !r.revoked)
            .map(|r| crate::types::ApiKeyInfo {
                name: r.name.clone(),
                permissions: r.permissions.clone(),
                created_at: r.created_at,
                last_used_at: r.last_used_at,
                expires_at: r.expires_at,
            })
            .collect()
    }

    /// List all keys with full management metadata, including the public id and
    /// revoked status. Used by the key-management API.
    pub async fn list_all(&self) -> Vec<crate::types::ApiKeyMeta> {
        let inner = self.inner.read().await;
        inner.keys.values().map(Self::meta_of).collect()
    }

    fn meta_of(r: &ApiKeyRecord) -> crate::types::ApiKeyMeta {
        crate::types::ApiKeyMeta {
            key_id: r.id.clone(),
            name: r.name.clone(),
            scopes: r.permissions.clone(),
            created_at: r.created_at,
            last_used_at: r.last_used_at,
            expires_at: r.expires_at,
            revoked: r.revoked,
        }
    }

    // ── Schema helpers ─────────────────────────────────────────────────────

    fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS api_keys (
                lookup       TEXT PRIMARY KEY,
                id           TEXT,
                name         TEXT NOT NULL,
                key_hash     BLOB NOT NULL,
                permissions  TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                last_used_at TEXT,
                expires_at   TEXT,
                revoked      INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value BLOB NOT NULL
            );",
        )?;
        Self::migrate_schema(conn)
    }

    /// Add columns introduced after the original schema to pre-existing databases.
    /// `CREATE TABLE IF NOT EXISTS` does not alter an existing table, so new
    /// columns must be added explicitly and idempotently.
    fn migrate_schema(conn: &Connection) -> rusqlite::Result<()> {
        let has_id = conn
            .prepare("SELECT 1 FROM pragma_table_info('api_keys') WHERE name = 'id'")?
            .exists([])?;
        if !has_id {
            conn.execute("ALTER TABLE api_keys ADD COLUMN id TEXT", [])?;
        }
        Ok(())
    }

    fn load_secret(conn: &Connection) -> rusqlite::Result<Option<Vec<u8>>> {
        match conn.query_row(
            "SELECT value FROM meta WHERE key = 'hmac_secret'",
            [],
            |row| row.get(0),
        ) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn store_secret(conn: &Connection, secret: &[u8]) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('hmac_secret', ?1)",
            params![secret],
        )?;
        Ok(())
    }

    fn load_keys(conn: &Connection) -> rusqlite::Result<HashMap<String, ApiKeyRecord>> {
        let mut stmt = conn.prepare(
            "SELECT lookup, id, name, key_hash, permissions, created_at, last_used_at, expires_at \
             FROM api_keys WHERE revoked = 0",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;

        let mut keys = HashMap::new();
        for row in rows {
            let (lookup, id_opt, name, key_hash, perms_json, created_str, used_str, exp_str) = row?;
            // Legacy rows created before the `id` column may be NULL — synthesize
            // a stable-enough public id so management APIs can still address them.
            let id = id_opt.unwrap_or_else(|| {
                let mut b = [0u8; 16];
                rand::thread_rng().fill(&mut b);
                hex::encode(b)
            });
            let permissions: Vec<String> = serde_json::from_str(&perms_json).unwrap_or_default();
            let created_at = DateTime::parse_from_rfc3339(&created_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let last_used_at = used_str.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            });
            let expires_at = exp_str.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            });
            keys.insert(
                lookup,
                ApiKeyRecord {
                    id,
                    name,
                    key_hash,
                    permissions,
                    created_at,
                    last_used_at,
                    expires_at,
                    revoked: false,
                },
            );
        }
        Ok(keys)
    }

    fn hmac_key(secret: &[u8], key: &str) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
        mac.update(key.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_validate_key() {
        let store = ApiKeyStore::with_secret(vec![42u8; 32]);
        let key = store
            .create_key("test".into(), vec!["read".into()], None)
            .await;

        assert!(key.starts_with("agos_"));
        assert_eq!(key.len(), 5 + 64); // "agos_" + 64 hex chars

        let record = store.validate(&key).await;
        assert!(record.is_some());
        assert_eq!(record.as_ref().map(|r| r.name.as_str()), Some("test"));
    }

    #[tokio::test]
    async fn invalid_key_rejected() {
        let store = ApiKeyStore::with_secret(vec![42u8; 32]);
        let _ = store.create_key("test".into(), vec![], None).await;

        // Wrong key entirely.
        assert!(store
            .validate("agos_0000000000000000deadbeef0000000000000000000000000000000000000000")
            .await
            .is_none());
        // Missing prefix.
        assert!(store.validate("bad_key").await.is_none());
    }

    #[tokio::test]
    async fn revoked_key_rejected() {
        let store = ApiKeyStore::with_secret(vec![42u8; 32]);
        let key = store.create_key("revoke-me".into(), vec![], None).await;

        assert!(store.validate(&key).await.is_some());
        assert!(store.revoke(&key).await);
        assert!(store.validate(&key).await.is_none());
    }

    #[tokio::test]
    async fn list_excludes_revoked() {
        let store = ApiKeyStore::with_secret(vec![42u8; 32]);
        let k1 = store.create_key("a".into(), vec![], None).await;
        let _k2 = store.create_key("b".into(), vec![], None).await;

        assert_eq!(store.list().await.len(), 2);
        store.revoke(&k1).await;
        assert_eq!(store.list().await.len(), 1);
    }

    #[tokio::test]
    async fn issue_public_id_is_not_key_material() {
        let store = ApiKeyStore::with_secret(vec![7u8; 32]);
        let issued = store.issue("k".into(), vec!["agents:r".into()], None).await;

        // The public id must be an independent 128-bit value, NOT the lookup
        // prefix derived from the raw key (which would leak key material).
        let hex = issued.api_key.strip_prefix("agos_").unwrap();
        assert_eq!(
            issued.key_id.len(),
            32,
            "128-bit id rendered as 32 hex chars"
        );
        assert_ne!(issued.key_id, hex[..16], "id must not be the lookup prefix");
        assert!(
            !issued.api_key.contains(&issued.key_id),
            "id must not appear inside the raw key"
        );
        // The issued key still validates.
        assert!(store.validate(&issued.api_key).await.is_some());
    }

    #[tokio::test]
    async fn revoke_by_id_revokes_and_status_visible() {
        let store = ApiKeyStore::with_secret(vec![7u8; 32]);
        let a = store.issue("a".into(), vec![], None).await;
        let b = store.issue("b".into(), vec![], None).await;

        assert!(store.validate(&a.api_key).await.is_some());
        assert!(store.revoke_by_id(&a.key_id).await);
        // Revoked key no longer authenticates.
        assert!(store.validate(&a.api_key).await.is_none());
        // Unknown id is a no-op.
        assert!(!store.revoke_by_id("deadbeef").await);

        let all = store.list_all().await;
        assert!(all.iter().find(|m| m.key_id == a.key_id).unwrap().revoked);
        assert!(!all.iter().find(|m| m.key_id == b.key_id).unwrap().revoked);
        assert_eq!(store.get_by_id(&b.key_id).await.unwrap().name, "b");
    }

    #[tokio::test]
    async fn list_all_excludes_key_material() {
        let store = ApiKeyStore::with_secret(vec![7u8; 32]);
        let issued = store.issue("k".into(), vec!["x:r".into()], None).await;
        let json = serde_json::to_string(&store.list_all().await).unwrap();
        assert!(
            !json.contains(&issued.api_key),
            "list metadata must never contain raw key material"
        );
    }

    #[tokio::test]
    async fn open_persists_keys_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("api_keys.db");

        // Open store, create a key, then drop the store.
        let key = {
            let store = ApiKeyStore::open(&db_path).await.unwrap();
            store
                .create_key("persistent".into(), vec!["read".into()], None)
                .await
        };

        // Re-open the store from the same DB file — the key must still validate.
        let store2 = ApiKeyStore::open(&db_path).await.unwrap();
        let record = store2.validate(&key).await;
        assert!(record.is_some(), "key should survive store re-open");
        assert_eq!(record.unwrap().name, "persistent");
    }
}
