use agentos_types::AgentOSError;
use argon2::Argon2;
use rand::RngCore;
use zeroize::ZeroizeOnDrop;

/// The master key — zeroed from memory when dropped.
#[derive(ZeroizeOnDrop)]
pub struct MasterKey {
    pub(crate) key_bytes: [u8; 32],
}

impl MasterKey {
    /// Derive a 256-bit key from passphrase using Argon2id.
    pub fn derive(passphrase: &str, salt: &[u8; 32]) -> Result<MasterKey, AgentOSError> {
        let argon2 = Argon2::default(); // Argon2id with recommended params
        let mut key_bytes = [0u8; 32];

        argon2
            .hash_password_into(passphrase.as_bytes(), salt, &mut key_bytes)
            .map_err(|e| AgentOSError::VaultError(format!("Key derivation failed: {}", e)))?;

        Ok(MasterKey { key_bytes })
    }

    /// Generate a random 32-byte salt for Argon2.
    pub fn generate_salt() -> [u8; 32] {
        let mut salt = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut salt);
        salt
    }
}

/// A string that auto-zeroes its contents on drop.
#[derive(ZeroizeOnDrop)]
pub struct ZeroizingString {
    inner: String,
}

impl ZeroizingString {
    pub fn new(s: String) -> Self {
        Self { inner: s }
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }
}
