use crate::token::compute_signature;
use agentos_types::*;
use rand::RngCore;
use std::collections::{BTreeSet, HashMap};
use std::sync::RwLock;
use std::time::Duration;
use zeroize::Zeroize;

/// Internal vault key name for the persisted HMAC signing key.
const SIGNING_KEY_NAME: &str = "__internal_hmac_signing_key";

pub struct CapabilityEngine {
    /// The kernel's secret signing key (256-bit).
    /// Zeroized on drop to prevent key material lingering in memory.
    signing_key: [u8; 32],
    /// Per-agent permission sets. Key is AgentID.
    agent_permissions: RwLock<HashMap<AgentID, PermissionSet>>,
}

impl Drop for CapabilityEngine {
    fn drop(&mut self) {
        self.signing_key.zeroize();
    }
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
    pub async fn boot(vault: &agentos_vault::SecretsVault) -> Self {
        match vault.get(SIGNING_KEY_NAME).await {
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
                Self::generate_and_persist(vault).await
            }
            Err(_) => {
                tracing::info!("No existing signing key found, generating new one");
                Self::generate_and_persist(vault).await
            }
        }
    }

    /// Generate a new signing key and persist it in the vault.
    /// Uses `SecretScope::Kernel` so no agent can proxy-access this key.
    async fn generate_and_persist(vault: &agentos_vault::SecretsVault) -> Self {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let key_hex = hex::encode(key);

        if let Err(e) = vault
            .set(
                SIGNING_KEY_NAME,
                &key_hex,
                agentos_types::SecretOwner::Kernel,
                agentos_types::SecretScope::Kernel,
            )
            .await
        {
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
            IntentType::Subscribe => IntentTypeFlag::Subscribe,
            IntentType::Unsubscribe => IntentTypeFlag::Unsubscribe,
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
    ///
    /// Delegates to `verify_token_signature` to avoid duplicating the HMAC field layout.
    pub fn verify_signature(&self, token: &CapabilityToken) -> bool {
        crate::token::verify_token_signature(&self.signing_key, token)
    }

    /// Issue a capability token for a child agent scoped to the intersection of
    /// `parent_token`'s permissions and `requested`.
    ///
    /// # Security invariant
    /// The child token is issued for `child_task_id` and `child_agent_id` — never
    /// re-using the parent's task or agent IDs — preventing scheduler state corruption
    /// and ensuring audit attribution is always correct.
    ///
    /// Returns `Err` if the parent token is invalid, expired, or the intersection is empty.
    pub fn scope_for_child(
        &self,
        parent_token: &CapabilityToken,
        child_task_id: TaskID,
        child_agent_id: AgentID,
        requested: &PermissionSet,
        ttl: Duration,
    ) -> Result<CapabilityToken, AgentOSError> {
        // 1. Verify parent token signature — reject tampered tokens immediately.
        if !self.verify_signature(parent_token) {
            tracing::warn!(
                parent_task_id = %parent_token.task_id,
                parent_agent_id = %parent_token.agent_id,
                "scope_for_child: parent token has invalid HMAC signature"
            );
            return Err(AgentOSError::InvalidToken {
                reason: "Parent token has invalid HMAC signature".into(),
            });
        }

        // 2. Check parent token expiry.
        let now = chrono::Utc::now();
        if now > parent_token.expires_at {
            tracing::warn!(
                parent_task_id = %parent_token.task_id,
                expired_at = %parent_token.expires_at,
                "scope_for_child: parent token has expired"
            );
            return Err(AgentOSError::TokenExpired);
        }

        // 3. Intersect parent permissions with what the child requested.
        //    This is the core security invariant: child ⊆ parent, always.
        let intersection = parent_token.permissions.intersect(requested);

        // 4. Reject empty intersection — child asked for nothing the parent holds.
        if intersection.is_empty() {
            tracing::warn!(
                parent_task_id = %parent_token.task_id,
                child_task_id = %child_task_id,
                child_agent_id = %child_agent_id,
                "scope_for_child: requested permissions have empty intersection with parent"
            );
            return Err(AgentOSError::PermissionDenied {
                resource: "child_permissions".into(),
                operation: "child requested permissions not held by parent".into(),
            });
        }

        tracing::debug!(
            parent_task_id = %parent_token.task_id,
            child_task_id = %child_task_id,
            child_agent_id = %child_agent_id,
            granted_resources = intersection.entries.len(),
            "scope_for_child: issuing scoped child token"
        );

        // 5. Issue a fresh token scoped to the child's own task/agent IDs.
        //    Using child_task_id (not parent) prevents scheduler entry collision.
        self.issue_token(
            child_task_id,
            child_agent_id,
            parent_token.allowed_tools.clone(),
            parent_token.allowed_intents.clone(),
            intersection,
            ttl,
        )
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

    /// Mirrors the per-turn token the chat path mints (finding S1): a read-only
    /// chat turn produces a token whose `allowed_intents` is `{Read}`. Such a
    /// token must ALLOW an in-scope Read intent and DENY an out-of-scope Execute
    /// intent — the scoped-intent narrowing that chat previously lacked entirely
    /// (it ran at the agent's full standing permissions with no token at all).
    #[test]
    fn test_chat_style_scoped_intent_token() {
        let engine = CapabilityEngine::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".into(), true, false, false, None);
        let agent_id = AgentID::new();
        engine.register_agent(agent_id, perms.clone());

        // A read-only turn: allowed_intents scoped to {Read}, exactly as the
        // chat loop derives from the turn's requested intents.
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

        let make_intent = |intent_type: IntentType| IntentMessage {
            id: MessageID::new(),
            sender_token: token.clone(),
            intent_type,
            target: IntentTarget::Kernel,
            payload: SemanticPayload {
                schema: "fs-read".to_string(),
                data: serde_json::Value::Null,
            },
            context_ref: ContextID::new(),
            priority: 5,
            timeout_ms: 1000,
            trace_id: TraceID::new(),
            timestamp: chrono::Utc::now(),
        };

        // In-scope Read with a held permission → allowed.
        assert!(engine
            .validate_intent(
                &token,
                &make_intent(IntentType::Read),
                &[("fs.user_data".to_string(), PermissionOp::Read)],
            )
            .is_ok());

        // Out-of-scope Execute intent → denied on intent_type, even though the
        // agent's standing permissions might otherwise cover it.
        match engine.validate_intent(&token, &make_intent(IntentType::Execute), &[]) {
            Err(AgentOSError::PermissionDenied { resource, .. }) => {
                assert_eq!(resource, "intent_type")
            }
            other => panic!("expected intent_type PermissionDenied, got {other:?}"),
        }
    }

    #[test]
    fn test_deny_entries_tampering_invalidates_signature() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs:/home/user/".into(), true, true, false, None);
        perms.deny("fs:/home/user/.ssh/".into());
        engine.register_agent(agent_id, perms.clone());

        let mut token = engine
            .issue_token(
                TaskID::new(),
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read]),
                perms,
                Duration::from_secs(300),
            )
            .unwrap();

        // Verify original token is valid
        assert!(engine.verify_signature(&token));

        // Tamper: strip all deny entries
        token.permissions.deny_entries.clear();

        // Signature must now be invalid
        assert!(!engine.verify_signature(&token));
    }

    #[test]
    fn test_expires_at_tampering_invalidates_signature() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        let mut perms = PermissionSet::new();
        let past = chrono::Utc::now() - chrono::Duration::seconds(60);
        perms.grant("fs:/tmp/".into(), true, false, false, Some(past));
        engine.register_agent(agent_id, perms.clone());

        let mut token = engine
            .issue_token(
                TaskID::new(),
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read]),
                perms,
                Duration::from_secs(300),
            )
            .unwrap();

        // Verify original token is valid
        assert!(engine.verify_signature(&token));

        // Tamper: remove expires_at to make the time-limited permission permanent
        token.permissions.entries[0].expires_at = None;

        // Signature must now be invalid
        assert!(!engine.verify_signature(&token));
    }

    #[test]
    fn test_cross_engine_token_rejected() {
        let engine1 = CapabilityEngine::new();
        let engine2 = CapabilityEngine::new();
        let agent_id = AgentID::new();
        engine1.register_agent(agent_id, PermissionSet::new());

        let token = engine1
            .issue_token(
                TaskID::new(),
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read]),
                PermissionSet::new(),
                Duration::from_secs(300),
            )
            .unwrap();

        // Token signed by engine1 must fail verification on engine2 (different key)
        assert!(!engine2.verify_signature(&token));
    }

    #[test]
    fn test_scope_for_child_intersects_permissions() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        let task_id = TaskID::new();

        // Parent has read + write + shell (fs.user_data = rw, shell.exec = x)
        let mut parent_perms = PermissionSet::new();
        parent_perms.grant("fs.user_data".into(), true, true, false, None); // read + write
        parent_perms.grant("shell.exec".into(), false, false, true, None); // execute (shell)
        engine.register_agent(agent_id, parent_perms.clone());

        let parent_token = engine
            .issue_token(
                task_id,
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read, IntentTypeFlag::Write]),
                parent_perms,
                Duration::from_secs(300),
            )
            .unwrap();

        // Child requests read + shell + network (network not in parent)
        let mut child_requested = PermissionSet::new();
        child_requested.grant("fs.user_data".into(), true, false, false, None); // read only
        child_requested.grant("shell.exec".into(), false, false, true, None); // execute
        child_requested.grant("network.outbound".into(), false, false, true, None); // NOT in parent

        let child_task_id = TaskID::new();
        let child_agent_id = AgentID::new();
        let child_token = engine
            .scope_for_child(
                &parent_token,
                child_task_id,
                child_agent_id,
                &child_requested,
                Duration::from_secs(300),
            )
            .unwrap();
        // Verify child token is bound to child's IDs, not the parent's.
        assert_eq!(
            child_token.task_id, child_task_id,
            "child token must use child task ID"
        );
        assert_eq!(
            child_token.agent_id, child_agent_id,
            "child token must use child agent ID"
        );

        // Verify child token is properly signed
        assert!(engine.verify_signature(&child_token));

        let child_perms = &child_token.permissions;
        // read on fs.user_data — in both parent and child request
        assert!(
            child_perms.check("fs.user_data", PermissionOp::Read),
            "child should have read on fs.user_data"
        );
        // shell execute — in both parent and child request
        assert!(
            child_perms.check("shell.exec", PermissionOp::Execute),
            "child should have shell execute"
        );
        // network — NOT in parent, must be excluded
        assert!(
            !child_perms.check("network.outbound", PermissionOp::Execute),
            "network not in parent — must be excluded"
        );
        // write on fs.user_data — in parent but NOT requested by child
        assert!(
            !child_perms.check("fs.user_data", PermissionOp::Write),
            "write not requested by child — must be excluded"
        );
    }

    #[test]
    fn test_scope_for_child_empty_intersection_errors() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        let task_id = TaskID::new();

        let mut parent_perms = PermissionSet::new();
        parent_perms.grant("fs.user_data".into(), true, false, false, None);
        engine.register_agent(agent_id, parent_perms.clone());

        let parent_token = engine
            .issue_token(
                task_id,
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read]),
                parent_perms,
                Duration::from_secs(300),
            )
            .unwrap();

        // Child requests something the parent doesn't have
        let mut child_requested = PermissionSet::new();
        child_requested.grant("network.outbound".into(), false, false, true, None);
        child_requested.grant("shell.exec".into(), false, false, true, None);

        let result = engine.scope_for_child(
            &parent_token,
            TaskID::new(),
            AgentID::new(),
            &child_requested,
            Duration::from_secs(300),
        );
        assert!(result.is_err(), "empty intersection should return error");
        assert!(
            matches!(result, Err(AgentOSError::PermissionDenied { .. })),
            "should be PermissionDenied error"
        );
    }

    #[test]
    fn test_serialization_roundtrip_preserves_signature() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".into(), true, true, false, None);
        perms.deny("fs:~/.ssh/".into());
        engine.register_agent(agent_id, perms.clone());

        let token = engine
            .issue_token(
                TaskID::new(),
                agent_id,
                BTreeSet::new(),
                BTreeSet::from([IntentTypeFlag::Read, IntentTypeFlag::Write]),
                perms,
                Duration::from_secs(300),
            )
            .unwrap();

        // Serialize to JSON and back
        let json = serde_json::to_string(&token).unwrap();
        let deserialized: CapabilityToken = serde_json::from_str(&json).unwrap();

        // Signature must still verify after round-trip
        assert!(engine.verify_signature(&deserialized));
    }
}
