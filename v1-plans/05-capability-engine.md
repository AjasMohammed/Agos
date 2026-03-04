# Plan 05 — Capability Engine (`agentos-capability` crate)

## Goal

Implement the capability token system and permission matrix. The capability engine issues HMAC-SHA256-signed tokens for every task, validates them on every intent, and enforces the per-agent permission matrix.

## Dependencies

- `agentos-types`
- `hmac`, `sha2` — HMAC-SHA256 signing
- `rand` — random signing key generation
- `chrono`
- `serde`, `serde_json`

## Architecture

```
Agent connects → Kernel stores AgentProfile with PermissionSet
    ↓
Task created → CapabilityEngine issues a CapabilityToken
    Token contains: task_id, agent_id, allowed_tools, allowed_intents,
                    permissions (copied from agent's PermissionSet), expiry
    Token is HMAC-SHA256 signed with the kernel's secret key
    ↓
Intent arrives → CapabilityEngine.validate(token, intent)
    1. Verify HMAC signature (was this token issued by this kernel?)
    2. Check expiry (has the token expired?)
    3. Check target tool is in allowed_tools
    4. Check intent_type is in allowed_intents
    5. Check required permissions against token's PermissionSet
    → If all pass: allow. If any fail: return PermissionDenied.
```

## Core Struct: `CapabilityEngine`

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub struct CapabilityEngine {
    /// The kernel's secret signing key (256-bit, randomly generated at kernel start).
    signing_key: [u8; 32],
    /// Per-agent permission sets. Key is AgentID.
    agent_permissions: RwLock<HashMap<AgentID, PermissionSet>>,
}
```

## Public API

```rust
impl CapabilityEngine {
    /// Create a new engine with a randomly generated signing key.
    pub fn new() -> Self;

    /// Create from an existing key (for persistence across restarts).
    pub fn with_key(signing_key: [u8; 32]) -> Self;

    /// Register an agent with an initial permission set.
    pub fn register_agent(&self, agent_id: AgentID, permissions: PermissionSet);

    /// Update an agent's permission set (grant/revoke).
    pub fn update_permissions(&self, agent_id: &AgentID, permissions: PermissionSet)
        -> Result<(), AgentOSError>;

    /// Get an agent's current permissions.
    pub fn get_permissions(&self, agent_id: &AgentID) -> Result<PermissionSet, AgentOSError>;

    /// Issue a new capability token for a task.
    /// The token's permissions are copied from the agent's current PermissionSet.
    pub fn issue_token(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        allowed_tools: BTreeSet<ToolID>,
        allowed_intents: BTreeSet<IntentTypeFlag>,
        ttl: Duration,
    ) -> Result<CapabilityToken, AgentOSError>;

    /// Validate a capability token against an incoming intent.
    /// Returns Ok(()) if authorized, Err(PermissionDenied) if not.
    pub fn validate_intent(
        &self,
        token: &CapabilityToken,
        intent: &IntentMessage,
        required_permissions: &[(String, PermissionOp)],
    ) -> Result<(), AgentOSError>;

    /// Check ONLY the HMAC signature validity (not expiry or permissions).
    pub fn verify_signature(&self, token: &CapabilityToken) -> bool;
}
```

## Token Signing Implementation

```rust
/// Compute the HMAC-SHA256 signature for a token.
/// Signs over: task_id | agent_id | allowed_tools | allowed_intents | issued_at | expires_at
fn compute_signature(signing_key: &[u8; 32], token: &CapabilityToken) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(signing_key)
        .expect("HMAC can take any size key");

    // Create a canonical byte representation to sign
    mac.update(token.task_id.as_uuid().as_bytes());
    mac.update(token.agent_id.as_uuid().as_bytes());

    // Sort tool IDs for deterministic signing
    for tool_id in &token.allowed_tools {
        mac.update(tool_id.as_uuid().as_bytes());
    }

    // Encode intent flags as bytes
    for flag in &token.allowed_intents {
        mac.update(&[*flag as u8]);
    }

    // Timestamps
    mac.update(token.issued_at.to_rfc3339().as_bytes());
    mac.update(token.expires_at.to_rfc3339().as_bytes());

    mac.finalize().into_bytes().to_vec()
}
```

## Permission Parsing (for CLI)

Parse permission strings like `"fs.user_data:rw"`, `"network.outbound:x"`:

```rust
/// Parse a permission string like "resource:rwx" into a PermissionEntry.
pub fn parse_permission_str(s: &str) -> Result<PermissionEntry, AgentOSError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(AgentOSError::SchemaValidation(
            format!("Invalid permission format '{}', expected 'resource:rwx'", s)
        ));
    }

    let resource = parts[0].to_string();
    let bits = parts[1];
    let read = bits.contains('r');
    let write = bits.contains('w');
    let execute = bits.contains('x');

    if !read && !write && !execute {
        return Err(AgentOSError::SchemaValidation(
            format!("Permission bits must contain at least one of r, w, x")
        ));
    }

    Ok(PermissionEntry { resource, read, write, execute })
}
```

## Validation Flow (Detailed)

```rust
impl CapabilityEngine {
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

        // 3. Check target tool is allowed
        if let IntentTarget::Tool(tool_id) = &intent.target {
            if !token.allowed_tools.contains(tool_id) {
                return Err(AgentOSError::PermissionDenied {
                    resource: format!("tool:{}", tool_id),
                    operation: "invoke".into(),
                });
            }
        }

        // 4. Check intent type is allowed
        let intent_flag = intent_type_to_flag(&intent.intent_type);
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
        }

        Ok(())
    }
}
```

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_engine() -> CapabilityEngine {
        let engine = CapabilityEngine::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".into(), true, false, false);
        perms.grant("memory.semantic".into(), true, true, false);
        let agent_id = AgentID::new();
        engine.register_agent(agent_id, perms);
        engine
    }

    #[test]
    fn test_issue_and_verify_token() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        engine.register_agent(agent_id, PermissionSet::new());

        let token = engine.issue_token(
            TaskID::new(),
            agent_id,
            BTreeSet::new(),
            BTreeSet::from([IntentTypeFlag::Read]),
            Duration::from_secs(300),
        ).unwrap();

        assert!(engine.verify_signature(&token));
    }

    #[test]
    fn test_tampered_token_fails_signature() {
        let engine = CapabilityEngine::new();
        let agent_id = AgentID::new();
        engine.register_agent(agent_id, PermissionSet::new());

        let mut token = engine.issue_token(
            TaskID::new(),
            agent_id,
            BTreeSet::new(),
            BTreeSet::from([IntentTypeFlag::Read]),
            Duration::from_secs(300),
        ).unwrap();

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

        let token = engine.issue_token(
            TaskID::new(),
            agent_id,
            BTreeSet::new(),
            BTreeSet::from([IntentTypeFlag::Read]),
            Duration::from_secs(0),  // expires immediately
        ).unwrap();

        std::thread::sleep(Duration::from_millis(10));

        // Build a dummy intent
        let intent = IntentMessage { /* ... */ };
        let result = engine.validate_intent(&token, &intent, &[]);
        assert!(matches!(result, Err(AgentOSError::TokenExpired)));
    }

    #[test]
    fn test_permission_denied_for_missing_resource() {
        let engine = make_engine();
        // Agent has fs.user_data:r but NOT network.outbound:x
        // validate_intent with required [("network.outbound", Execute)] should fail
    }

    #[test]
    fn test_parse_permission_str() {
        let entry = parse_permission_str("fs.user_data:rw").unwrap();
        assert_eq!(entry.resource, "fs.user_data");
        assert!(entry.read);
        assert!(entry.write);
        assert!(!entry.execute);

        let entry = parse_permission_str("network.outbound:x").unwrap();
        assert!(!entry.read);
        assert!(!entry.write);
        assert!(entry.execute);

        assert!(parse_permission_str("invalid").is_err());
        assert!(parse_permission_str("resource:").is_err());
    }
}
```

## Verification

```bash
cargo test -p agentos-capability
```
