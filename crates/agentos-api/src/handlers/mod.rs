//! HTTP handler modules — one per resource area.
//!
//! Each handler function takes Axum extractors (Path, Query, Json, State) and
//! delegates to the [`KernelService`] trait. Errors are returned as [`ApiError`]
//! which implements `IntoResponse`.

pub mod agent_chats;
pub mod agents;
pub mod approval_policies;
pub mod audit;
pub mod auth;
pub mod channels;
pub mod chat;
pub mod chat_sessions;
pub mod config;
pub mod connectors;
pub mod costs;
pub mod dashboard;
pub mod doctor;
pub mod escalations;
pub mod events;
pub mod files;
pub mod identity;
pub mod inbox;
pub mod keys;
pub mod logs;
pub mod marketplace;
pub mod mcp;
pub mod memory;
pub mod notifications;
pub mod pipelines;
pub mod plugins;
pub mod prefs;
pub mod roles;
pub mod schedules;
pub mod scratchpad;
pub mod secrets;
pub mod skills;
pub mod sse;
pub mod system;
pub mod system_info;
pub mod tasks;
pub mod tools;
pub mod webhooks;
pub mod webhooks_admin;
pub mod workflows;

use crate::auth::AuthenticatedKey;
use crate::error::ApiError;

/// Check that the authenticated key has the required permission.
///
/// Permission format: `"resource:op"` where `op` is a single char like `r` or `w`.
///
/// An **empty** scope list grants NO access (fail-closed): a key created with
/// no scopes can authenticate but authorizes nothing. The bootstrap/admin key
/// holds the explicit `"*"` wildcard (see CLI `bootstrap` key issuance), which
/// is matched by the `res == "*"` arm below — it does NOT rely on emptiness.
pub fn require_permission(key: &AuthenticatedKey, perm: &str) -> Result<(), ApiError> {
    if key.0.permissions.is_empty() {
        return Err(ApiError::Forbidden(format!(
            "Key has no scopes; missing permission: {perm}"
        )));
    }
    let required_resource = perm.split(':').next().unwrap_or(perm);
    let required_op = perm.split(':').nth(1).unwrap_or("r");
    for p in &key.0.permissions {
        // A bare `"*"` is the admin wildcard: all resources AND all ops.
        if p == "*" {
            return Ok(());
        }
        let res = p.split(':').next().unwrap_or(p);
        let op = p.split(':').nth(1).unwrap_or("r");
        let res_ok = res == required_resource || res == "*";
        let op_ok = op == "*" || op.contains(required_op.chars().next().unwrap_or('r'));
        if res_ok && op_ok {
            return Ok(());
        }
    }
    Err(ApiError::Forbidden(format!("Missing permission: {perm}")))
}

#[cfg(test)]
mod require_permission_tests {
    use super::*;
    use crate::api_key::ApiKeyRecord;

    fn key(perms: &[&str]) -> AuthenticatedKey {
        AuthenticatedKey(ApiKeyRecord {
            id: "id".into(),
            name: "test".into(),
            key_hash: vec![],
            permissions: perms.iter().map(|s| s.to_string()).collect(),
            created_at: chrono::Utc::now(),
            last_used_at: None,
            expires_at: None,
            revoked: false,
        })
    }

    #[test]
    fn empty_scopes_grant_no_access() {
        // W9: an empty scope list must authorize nothing (fail-closed).
        assert!(require_permission(&key(&[]), "agents:r").is_err());
        assert!(require_permission(&key(&[]), "keys:w").is_err());
    }

    #[test]
    fn wildcard_key_grants_all() {
        // The bootstrap/admin key uses the explicit "*" wildcard.
        assert!(require_permission(&key(&["*"]), "agents:r").is_ok());
        assert!(require_permission(&key(&["*"]), "keys:w").is_ok());
    }

    #[test]
    fn scoped_key_grants_only_its_scope() {
        let k = key(&["agents:r"]);
        assert!(require_permission(&k, "agents:r").is_ok());
        assert!(require_permission(&k, "keys:w").is_err());
    }
}
