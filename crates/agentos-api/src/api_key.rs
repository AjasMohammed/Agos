//! API key store — create, validate, revoke, and list API keys.
//!
//! Keys have the format `agos_<64-hex-chars>` and are validated via HMAC-SHA256
//! against a server-side secret. The store is protected by `Arc<RwLock<_>>` for
//! concurrent access.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

type HmacSha256 = Hmac<Sha256>;

/// Prefix for all AgentOS API keys.
const KEY_PREFIX: &str = "agos_";

/// Metadata about an issued API key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Display name for the key (e.g. "CI pipeline", "dev laptop").
    pub name: String,
    /// The full key string (`agos_<hex>`). Stored for revocation lookups.
    /// In production you would store only the HMAC digest, but for this
    /// in-memory store we keep the key for simplicity.
    #[serde(skip_serializing)]
    pub key_hash: Vec<u8>,
    /// Permission scopes granted to this key.
    pub permissions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

/// Thread-safe API key store backed by an in-memory `HashMap`.
#[derive(Clone)]
pub struct ApiKeyStore {
    inner: Arc<RwLock<StoreInner>>,
}

struct StoreInner {
    /// HMAC signing secret (server-side, never exposed).
    secret: Vec<u8>,
    /// Map from key prefix (first 16 hex chars after `agos_`) to record.
    keys: HashMap<String, ApiKeyRecord>,
}

impl Default for ApiKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiKeyStore {
    /// Create a new store with a random 32-byte HMAC secret.
    pub fn new() -> Self {
        let mut secret = vec![0u8; 32];
        rand::thread_rng().fill(&mut secret[..]);
        Self {
            inner: Arc::new(RwLock::new(StoreInner {
                secret,
                keys: HashMap::new(),
            })),
        }
    }

    /// Create a new store with a caller-supplied secret (useful for tests).
    pub fn with_secret(secret: Vec<u8>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(StoreInner {
                secret,
                keys: HashMap::new(),
            })),
        }
    }

    /// Issue a new API key. Returns the full key string that must be given to
    /// the caller — it is **not** stored in plaintext after this call in a
    /// production system, but for this in-memory store we keep the hash.
    pub async fn create_key(
        &self,
        name: String,
        permissions: Vec<String>,
        expires_at: Option<DateTime<Utc>>,
    ) -> String {
        let mut inner = self.inner.write().await;

        // Generate 32 random bytes → 64 hex chars.
        let mut raw = [0u8; 32];
        rand::thread_rng().fill(&mut raw);
        let hex_part = hex::encode(raw);
        let full_key = format!("{KEY_PREFIX}{hex_part}");

        // HMAC of the full key.
        let hash = Self::hmac_key(&inner.secret, &full_key);

        // Lookup prefix: first 16 hex chars.
        let lookup = hex_part[..16].to_string();

        inner.keys.insert(
            lookup,
            ApiKeyRecord {
                name,
                key_hash: hash,
                permissions,
                created_at: Utc::now(),
                last_used_at: None,
                expires_at,
                revoked: false,
            },
        );

        full_key
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
        if expected.ct_eq(&record.key_hash).into() {
            // match — continue
        } else {
            return None;
        }

        // Update last-used timestamp.
        record.last_used_at = Some(Utc::now());
        Some(record.clone())
    }

    /// Revoke a key by its lookup prefix (first 16 hex chars after `agos_`).
    pub async fn revoke(&self, key: &str) -> bool {
        if !key.starts_with(KEY_PREFIX) {
            return false;
        }
        let hex_part = &key[KEY_PREFIX.len()..];
        if hex_part.len() < 16 {
            return false;
        }
        let lookup = &hex_part[..16];

        let mut inner = self.inner.write().await;
        if let Some(record) = inner.keys.get_mut(lookup) {
            record.revoked = true;
            true
        } else {
            false
        }
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

    // ── Internals ──────────────────────────────────────────────────────────

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
}
