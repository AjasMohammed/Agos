use agentos_types::{AgentID, SecretOwner, SecretScope};
use agentos_vault::SecretsVault;
use ed25519_dalek::{SigningKey, VerifyingKey};
use std::sync::Arc;

/// Manages Ed25519 cryptographic identities for agents.
///
/// On agent registration, generates a keypair:
/// - Private (signing) key → stored in the vault keyed by agent_id
/// - Public (verifying) key → stored as hex in the agent profile
///
/// On agent restart, reloads the keypair from the vault so the identity
/// persists across kernel restarts.
pub struct IdentityManager {
    vault: Arc<SecretsVault>,
}

impl IdentityManager {
    pub fn new(vault: Arc<SecretsVault>) -> Self {
        Self { vault }
    }

    /// Generate a new Ed25519 keypair for an agent.
    /// Returns the hex-encoded public key. The private key is stored in the vault.
    pub async fn generate_identity(&self, agent_id: &AgentID) -> Result<String, anyhow::Error> {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();

        // Store the 32-byte signing key seed in the vault
        let vault_key = format!("agent_identity:{}", agent_id);
        self.vault
            .set(
                &vault_key,
                &hex::encode(signing_key.to_bytes()),
                SecretOwner::Agent(*agent_id),
                SecretScope::Agent(*agent_id),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to store signing key: {}", e))?;

        let public_hex = hex::encode(verifying_key.to_bytes());
        Ok(public_hex)
    }

    /// Load an existing signing key from the vault.
    /// Returns None if no identity exists for this agent.
    pub async fn load_signing_key(
        &self,
        agent_id: &AgentID,
    ) -> Result<Option<SigningKey>, anyhow::Error> {
        let vault_key = format!("agent_identity:{}", agent_id);
        match self.vault.get(&vault_key).await {
            Ok(zeroizing_str) => {
                let bytes = hex::decode(zeroizing_str.as_str())
                    .map_err(|e| anyhow::anyhow!("Invalid hex in vault: {}", e))?;
                let key_bytes: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Invalid key length in vault"))?;
                Ok(Some(SigningKey::from_bytes(&key_bytes)))
            }
            Err(_) => Ok(None),
        }
    }

    /// Revoke an agent's identity by removing the signing key from the vault.
    pub async fn revoke_identity(&self, agent_id: &AgentID) -> Result<(), anyhow::Error> {
        let vault_key = format!("agent_identity:{}", agent_id);
        self.vault
            .revoke(&vault_key)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to revoke identity: {}", e))?;
        Ok(())
    }

    /// Sign a message with the agent's private key.
    /// Returns the hex-encoded signature, or an error if the agent has no identity.
    pub async fn sign_message(
        &self,
        agent_id: &AgentID,
        message: &[u8],
    ) -> Result<String, anyhow::Error> {
        use ed25519_dalek::Signer;
        let signing_key = self
            .load_signing_key(agent_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("No identity found for agent {}", agent_id))?;
        let signature = signing_key.sign(message);
        Ok(hex::encode(signature.to_bytes()))
    }

    /// Verify a signature against a known public key (hex-encoded).
    pub fn verify_signature(
        public_key_hex: &str,
        message: &[u8],
        signature_hex: &str,
    ) -> Result<bool, anyhow::Error> {
        use ed25519_dalek::Verifier;
        let pub_bytes = hex::decode(public_key_hex)
            .map_err(|e| anyhow::anyhow!("Invalid public key hex: {}", e))?;
        let pub_array: [u8; 32] = pub_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid public key length"))?;
        let verifying_key = VerifyingKey::from_bytes(&pub_array)
            .map_err(|e| anyhow::anyhow!("Invalid public key: {}", e))?;

        let sig_bytes = hex::decode(signature_hex)
            .map_err(|e| anyhow::anyhow!("Invalid signature hex: {}", e))?;
        let sig_array: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid signature length"))?;
        let signature = ed25519_dalek::Signature::from_bytes(&sig_array);

        Ok(verifying_key.verify(message, &signature).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_audit::AuditLog;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<SecretsVault>) {
        let dir = TempDir::new().unwrap();
        let vault_path = dir.path().join("test.vault");
        let audit_path = dir.path().join("audit.db");
        let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
        let vault = Arc::new(
            SecretsVault::initialize(
                &vault_path,
                &agentos_vault::ZeroizingString::new("test-passphrase".to_string()),
                audit,
            )
            .unwrap(),
        );
        (dir, vault)
    }

    #[tokio::test]
    async fn test_generate_and_load_identity() {
        let (_dir, vault) = setup();
        let mgr = IdentityManager::new(vault);
        let agent_id = AgentID::new();

        let pub_hex = mgr.generate_identity(&agent_id).await.unwrap();
        assert_eq!(pub_hex.len(), 64); // 32 bytes = 64 hex chars

        let loaded = mgr.load_signing_key(&agent_id).await.unwrap();
        assert!(loaded.is_some());

        // Verify the loaded key produces the same public key
        let loaded_pub = hex::encode(loaded.unwrap().verifying_key().to_bytes());
        assert_eq!(pub_hex, loaded_pub);
    }

    #[tokio::test]
    async fn test_sign_and_verify() {
        let (_dir, vault) = setup();
        let mgr = IdentityManager::new(vault);
        let agent_id = AgentID::new();

        let pub_hex = mgr.generate_identity(&agent_id).await.unwrap();
        let message = b"hello world";

        let sig_hex = mgr.sign_message(&agent_id, message).await.unwrap();
        let valid = IdentityManager::verify_signature(&pub_hex, message, &sig_hex).unwrap();
        assert!(valid);

        // Tampered message should fail
        let valid2 = IdentityManager::verify_signature(&pub_hex, b"tampered", &sig_hex).unwrap();
        assert!(!valid2);
    }

    #[tokio::test]
    async fn test_revoke_identity() {
        let (_dir, vault) = setup();
        let mgr = IdentityManager::new(vault);
        let agent_id = AgentID::new();

        mgr.generate_identity(&agent_id).await.unwrap();
        mgr.revoke_identity(&agent_id).await.unwrap();

        let loaded = mgr.load_signing_key(&agent_id).await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_no_identity_sign_fails() {
        let (_dir, vault) = setup();
        let mgr = IdentityManager::new(vault);
        let agent_id = AgentID::new();

        let result = mgr.sign_message(&agent_id, b"test").await;
        assert!(result.is_err());
    }
}
