# Plan 04 — Secrets Vault (`agentos-vault` crate)

## Goal

Implement an encrypted secrets store using AES-256-GCM encryption with a master key derived from a user-supplied passphrase via Argon2id. Secrets are stored in SQLite (using rusqlite). The vault is kernel-managed — no tool, agent, or CLI command ever sees a raw credential value after storage.

## Dependencies

- `agentos-types`
- `agentos-audit`
- `rusqlite` (with `bundled` feature)
- `aes-gcm` — AES-256-GCM authenticated encryption
- `argon2` — password-based key derivation (Argon2id)
- `rand` — cryptographic RNG for nonces and salts
- `zeroize` — zero-out secrets from memory after use
- `serde`, `serde_json`

## Architecture

```
User enters passphrase (interactive, hidden input)
    │
    ▼
Argon2id KDF derives 256-bit master key from passphrase + random salt
    │
    ▼
Master key exists only in memory (ZeroizingKey — zeroed on drop)
    │
    ├── Encrypt secret value with AES-256-GCM (random 96-bit nonce per entry)
    │   Store: salt || nonce || ciphertext || tag  → SQLite blob column
    │
    └── Decrypt on kernel request only
        Key is zeroed from memory immediately after decryption completes
```

## Database Schema

```sql
CREATE TABLE IF NOT EXISTS secrets (
    id          TEXT PRIMARY KEY,         -- SecretID UUID
    name        TEXT NOT NULL UNIQUE,     -- Human-readable name (e.g. "OPENAI_API_KEY")
    owner       TEXT NOT NULL,            -- JSON: SecretOwner
    scope       TEXT NOT NULL,            -- JSON: SecretScope
    encrypted_value BLOB NOT NULL,        -- nonce(12) || ciphertext || tag(16)
    created_at  TEXT NOT NULL,
    last_used_at TEXT
);

CREATE TABLE IF NOT EXISTS vault_meta (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
);
-- vault_meta stores: argon2_salt (32 bytes), initialized_at
```

## Core Structs

```rust
use aes_gcm::{Aes256Gcm, Key, Nonce};
use aes_gcm::aead::{Aead, KeyInit};
use argon2::{Argon2, PasswordHasher};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// The master key — zeroed from memory when dropped.
#[derive(ZeroizeOnDrop)]
struct MasterKey {
    key_bytes: [u8; 32],
}

/// The vault itself — opened with a passphrase, holds the master key in memory.
pub struct SecretsVault {
    conn: Mutex<Connection>,
    master_key: MasterKey,
    audit: Arc<AuditLog>,
}
```

## Public API

```rust
impl SecretsVault {
    /// Initialize a NEW vault with a passphrase. Creates the SQLite DB,
    /// generates a random Argon2 salt, and derives the master key.
    /// Called once on first run or when vault DB doesn't exist.
    pub fn initialize(path: &Path, passphrase: &str, audit: Arc<AuditLog>) -> Result<Self, AgentOSError>;

    /// Open an EXISTING vault with a passphrase.
    /// Reads the stored Argon2 salt, re-derives the master key.
    /// Returns error if passphrase is wrong (detected via a stored verification token).
    pub fn open(path: &Path, passphrase: &str, audit: Arc<AuditLog>) -> Result<Self, AgentOSError>;

    /// Store a secret. Encrypts with AES-256-GCM using a fresh random nonce.
    pub fn set(
        &self,
        name: &str,
        value: &str,       // the raw secret — never logged, never stored in plaintext
        owner: SecretOwner,
        scope: SecretScope,
    ) -> Result<SecretID, AgentOSError>;

    /// Retrieve a decrypted secret value. ONLY the kernel should call this.
    /// The returned String is zeroized when dropped.
    /// Logs a SecretAccessed audit event.
    pub fn get(&self, name: &str) -> Result<ZeroizingString, AgentOSError>;

    /// List all secrets (metadata only — never returns values).
    pub fn list(&self) -> Result<Vec<SecretMetadata>, AgentOSError>;

    /// Revoke (delete) a secret.
    pub fn revoke(&self, name: &str) -> Result<(), AgentOSError>;

    /// Rotate a secret — atomically replace the old value with a new one.
    pub fn rotate(&self, name: &str, new_value: &str) -> Result<(), AgentOSError>;

    /// Check if the vault has been initialized.
    pub fn is_initialized(path: &Path) -> bool;
}

/// A string that auto-zeroes its contents on drop.
#[derive(ZeroizeOnDrop)]
pub struct ZeroizingString {
    inner: String,
}

impl ZeroizingString {
    pub fn as_str(&self) -> &str {
        &self.inner
    }
}
```

## Encryption Implementation

```rust
/// Encrypt a secret value with AES-256-GCM.
/// Returns: nonce (12 bytes) || ciphertext || tag (16 bytes)
fn encrypt(master_key: &MasterKey, plaintext: &[u8]) -> Result<Vec<u8>, AgentOSError> {
    let key = Key::<Aes256Gcm>::from_slice(&master_key.key_bytes);
    let cipher = Aes256Gcm::new(key);

    // Generate a fresh random 96-bit nonce for every encryption
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher.encrypt(nonce, plaintext)
        .map_err(|e| AgentOSError::VaultError(format!("Encryption failed: {}", e)))?;

    // Prepend nonce to ciphertext
    let mut output = Vec::with_capacity(12 + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend(ciphertext);
    Ok(output)
}

/// Decrypt: first 12 bytes are nonce, remainder is ciphertext + tag.
fn decrypt(master_key: &MasterKey, encrypted: &[u8]) -> Result<Vec<u8>, AgentOSError> {
    if encrypted.len() < 12 {
        return Err(AgentOSError::VaultError("Encrypted data too short".into()));
    }

    let key = Key::<Aes256Gcm>::from_slice(&master_key.key_bytes);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&encrypted[..12]);
    let ciphertext = &encrypted[12..];

    cipher.decrypt(nonce, ciphertext)
        .map_err(|e| AgentOSError::VaultError(format!("Decryption failed (wrong passphrase?): {}", e)))
}
```

## Master Key Derivation

```rust
/// Derive a 256-bit key from passphrase using Argon2id.
fn derive_master_key(passphrase: &str, salt: &[u8; 32]) -> Result<MasterKey, AgentOSError> {
    let argon2 = Argon2::default(); // Argon2id with recommended params
    let mut key_bytes = [0u8; 32];

    argon2.hash_password_into(
        passphrase.as_bytes(),
        salt,
        &mut key_bytes,
    ).map_err(|e| AgentOSError::VaultError(format!("Key derivation failed: {}", e)))?;

    Ok(MasterKey { key_bytes })
}

/// Generate a random 32-byte salt for Argon2.
fn generate_salt() -> [u8; 32] {
    let mut salt = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}
```

## Passphrase Verification

To detect wrong passphrases without storing the passphrase itself:

1. On vault initialization, encrypt a known sentinel value ("AGENTOS_VAULT_OK") with the master key
2. Store the encrypted sentinel in `vault_meta` table
3. On `open()`, derive the key and try to decrypt the sentinel
4. If decryption succeeds and produces "AGENTOS_VAULT_OK", the passphrase is correct

## Security Invariants

1. **No plaintext secrets** ever stored on disk — only AES-256-GCM ciphertext
2. **Fresh nonce per encryption** — prevents nonce reuse attacks
3. **Master key zeroed on drop** — `ZeroizeOnDrop` derive macro
4. **Returned secrets zeroed on drop** — `ZeroizingString` wrapper
5. **Every access logged** — `SecretAccessed` audit event includes who requested it (kernel/agent/tool) but NOT the secret value
6. **No environment variable fallback** — there is no code path that reads secrets from env vars

## Tests

```rust
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
```

## Verification

```bash
cargo test -p agentos-vault
```
