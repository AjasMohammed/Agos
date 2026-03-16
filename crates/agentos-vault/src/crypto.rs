use crate::master_key::MasterKey;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use agentos_types::AgentOSError;
use rand::RngCore;

/// Encrypt a secret value with AES-256-GCM.
/// Returns: nonce (12 bytes) || ciphertext || tag (16 bytes)
pub fn encrypt(master_key: &MasterKey, plaintext: &[u8]) -> Result<Vec<u8>, AgentOSError> {
    let key = Key::<Aes256Gcm>::from_slice(&master_key.key_bytes);
    let cipher = Aes256Gcm::new(key);

    // Generate a fresh random 96-bit nonce for every encryption
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| AgentOSError::VaultError(format!("Encryption failed: {}", e)))?;

    // Prepend nonce to ciphertext
    let mut output = Vec::with_capacity(12 + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend(ciphertext);
    Ok(output)
}

/// Decrypt: first 12 bytes are nonce, remainder is ciphertext + tag.
pub fn decrypt(master_key: &MasterKey, encrypted: &[u8]) -> Result<Vec<u8>, AgentOSError> {
    // AES-256-GCM: 12-byte nonce + 16-byte auth tag minimum
    if encrypted.len() < 28 {
        return Err(AgentOSError::VaultError("Encrypted data too short".into()));
    }

    let key = Key::<Aes256Gcm>::from_slice(&master_key.key_bytes);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&encrypted[..12]);
    let ciphertext = &encrypted[12..];

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| AgentOSError::VaultError(format!("Decryption failed: {}", e)))
}
