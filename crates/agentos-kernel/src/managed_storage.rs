//! Managed Storage Zones capability provider (`storage.*`).
//!
//! Expands agent filesystem access beyond `data_dir` through policy-controlled,
//! audited storage zones. Zones are per-agent, time-limited, and revocable.
//!
//! File tools (reader, writer, editor) check the zone table when validating paths.
//! A path within an active zone for the requesting agent is allowed even if it's
//! outside `data_dir` and `workspace_paths`.

use crate::capability_provider::{CapabilityContext, CapabilityProvider, CapabilityResult};
use agentos_types::{AgentID, AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Access level for a storage zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneAccess {
    /// Read-only access.
    ReadOnly,
    /// Read and write access.
    ReadWrite,
}

impl ZoneAccess {
    fn from_str_loose(s: &str) -> Result<Self, AgentOSError> {
        match s.to_ascii_lowercase().as_str() {
            "ro" | "read" | "read_only" | "readonly" => Ok(Self::ReadOnly),
            "rw" | "readwrite" | "read_write" => Ok(Self::ReadWrite),
            other => Err(AgentOSError::SchemaValidation(format!(
                "invalid access level '{other}': expected 'ro' or 'rw'"
            ))),
        }
    }
}

/// How a zone was granted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneGrantSource {
    /// Granted by policy (path matched an allowed pattern).
    Policy,
    /// Granted by operator approval via escalation.
    OperatorApproval { escalation_id: u64 },
}

/// A filesystem zone granting an agent access to a specific directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageZone {
    /// Unique zone identifier.
    pub zone_id: String,
    /// The agent that owns this zone.
    pub agent_id: AgentID,
    /// Canonical absolute path to the directory.
    pub path: PathBuf,
    /// Access level (read-only or read-write).
    pub access: ZoneAccess,
    /// When this zone was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Optional expiry time (None = no expiry).
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// How this zone was granted.
    pub granted_by: ZoneGrantSource,
}

impl StorageZone {
    /// Check whether this zone has expired.
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map(|exp| chrono::Utc::now() > exp)
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the managed storage capability.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StorageConfig {
    /// Glob patterns for paths that agents may request access to without approval.
    #[serde(default)]
    pub allowed_zone_patterns: Vec<String>,
    /// Glob patterns for paths that are NEVER accessible (deny > allow).
    #[serde(default = "default_denied_patterns")]
    pub denied_zone_patterns: Vec<String>,
    /// Maximum number of active zones per agent.
    #[serde(default = "default_max_zones")]
    pub max_zones_per_agent: usize,
}

fn default_denied_patterns() -> Vec<String> {
    vec![
        "/etc/**".into(),
        "/root/**".into(),
        "/home/*/.ssh/**".into(),
        "/home/*/.gnupg/**".into(),
        "/home/*/.aws/**".into(),
        "/home/*/.config/gcloud/**".into(),
        "/var/**".into(),
        "/usr/**".into(),
        "/boot/**".into(),
        "/proc/**".into(),
        "/sys/**".into(),
    ]
}

fn default_max_zones() -> usize {
    10
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            allowed_zone_patterns: vec![
                "/home/*/projects/**".into(),
                "/home/*/Desktop/**".into(),
                "/tmp/agentos-*/**".into(),
            ],
            denied_zone_patterns: default_denied_patterns(),
            max_zones_per_agent: default_max_zones(),
        }
    }
}

// ---------------------------------------------------------------------------
// Glob matching
// ---------------------------------------------------------------------------

/// Simple glob pattern matching for path policy checks.
///
/// Supports `*` (single path component) and `**` (any depth).
/// This is intentionally simple — no regex, no character classes.
fn glob_matches(pattern: &str, path: &str) -> bool {
    let raw_parts: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    // Collapse consecutive `**` to prevent exponential recursion.
    let pat_parts: Vec<&str> = raw_parts.into_iter().fold(Vec::new(), |mut acc, p| {
        if p == "**" && acc.last() == Some(&"**") {
            // Skip duplicate
        } else {
            acc.push(p);
        }
        acc
    });
    let path_parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    glob_match_parts(&pat_parts, &path_parts)
}

fn glob_match_parts(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return true; // Pattern exhausted — match (path may have more components)
    }
    if path.is_empty() {
        // Path exhausted — only match if remaining pattern is all `**`
        return pattern.iter().all(|p| *p == "**");
    }

    match pattern[0] {
        "**" => {
            // ** matches zero or more path components
            // Try matching: skip 0, 1, 2, ... path components
            for skip in 0..=path.len() {
                if glob_match_parts(&pattern[1..], &path[skip..]) {
                    return true;
                }
            }
            false
        }
        pat => {
            // Single component match (supports * as wildcard within component)
            if component_matches(pat, path[0]) {
                glob_match_parts(&pattern[1..], &path[1..])
            } else {
                false
            }
        }
    }
}

/// Match a single path component against a pattern component.
/// Supports `*` as a wildcard matching any substring.
fn component_matches(pattern: &str, component: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == component;
    }

    // Split pattern on `*` and match greedily
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = component[pos..].find(part) {
            if i == 0 && found != 0 {
                return false; // First part must match at start
            }
            pos += found + part.len();
        } else {
            return false;
        }
    }

    // If pattern doesn't end with *, the remaining component must be consumed
    if !pattern.ends_with('*') {
        return pos == component.len();
    }

    true
}

// ---------------------------------------------------------------------------
// Zone table (shared state)
// ---------------------------------------------------------------------------

/// Shared zone table accessible from both the provider and file tools.
///
/// Wrapped in `Arc<RwLock<_>>` so file tools can check zone membership
/// without going through the full capability provider dispatch.
#[derive(Clone)]
pub struct ZoneTable {
    inner: Arc<RwLock<ZoneTableInner>>,
}

struct ZoneTableInner {
    /// All active zones, keyed by zone_id.
    zones: HashMap<String, StorageZone>,
    /// Counter for generating zone IDs.
    next_id: u64,
}

impl ZoneTable {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ZoneTableInner {
                zones: HashMap::new(),
                next_id: 1,
            })),
        }
    }

    /// Check whether a path is within an active (non-expired) zone for this agent.
    pub async fn is_path_in_zone(&self, agent_id: &AgentID, path: &Path) -> bool {
        let inner = self.inner.read().await;
        inner
            .zones
            .values()
            .any(|z| z.agent_id == *agent_id && !z.is_expired() && path.starts_with(&z.path))
    }

    /// Synchronous version for use in non-async contexts.
    /// Uses `try_read` to avoid blocking — returns `false` if lock contended.
    pub fn is_path_in_zone_sync(&self, agent_id: &AgentID, path: &Path) -> bool {
        match self.inner.try_read() {
            Ok(inner) => inner
                .zones
                .values()
                .any(|z| z.agent_id == *agent_id && !z.is_expired() && path.starts_with(&z.path)),
            Err(_) => false, // Lock contended — conservative deny
        }
    }

    /// Check zone access level for a path.
    ///
    /// When multiple zones overlap, returns the access level of the most
    /// specific (longest path prefix) match. This ensures a ReadOnly sub-zone
    /// overrides a ReadWrite parent zone.
    pub async fn zone_access_for(&self, agent_id: &AgentID, path: &Path) -> Option<ZoneAccess> {
        let inner = self.inner.read().await;
        inner
            .zones
            .values()
            .filter(|z| z.agent_id == *agent_id && !z.is_expired() && path.starts_with(&z.path))
            .max_by_key(|z| z.path.as_os_str().len())
            .map(|z| z.access)
    }

    /// List all zones for an agent.
    pub async fn list_for_agent(&self, agent_id: &AgentID) -> Vec<StorageZone> {
        let inner = self.inner.read().await;
        inner
            .zones
            .values()
            .filter(|z| z.agent_id == *agent_id && !z.is_expired())
            .cloned()
            .collect()
    }

    /// Insert a new zone. Returns the zone_id.
    pub async fn insert(&self, zone: StorageZone) -> String {
        let mut inner = self.inner.write().await;
        let id = zone.zone_id.clone();
        inner.zones.insert(id.clone(), zone);
        id
    }

    /// Atomically check the zone limit, generate an ID, and insert a zone.
    ///
    /// Holds a single write lock across the entire operation to prevent
    /// TOCTOU races where concurrent requests both pass the limit check.
    pub async fn insert_if_under_limit(
        &self,
        agent_id: AgentID,
        path: std::path::PathBuf,
        access: ZoneAccess,
        granted_by: ZoneGrantSource,
        max_zones: usize,
    ) -> Result<String, AgentOSError> {
        let mut inner = self.inner.write().await;

        let count = inner
            .zones
            .values()
            .filter(|z| z.agent_id == agent_id && !z.is_expired())
            .count();
        if count >= max_zones {
            return Err(AgentOSError::KernelError {
                reason: format!(
                    "agent has reached the maximum of {max_zones} active storage zones"
                ),
            });
        }

        let zone_id = format!("zone-{}", inner.next_id);
        inner.next_id += 1;

        let zone = StorageZone {
            zone_id: zone_id.clone(),
            agent_id,
            path,
            access,
            created_at: chrono::Utc::now(),
            expires_at: None,
            granted_by,
        };

        inner.zones.insert(zone_id.clone(), zone);
        Ok(zone_id)
    }

    /// Remove a zone by ID, returning it if found.
    pub async fn remove(&self, zone_id: &str, agent_id: &AgentID) -> Option<StorageZone> {
        let mut inner = self.inner.write().await;
        // Only allow removing zones owned by the requesting agent.
        if let Some(zone) = inner.zones.get(zone_id) {
            if zone.agent_id != *agent_id {
                return None;
            }
        }
        inner.zones.remove(zone_id)
    }

    /// Count active zones for an agent.
    pub async fn count_for_agent(&self, agent_id: &AgentID) -> usize {
        let inner = self.inner.read().await;
        inner
            .zones
            .values()
            .filter(|z| z.agent_id == *agent_id && !z.is_expired())
            .count()
    }

    /// Generate a new unique zone ID.
    pub async fn next_zone_id(&self) -> String {
        let mut inner = self.inner.write().await;
        let id = format!("zone-{}", inner.next_id);
        inner.next_id += 1;
        id
    }

    /// Sweep expired zones. Returns the number of zones removed.
    pub async fn sweep_expired(&self) -> usize {
        let mut inner = self.inner.write().await;
        let before = inner.zones.len();
        inner.zones.retain(|_, z| !z.is_expired());
        before - inner.zones.len()
    }
}

impl Default for ZoneTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// StorageZoneQuery implementation
// ---------------------------------------------------------------------------

/// Implements the cross-crate `StorageZoneQuery` trait so file tools can check
/// zone membership without depending on `agentos-kernel`.
impl agentos_types::StorageZoneQuery for ZoneTable {
    fn is_path_in_zone(&self, agent_id: &AgentID, path: &std::path::Path) -> bool {
        self.is_path_in_zone_sync(agent_id, path)
    }

    fn zone_access(
        &self,
        agent_id: &AgentID,
        path: &std::path::Path,
    ) -> Option<agentos_types::ZoneAccessLevel> {
        match self.inner.try_read() {
            Ok(inner) => inner
                .zones
                .values()
                .filter(|z| z.agent_id == *agent_id && !z.is_expired() && path.starts_with(&z.path))
                .max_by_key(|z| z.path.as_os_str().len())
                .map(|z| match z.access {
                    ZoneAccess::ReadOnly => agentos_types::ZoneAccessLevel::ReadOnly,
                    ZoneAccess::ReadWrite => agentos_types::ZoneAccessLevel::ReadWrite,
                }),
            Err(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// StorageProvider
// ---------------------------------------------------------------------------

/// Managed storage zones capability provider.
pub struct StorageProvider {
    config: StorageConfig,
    zone_table: ZoneTable,
}

impl StorageProvider {
    pub fn new(config: StorageConfig, zone_table: ZoneTable) -> Self {
        Self { config, zone_table }
    }

    pub fn with_defaults(zone_table: ZoneTable) -> Self {
        Self::new(StorageConfig::default(), zone_table)
    }

    /// Get a reference to the zone table for sharing with file tools.
    pub fn zone_table(&self) -> &ZoneTable {
        &self.zone_table
    }

    /// Check if a path matches any denied pattern.
    fn is_denied(&self, path_str: &str) -> bool {
        self.config
            .denied_zone_patterns
            .iter()
            .any(|pat| glob_matches(pat, path_str))
    }

    /// Check if a path matches any allowed pattern.
    fn is_allowed(&self, path_str: &str) -> bool {
        self.config
            .allowed_zone_patterns
            .iter()
            .any(|pat| glob_matches(pat, path_str))
    }

    async fn action_zone_create(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let path_str = params["path"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'path' field".into()))?;

        let access_str = params["access"].as_str().unwrap_or("rw");
        let access = ZoneAccess::from_str_loose(access_str)?;

        // SECURITY: reject path traversal before any filesystem access.
        let path = Path::new(path_str);
        if !path.is_absolute() {
            return Err(AgentOSError::SchemaValidation(
                "zone path must be absolute".into(),
            ));
        }
        if path_str.contains("..") {
            return Err(AgentOSError::PermissionDenied {
                resource: "storage.zone".into(),
                operation: "path traversal ('..') not allowed in zone paths".into(),
            });
        }

        // Canonicalize to resolve symlinks (best-effort — path may not exist yet).
        let canonical = tokio::task::spawn_blocking({
            let p = PathBuf::from(path_str);
            move || {
                // Try to canonicalize. If the path doesn't exist, use the raw path.
                p.canonicalize().unwrap_or(p)
            }
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("canonicalization task panicked: {e}"),
        })?;

        let canonical_str = canonical.to_string_lossy();

        // SECURITY: check deny list first (deny > allow, always).
        if self.is_denied(&canonical_str) {
            return Err(AgentOSError::PermissionDenied {
                resource: "storage.zone".into(),
                operation: format!(
                    "path '{}' matches a denied zone pattern and cannot be granted",
                    canonical_str
                ),
            });
        }

        // Check allow list.
        if !self.is_allowed(&canonical_str) {
            return Err(AgentOSError::PermissionDenied {
                resource: "storage.zone".into(),
                operation: format!(
                    "path '{}' does not match any allowed zone pattern; \
                     requires operator approval",
                    canonical_str
                ),
            });
        }

        // Atomically check zone limit, generate ID, and insert — prevents
        // TOCTOU race where concurrent requests both pass the limit check.
        let zone_id = self
            .zone_table
            .insert_if_under_limit(
                context.agent_id,
                canonical.clone(),
                access,
                ZoneGrantSource::Policy,
                self.config.max_zones_per_agent,
            )
            .await?;

        Ok(CapabilityResult {
            output: json!({
                "zone_id": zone_id,
                "path": canonical_str.to_string(),
                "access": access_str,
                "granted_by": "policy",
            }),
            audit_metadata: json!({
                "event": "StorageZoneCreated",
                "zone_id": zone_id,
                "path": canonical_str.to_string(),
                "access": access_str,
                "agent_id": context.agent_id.to_string(),
            }),
        })
    }

    async fn action_zone_list(
        &self,
        _params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let zones = self.zone_table.list_for_agent(&context.agent_id).await;
        let entries: Vec<Value> = zones
            .iter()
            .map(|z| {
                json!({
                    "zone_id": z.zone_id,
                    "path": z.path.to_string_lossy().to_string(),
                    "access": serde_json::to_value(z.access).unwrap_or(Value::Null),
                    "created_at": z.created_at.to_rfc3339(),
                    "expires_at": z.expires_at.map(|e| e.to_rfc3339()),
                })
            })
            .collect();

        Ok(CapabilityResult {
            output: json!({
                "zones": entries,
                "count": entries.len(),
            }),
            audit_metadata: json!({
                "action": "zone.list",
                "count": entries.len(),
            }),
        })
    }

    async fn action_zone_revoke(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let zone_id = params["zone_id"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'zone_id' field".into()))?;

        let removed = self
            .zone_table
            .remove(zone_id, &context.agent_id)
            .await
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("zone '{zone_id}' not found or not owned by this agent"),
            })?;

        Ok(CapabilityResult {
            output: json!({
                "revoked": zone_id,
                "path": removed.path.to_string_lossy().to_string(),
            }),
            audit_metadata: json!({
                "event": "StorageZoneRevoked",
                "zone_id": zone_id,
                "path": removed.path.to_string_lossy().to_string(),
                "agent_id": context.agent_id.to_string(),
            }),
        })
    }
}

#[async_trait]
impl CapabilityProvider for StorageProvider {
    fn domain(&self) -> &str {
        "storage"
    }

    fn supported_actions(&self) -> &[&str] {
        &["zone.create", "zone.list", "zone.revoke"]
    }

    fn required_permissions(&self, action: &str) -> Option<Vec<(String, PermissionOp)>> {
        match action {
            "zone.create" => Some(vec![(
                "storage.zone.create".to_string(),
                PermissionOp::Execute,
            )]),
            "zone.list" => Some(vec![("storage.zone.list".to_string(), PermissionOp::Read)]),
            "zone.revoke" => Some(vec![(
                "storage.zone.revoke".to_string(),
                PermissionOp::Execute,
            )]),
            _ => None,
        }
    }

    async fn execute(
        &self,
        action: &str,
        params: Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        match action {
            "zone.create" => self.action_zone_create(&params, context).await,
            "zone.list" => self.action_zone_list(&params, context).await,
            "zone.revoke" => self.action_zone_revoke(&params, context).await,
            other => Err(AgentOSError::KernelError {
                reason: format!("unknown storage action '{other}'"),
            }),
        }
    }

    fn description(&self) -> &str {
        "Manage filesystem access zones for project directories"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, TaskID, TraceID};

    fn make_config() -> StorageConfig {
        StorageConfig {
            allowed_zone_patterns: vec!["/home/*/projects/**".into(), "/tmp/agentos-*/**".into()],
            denied_zone_patterns: vec![
                "/etc/**".into(),
                "/root/**".into(),
                "/home/*/.ssh/**".into(),
            ],
            max_zones_per_agent: 3,
        }
    }

    fn make_provider() -> StorageProvider {
        StorageProvider::new(make_config(), ZoneTable::new())
    }

    fn make_context() -> CapabilityContext {
        CapabilityContext {
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
            data_dir: PathBuf::from("/tmp/test-data"),
            permissions: agentos_types::PermissionSet::default(),
            workspace_paths: vec![],
        }
    }

    // -- Glob matching tests --

    #[test]
    fn glob_exact_match() {
        assert!(glob_matches("/home/user/projects", "/home/user/projects"));
    }

    #[test]
    fn glob_star_matches_single_component() {
        assert!(glob_matches("/home/*/projects", "/home/user/projects"));
        assert!(glob_matches("/home/*/projects", "/home/admin/projects"));
        assert!(!glob_matches("/home/*/projects", "/home/user/other"));
    }

    #[test]
    fn glob_doublestar_matches_any_depth() {
        assert!(glob_matches("/etc/**", "/etc/passwd"));
        assert!(glob_matches("/etc/**", "/etc/nginx/nginx.conf"));
        assert!(glob_matches("/etc/**", "/etc"));
        assert!(!glob_matches("/etc/**", "/var/log"));
    }

    #[test]
    fn glob_complex_pattern() {
        assert!(glob_matches(
            "/home/*/projects/**",
            "/home/user/projects/myapp/src/main.rs"
        ));
        assert!(!glob_matches(
            "/home/*/projects/**",
            "/home/user/.ssh/id_rsa"
        ));
    }

    #[test]
    fn glob_prefix_wildcard() {
        assert!(glob_matches(
            "/tmp/agentos-*/**",
            "/tmp/agentos-abc123/data"
        ));
        assert!(!glob_matches("/tmp/agentos-*/**", "/tmp/other/data"));
    }

    #[test]
    fn component_match_tests() {
        assert!(component_matches("*", "anything"));
        assert!(component_matches("agentos-*", "agentos-abc123"));
        assert!(!component_matches("agentos-*", "other-abc123"));
        assert!(component_matches("file.*", "file.txt"));
        assert!(!component_matches("file.*", "other.txt"));
    }

    // -- Provider metadata tests --

    #[test]
    fn provider_metadata() {
        let p = make_provider();
        assert_eq!(p.domain(), "storage");
        assert_eq!(
            p.supported_actions(),
            &["zone.create", "zone.list", "zone.revoke"]
        );
        assert!(p.required_permissions("zone.create").is_some());
        assert!(p.required_permissions("zone.list").is_some());
        assert!(p.required_permissions("zone.revoke").is_some());
        assert!(p.required_permissions("unknown").is_none());
    }

    // -- Policy tests --

    #[test]
    fn denied_paths_detected() {
        let p = make_provider();
        assert!(p.is_denied("/etc/passwd"));
        assert!(p.is_denied("/root/.bashrc"));
        assert!(p.is_denied("/home/user/.ssh/id_rsa"));
        assert!(!p.is_denied("/home/user/projects/myapp"));
    }

    #[test]
    fn allowed_paths_detected() {
        let p = make_provider();
        assert!(p.is_allowed("/home/user/projects/myapp"));
        assert!(p.is_allowed("/home/user/projects/myapp/src/main.rs"));
        assert!(p.is_allowed("/tmp/agentos-abc123/data"));
        assert!(!p.is_allowed("/var/log/syslog"));
        assert!(!p.is_allowed("/home/user/.config/secret"));
    }

    // -- Action tests --

    #[tokio::test]
    async fn create_zone_for_allowed_path() {
        let p = make_provider();
        let ctx = make_context();

        let result = p
            .execute(
                "zone.create",
                json!({"path": "/home/user/projects/myapp", "access": "rw"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.output["zone_id"].is_string());
        assert_eq!(result.output["granted_by"], "policy");
    }

    #[tokio::test]
    async fn deny_zone_for_denied_path() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "zone.create",
                json!({"path": "/etc/passwd", "access": "ro"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("denied zone pattern"));
    }

    #[tokio::test]
    async fn deny_zone_for_ssh_dir() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "zone.create",
                json!({"path": "/home/user/.ssh/id_rsa", "access": "ro"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("denied zone pattern"));
    }

    #[tokio::test]
    async fn deny_zone_for_unmatched_path() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "zone.create",
                json!({"path": "/var/log/syslog", "access": "ro"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("does not match any allowed zone pattern"));
    }

    #[tokio::test]
    async fn reject_relative_path() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "zone.create",
                json!({"path": "relative/path", "access": "rw"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("must be absolute"));
    }

    #[tokio::test]
    async fn reject_path_traversal() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "zone.create",
                json!({"path": "/home/user/projects/../.ssh/id_rsa", "access": "ro"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("path traversal"));
    }

    #[tokio::test]
    async fn max_zones_per_agent_enforced() {
        let p = make_provider();
        let ctx = make_context();

        for i in 0..3 {
            p.execute(
                "zone.create",
                json!({"path": format!("/home/user/projects/app{i}"), "access": "rw"}),
                &ctx,
            )
            .await
            .unwrap();
        }

        let err = p
            .execute(
                "zone.create",
                json!({"path": "/home/user/projects/app4", "access": "rw"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("maximum of 3 active storage zones"));
    }

    #[tokio::test]
    async fn list_zones() {
        let p = make_provider();
        let ctx = make_context();

        p.execute(
            "zone.create",
            json!({"path": "/home/user/projects/app1", "access": "rw"}),
            &ctx,
        )
        .await
        .unwrap();

        let result = p.execute("zone.list", json!({}), &ctx).await.unwrap();

        assert_eq!(result.output["count"], 1);
        let zones = result.output["zones"].as_array().unwrap();
        assert_eq!(zones.len(), 1);
    }

    #[tokio::test]
    async fn revoke_zone() {
        let p = make_provider();
        let ctx = make_context();

        let create_result = p
            .execute(
                "zone.create",
                json!({"path": "/home/user/projects/app1", "access": "rw"}),
                &ctx,
            )
            .await
            .unwrap();

        let zone_id = create_result.output["zone_id"].as_str().unwrap();

        p.execute("zone.revoke", json!({"zone_id": zone_id}), &ctx)
            .await
            .unwrap();

        // Zone should be gone
        let list_result = p.execute("zone.list", json!({}), &ctx).await.unwrap();
        assert_eq!(list_result.output["count"], 0);
    }

    #[tokio::test]
    async fn revoke_nonexistent_zone_fails() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute("zone.revoke", json!({"zone_id": "zone-999"}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not found"));
    }

    #[tokio::test]
    async fn agent_isolation() {
        let p = make_provider();
        let ctx_a = make_context();
        let ctx_b = CapabilityContext {
            agent_id: AgentID::new(),
            ..make_context()
        };

        let result = p
            .execute(
                "zone.create",
                json!({"path": "/home/user/projects/shared", "access": "rw"}),
                &ctx_a,
            )
            .await
            .unwrap();

        let zone_id = result.output["zone_id"].as_str().unwrap();

        // Agent B can't see Agent A's zones
        let list_b = p.execute("zone.list", json!({}), &ctx_b).await.unwrap();
        assert_eq!(list_b.output["count"], 0);

        // Agent B can't revoke Agent A's zones
        let err = p
            .execute("zone.revoke", json!({"zone_id": zone_id}), &ctx_b)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not found"));
    }

    // -- Zone table tests --

    #[tokio::test]
    async fn zone_table_path_check() {
        let table = ZoneTable::new();
        let agent_id = AgentID::new();

        table
            .insert(StorageZone {
                zone_id: "z1".into(),
                agent_id,
                path: PathBuf::from("/home/user/projects/myapp"),
                access: ZoneAccess::ReadWrite,
                created_at: chrono::Utc::now(),
                expires_at: None,
                granted_by: ZoneGrantSource::Policy,
            })
            .await;

        assert!(
            table
                .is_path_in_zone(
                    &agent_id,
                    Path::new("/home/user/projects/myapp/src/main.rs")
                )
                .await
        );
        assert!(
            !table
                .is_path_in_zone(&agent_id, Path::new("/home/user/.ssh/id_rsa"))
                .await
        );

        // Different agent can't use this zone
        let other = AgentID::new();
        assert!(
            !table
                .is_path_in_zone(&other, Path::new("/home/user/projects/myapp/src/main.rs"))
                .await
        );
    }

    #[tokio::test]
    async fn zone_table_expiry() {
        let table = ZoneTable::new();
        let agent_id = AgentID::new();

        table
            .insert(StorageZone {
                zone_id: "z-expired".into(),
                agent_id,
                path: PathBuf::from("/home/user/projects/old"),
                access: ZoneAccess::ReadOnly,
                created_at: chrono::Utc::now() - chrono::Duration::hours(2),
                expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                granted_by: ZoneGrantSource::Policy,
            })
            .await;

        // Expired zone should not grant access
        assert!(
            !table
                .is_path_in_zone(&agent_id, Path::new("/home/user/projects/old/file.txt"))
                .await
        );
    }

    #[tokio::test]
    async fn sweep_expired_zones() {
        let table = ZoneTable::new();
        let agent_id = AgentID::new();

        table
            .insert(StorageZone {
                zone_id: "active".into(),
                agent_id,
                path: PathBuf::from("/home/user/projects/active"),
                access: ZoneAccess::ReadWrite,
                created_at: chrono::Utc::now(),
                expires_at: None,
                granted_by: ZoneGrantSource::Policy,
            })
            .await;

        table
            .insert(StorageZone {
                zone_id: "expired".into(),
                agent_id,
                path: PathBuf::from("/home/user/projects/expired"),
                access: ZoneAccess::ReadOnly,
                created_at: chrono::Utc::now() - chrono::Duration::hours(2),
                expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                granted_by: ZoneGrantSource::Policy,
            })
            .await;

        let swept = table.sweep_expired().await;
        assert_eq!(swept, 1);
        assert_eq!(table.count_for_agent(&agent_id).await, 1);
    }

    #[tokio::test]
    async fn unknown_action_fails() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute("zone.snapshot", json!({}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("unknown storage action"));
    }
}
