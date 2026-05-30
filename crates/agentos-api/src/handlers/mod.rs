//! HTTP handler modules — one per resource area.
//!
//! Each handler function takes Axum extractors (Path, Query, Json, State) and
//! delegates to the [`KernelService`] trait. Errors are returned as [`ApiError`]
//! which implements `IntoResponse`.

pub mod agents;
pub mod audit;
pub mod auth;
pub mod channels;
pub mod chat;
pub mod config;
pub mod connectors;
pub mod costs;
pub mod dashboard;
pub mod doctor;
pub mod escalations;
pub mod events;
pub mod identity;
pub mod keys;
pub mod logs;
pub mod mcp;
pub mod notifications;
pub mod pipelines;
pub mod plugins;
pub mod prefs;
pub mod roles;
pub mod schedules;
pub mod secrets;
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
/// Empty permissions on the key means full access (backwards compat with bootstrap key).
pub fn require_permission(key: &AuthenticatedKey, perm: &str) -> Result<(), ApiError> {
    if key.0.permissions.is_empty() {
        // Empty permissions = full access (backwards compat with bootstrap key)
        return Ok(());
    }
    let required_resource = perm.split(':').next().unwrap_or(perm);
    let required_op = perm.split(':').nth(1).unwrap_or("r");
    for p in &key.0.permissions {
        let res = p.split(':').next().unwrap_or(p);
        let op = p.split(':').nth(1).unwrap_or("r");
        if (res == required_resource || res == "*")
            && op.contains(required_op.chars().next().unwrap_or('r'))
        {
            return Ok(());
        }
    }
    Err(ApiError::Forbidden(format!("Missing permission: {perm}")))
}
