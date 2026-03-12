use crate::token::compute_signature;
use agentos_types::*;
use rand::RngCore;
use std::collections::{BTreeSet, HashMap};
use std::sync::RwLock;
use std::time::Duration;

/// Internal vault key name for the persisted HMAC signing key.
const SIGNING_KEY_NAME: &str = "__internal_hmac_signing_key";

pub struct CapabilityEngine {
    /// The kernel's secret signing key (256-bit).
    signing_key: [u8; 32],
    /// Per-agent permission sets. Key is AgentID.
    agent_permissions: RwLock<HashMap<AgentID, PermissionSet>>,
}

impl CapabilityEngine {
    /// Create a new engine with a randomly generated signing key.
    pub fn new() -> Self {
        let mut signing_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut signing_key);
        Self {
            signing_key,
            agent_permissions: RwLock::new(HashMap::new()),
        }
    }

    /// Create from an existing key (for persistence across restarts).
    pub fn with_key(signing_key: [u8; 32]) -> Self {
        Self {
            signing_key,
            agent_permissions: RwLock::new(HashMap::new()),
        }
    }

    /// Boot the capability engine: load existing signing key from the vault,
    /// or generate a new one and persist it. This ensures tokens survive restarts.
    pub fn boot(vault: &agentos_vault::SecretsVault) -> Self {
        match vault.get(SIGNING_KEY_NAME) {
            Ok(entry) => {
                let key_str = entry.as_str();
                // Stored as hex string
                if let Ok(key_bytes) = hex::decode(key_str) {
                    if key_bytes.len() == 32 {
                        let mut key = [0u8; 32];
                        key.copy_from_slice(&key_bytes);
                        tracing::info!("Loaded existing HMAC signing key from vault");
                        return Self::with_key(key);
                    }
                }
                tracing::warn!("Corrupt signing key in vault, generating new one");
                Self::generate_and_persist(vault)
            }
            Err(_) => {
                tracing::info!("No existing signing key found, generating new one");
                Self::generate_and_persist(vault)
            }
        }
    }

    /// Generate a new signing key and persist it in the vault.
    fn generate_and_persist(vault: &agentos_vault::SecretsVault) -> Self {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let key_hex = hex::encode(key);

        if let Err(e) = vault.set(
            SIGNING_KEY_NAME,
            &key_hex,
            agentos_types::SecretOwner::Kernel,
            agentos_types::SecretScope::Global,
        ) {
            tracing::error!(error = %e, "Failed to persist signing key to vault");
        }

        Self::with_key(key)
    }

    /// Register an agent with an initial permission set.
    pub fn register_agent(&self, agent_id: AgentID, permissions: PermissionSet) {
        let mut map = self.agent_permissions.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in capability engine write path"
            );
            error.into_inner()
        });
        map.insert(agent_id, permissions);
    }

    /// Update an agent's permission set (grant/revoke).
    pub fn update_permissions(
        &self,
        agent_id: &AgentID,
        permissions: PermissionSet,
    ) -> Result<(), AgentOSError> {
        let mut map = self.agent_permissions.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in capability engine write path"
            );
            error.into_inner()
        });
        map.insert(*agent_id, permissions);
        Ok(())
    }

    /// Revoke an agent's permissions entirely, removing them from the permission map.
    /// This effectively invalidates any tokens issued for the agent since they reference
    /// permissions that no longer exist.
    pub fn revoke_agent(&self, agent_id: &AgentID) {
        let mut map = self.agent_permissions.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in capability engine write path"
            );
            error.into_inner()
        });
        map.remove(agent_id);
    }

    /// Get an agent's current permissions.
    pub fn get_permissions(&self, agent_id: &AgentID) -> Result<PermissionSet, AgentOSError> {
        let map = self.agent_permissions.read().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in capability engine read path"
            );
            error.into_inner()
        });
        map.get(agent_id)
            .cloned()
            .ok_or_else(|| AgentOSError::PermissionDenied {
                resource: "agent_permissions".into(),
                operation: "get".into(),
            })
    }

    /// Issue a new capability token for a task.
    /// The token's permissions are provided by the caller (the Kernel), representing the effective
    /// permissions of the agent (base + roles + direct).
    pub fn issue_token(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        allowed_tools: BTreeSet<ToolID>,
        allowed_intents: BTreeSet<IntentTypeFlag>,
        effective_permissions: PermissionSet,
        ttl: Duration,
    ) -> Result<CapabilityToken, AgentOSError> {
        let issued_at = chrono::Utc::now();
        let expires_at = issued_at
            + chrono::Duration::from_std(ttl).map_err(|_| AgentOSError::KernelError {
                reason: format!("TTL duration {:?} out of range for chrono", ttl),
            })?;

        let mut token = CapabilityToken {
            task_id,
            agent_id,
            allowed_tools,
            allowed_intents,
            permissions: effective_permissions,
            issued_at,
            expires_at,
            signature: Vec::new(), // Will be populated next
        };

        token.signature = compute_signature(&self.signing_key, &token);
        Ok(token)
    }

    /// Validate a capability token against an incoming intent.
    /// Returns Ok(()) if authorized, Err(PermissionDenied) if not.
    pub fn validate_intent(
        &self,
        token: &CapabilityToken,
        intent: &IntentMessage,
        required_permissions: &[(String, PermissionOp)],
    ) -> Result<(), AgentOSError> {
        // 1. Verify HMAC signature
        if !self.verify_signature(token) {
            return Err(AgentOSError::InvalidToken {
                reason: "Invalid HMAC signature".into(),
            });
        }

        // 2. Check expiry
        if chrono::Utc::now() > token.expires_at {
            return Err(AgentOSError::TokenExpired);
        }

        // 3. Check target tool is allowed (if the target is a tool)
        if let IntentTarget::Tool(tool_id) = &intent.target {
            if !token.allowed_tools.contains(tool_id) {
                return Err(AgentOSError::PermissionDenied {
                    resource: format!("tool:{}", tool_id),
                    operation: "invoke".into(),
                });
            }
        }

        // 4. Check intent type is allowed
        let intent_flag = match intent.intent_type {
            IntentType::Read => IntentTypeFlag::Read,
            IntentType::Write => IntentTypeFlag::Write,
            IntentType::Execute => IntentTypeFlag::Execute,
            IntentType::Query => IntentTypeFlag::Query,
            IntentType::Observe => IntentTypeFlag::Observe,
            IntentType::Delegate => IntentTypeFlag::Delegate,
            IntentType::Message => IntentTypeFlag::Message,
            IntentType::Broadcast => IntentTypeFlag::Broadcast,
            IntentType::Escalate => IntentTypeFlag::Escalate,
        };

        if !token.allowed_intents.contains(&intent_flag) {
            return Err(AgentOSError::PermissionDenied {
                resource: "intent_type".into(),
                operation: format!("{:?}", intent.intent_type),
            });
        }

        // 5. Check required permissions
        for (resource, op) in required_permissions {
            if !token.permissions.check(resource, *op) {
                return Err(AgentOSError::PermissionDenied {
                    resource: resource.clone(),
                    operation: format!("{:?}", op),
                });
            }

            // Check if the individual permission has expired
            if let Some(entry) = token
                .permissions
                .entries
                .iter()
                .find(|e| e.resource == *resource)
            {
                if let Some(expires_at) = entry.expires_at {
                    if chrono::Utc::now() > expires_at {
                        return Err(AgentOSError::PermissionDenied {
                            resource: resource.clone(),
                            operation: "Permission expired".into(),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    /// Check ONLY the HMAC signature validity (not expiry or permissions).
    /// Uses constant-time comparison to prevent timing side-channel attacks.
    pub fn verify_signature(&self, token: &CapabilityToken) -> bool {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        // Recompute the HMAC and use verify_slice for constant-time comparison
        let mut mac =
            HmacSha256::new_from_slice(&self.signing_key).expect("HMAC can take any size key");

        // Replicate the same signing process from compute_signature
        mac.update(token.task_id.as_uuid().as_bytes());
        mac.update(token.agent_id.as_uuid().as_bytes());
        for tool_id in &token.allowed_tools {
            mac.update(tool_id.as_uuid().as_bytes());
        }
        for flag in &token.allowed_intents {
            mac.update(&[*flag as u8]);
        }
        for entry in &token.permissions.entries {
            mac.update(entry.resource.as_bytes());
            mac.update(&[entry.read as u8, entry.write as u8, entry.execute as u8]);
        }
        mac.update(token.issued_at.to_rfc3339().as_bytes());
        mac.update(token.expires_at.to_rfc3339().as_bytes());

        mac.verify_slice(&token.signature).is_ok()
    }

    /// Sign arbitrary bytes using the kernel's HMAC-SHA256 signing key.
    /// Used by the EventBus to sign `EventMessage` signatures.
    pub fn sign_data(&self, data: &[u8]) -> Vec<u8> {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let mut mac =
            HmacSha256::new_from_slice(&self.signing_key).expect("HMAC can take any size key");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// Verify an HMAC-SHA256 signature over arbitrary data.
    pub fn verify_data_signature(&self, data: &[u8], signature: &[u8]) -> bool {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let mut mac =
            HmacSha256::new_from_slice(&self.signing_key).expect("HMAC can take any size key");
        mac.update(data);
        mac.verify_slice(signature).is_ok()
    }
}

impl Default for CapabilityEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_and_verify_token() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        engine.register_agent(agent_id, PermissionSet::new());

        let token = engine
            .issue_token(
                TaskID::new(),
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read]),
                PermissionSet::new(),
                Duration::from_secs(300),
            )
            .unwrap();

        assert!(engine.verify_signature(&token));
    }

    #[test]
    fn test_tampered_token_fails_signature() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        engine.register_agent(agent_id, PermissionSet::new());

        let mut token = engine
            .issue_token(
                TaskID::new(),
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read]),
                PermissionSet::new(),
                Duration::from_secs(300),
            )
            .unwrap();

        // Tamper with the token
        token.allowed_intents.insert(IntentTypeFlag::Write);

        // Signature should now be invalid
        assert!(!engine.verify_signature(&token));
    }

    #[test]
    fn test_expired_token_rejected() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        engine.register_agent(agent_id, PermissionSet::new());

        let token = engine
            .issue_token(
                TaskID::new(),
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read]),
                PermissionSet::new(),
                Duration::from_secs(0), // expires immediately
            )
            .unwrap();

        std::thread::sleep(Duration::from_millis(10));

        let intent = IntentMessage {
            id: MessageID::new(),
            sender_token: token.clone(),
            intent_type: IntentType::Read,
            target: IntentTarget::Kernel,
            payload: SemanticPayload {
                schema: "Test".to_string(),
                data: serde_json::Value::Null,
            },
            context_ref: ContextID::new(),
            priority: 0,
            timeout_ms: 1000,
            trace_id: TraceID::new(),
            timestamp: chrono::Utc::now(),
        };
        let result = engine.validate_intent(&token, &intent, &[]);
        assert!(matches!(result, Err(AgentOSError::TokenExpired)));
    }

    #[test]
    fn test_permission_denied_for_missing_resource() {
        let engine = CapabilityEngine::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".into(), true, false, false, None);
        // NO network.outbound:x

        let agent_id = AgentID::new();
        engine.register_agent(agent_id, perms.clone());

        let token = engine
            .issue_token(
                TaskID::new(),
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read]),
                perms.clone(),
                Duration::from_secs(300),
            )
            .unwrap();

        let intent = IntentMessage {
            id: MessageID::new(),
            sender_token: token.clone(),
            intent_type: IntentType::Read,
            target: IntentTarget::Kernel,
            payload: SemanticPayload {
                schema: "Test".to_string(),
                data: serde_json::Value::Null,
            },
            context_ref: ContextID::new(),
            priority: 0,
            timeout_ms: 1000,
            trace_id: TraceID::new(),
            timestamp: chrono::Utc::now(),
        };

        // Missing network.outbound Execute permission
        let result = engine.validate_intent(
            &token,
            &intent,
            &[("network.outbound".to_string(), PermissionOp::Execute)],
        );

        match result {
            Err(AgentOSError::PermissionDenied { resource, .. }) => {
                assert_eq!(resource, "network.outbound")
            }
            _ => panic!("Expected permission denied error"),
        }
    }
}
