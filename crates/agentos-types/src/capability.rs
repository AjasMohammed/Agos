use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// An unforgeable, scoped, kernel-signed token issued to every task.
/// All tool invocations are checked against this token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub allowed_tools: BTreeSet<ToolID>,
    pub allowed_intents: BTreeSet<IntentTypeFlag>,
    pub permissions: PermissionSet,
    pub issued_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// HMAC-SHA256 signature computed by the kernel. Cannot be forged.
    pub signature: Vec<u8>,
}

/// Mirrors IntentType but used in capability tokens for efficient set membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IntentTypeFlag {
    Read,
    Write,
    Execute,
    Query,
    Observe,
    Delegate,
    Message,
    Broadcast,
    Escalate,
}

/// A set of resource permissions in rwx format.
///
/// Supports both exact matches and path-prefix matching:
/// - `"fs.user_data"` matches exactly `"fs.user_data"`
/// - `"fs:/home/user/"` matches `"fs:/home/user/docs/file.txt"` (prefix)
///
/// Deny entries take precedence over grants (Spec §2: deny lists like `~/.ssh/`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionSet {
    pub entries: Vec<PermissionEntry>,
    /// Deny entries — checked before grants, take absolute precedence.
    /// Supports exact and prefix matching (e.g. `"fs:~/.ssh/"` blocks all
    /// paths under `~/.ssh/`).
    #[serde(default)]
    pub deny_entries: Vec<String>,
}

/// A single permission entry: resource + rwx bits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionEntry {
    /// Resource class, e.g. "fs.user_data", "network.outbound", "memory.semantic"
    pub resource: String,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Well-known private/reserved IP ranges that agents must never connect to
/// (Spec §2: `deny_private_ranges: true` prevents SSRF to 192.168.x.x, 10.x.x.x, etc.).
const PRIVATE_NETWORK_PREFIXES: &[&str] = &[
    "10.",
    "172.16.",
    "172.17.",
    "172.18.",
    "172.19.",
    "172.20.",
    "172.21.",
    "172.22.",
    "172.23.",
    "172.24.",
    "172.25.",
    "172.26.",
    "172.27.",
    "172.28.",
    "172.29.",
    "172.30.",
    "172.31.",
    "192.168.",
    "127.",
    "169.254.",
    "0.",
    "fd",    // IPv6 ULA
    "fe80:", // IPv6 link-local
    "::1",   // IPv6 loopback
    "localhost",
];

impl PermissionSet {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            deny_entries: Vec::new(),
        }
    }

    pub fn entries(&self) -> &[PermissionEntry] {
        &self.entries
    }

    /// Add a deny rule. Deny entries take precedence over any grant.
    pub fn deny(&mut self, resource_pattern: String) {
        if !self.deny_entries.contains(&resource_pattern) {
            self.deny_entries.push(resource_pattern);
        }
    }

    /// Check if a resource is explicitly denied.
    pub fn is_denied(&self, resource: &str) -> bool {
        // Check explicit deny list
        for pattern in &self.deny_entries {
            if resource == pattern || resource.starts_with(pattern) {
                return true;
            }
        }

        // SSRF protection: block private network ranges for network resources
        if resource.starts_with("net:") || resource.starts_with("network:") {
            let target = resource
                .strip_prefix("net:")
                .or_else(|| resource.strip_prefix("network:"))
                .unwrap_or("");
            // Strip protocol prefix if present (e.g. "https://10.0.0.1/...")
            let host = target
                .strip_prefix("https://")
                .or_else(|| target.strip_prefix("http://"))
                .unwrap_or(target);
            for prefix in PRIVATE_NETWORK_PREFIXES {
                if host.starts_with(prefix) {
                    return true;
                }
            }
        }

        false
    }

    /// Check if a specific operation on a resource is allowed.
    ///
    /// Uses path-prefix matching: a grant on `"fs:/home/user/"` allows
    /// operations on `"fs:/home/user/docs/file.txt"`.
    /// Deny entries are checked first and take absolute precedence.
    pub fn check(&self, resource: &str, operation: PermissionOp) -> bool {
        // Deny entries take precedence
        if self.is_denied(resource) {
            return false;
        }

        self.entries.iter().any(|e| {
            // Exact match or prefix match (grant on "fs:/home/" covers "fs:/home/user/x.txt")
            let resource_matches = e.resource == resource || resource.starts_with(&e.resource);

            resource_matches
                && match operation {
                    PermissionOp::Read => e.read,
                    PermissionOp::Write => e.write,
                    PermissionOp::Execute => e.execute,
                }
        })
    }

    pub fn grant(
        &mut self,
        resource: String,
        read: bool,
        write: bool,
        execute: bool,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        // Upsert: if resource exists, update bits; otherwise add new entry
        if let Some(entry) = self.entries.iter_mut().find(|e| e.resource == resource) {
            entry.read |= read;
            entry.write |= write;
            entry.execute |= execute;
            // Update expiry: keep the one that expires later, or None if either has no expiry
            entry.expires_at = match (entry.expires_at, expires_at) {
                (Some(e1), Some(e2)) => Some(e1.max(e2)),
                _ => None,
            };
        } else {
            self.entries.push(PermissionEntry {
                resource,
                read,
                write,
                execute,
                expires_at,
            });
        }
    }

    pub fn revoke(&mut self, resource: &str, read: bool, write: bool, execute: bool) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.resource == resource) {
            if read {
                entry.read = false;
            }
            if write {
                entry.write = false;
            }
            if execute {
                entry.execute = false;
            }
        }
    }

    pub fn intersect(&self, other: &PermissionSet) -> Self {
        let mut intersected = Self::new();
        for e in &self.entries {
            if let Some(other_e) = other.entries.iter().find(|o| o.resource == e.resource) {
                let r = e.read && other_e.read;
                let w = e.write && other_e.write;
                let x = e.execute && other_e.execute;
                if r || w || x {
                    // Intersection of expires_at: keep the one that expires earlier
                    let expires_at = match (e.expires_at, other_e.expires_at) {
                        (Some(e1), Some(e2)) => Some(e1.min(e2)),
                        (Some(e1), None) => Some(e1),
                        (None, Some(e2)) => Some(e2),
                        (None, None) => None,
                    };
                    intersected.entries.push(PermissionEntry {
                        resource: e.resource.clone(),
                        read: r,
                        write: w,
                        execute: x,
                        expires_at,
                    });
                }
            }
        }
        intersected
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOp {
    Read,
    Write,
    Execute,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_set_check() {
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".into(), true, false, false, None);
        perms.grant("network.outbound".into(), false, false, true, None);

        assert!(perms.check("fs.user_data", PermissionOp::Read));
        assert!(!perms.check("fs.user_data", PermissionOp::Write));
        assert!(perms.check("network.outbound", PermissionOp::Execute));
        assert!(!perms.check("network.outbound", PermissionOp::Read));
        assert!(!perms.check("nonexistent.resource", PermissionOp::Read));
    }

    #[test]
    fn test_path_prefix_matching() {
        let mut perms = PermissionSet::new();
        perms.grant("fs:/home/user/".into(), true, true, false, None);

        // Prefix match: /home/user/docs/file.txt is under /home/user/
        assert!(perms.check("fs:/home/user/docs/file.txt", PermissionOp::Read));
        assert!(perms.check("fs:/home/user/docs/file.txt", PermissionOp::Write));
        // Does not match other paths
        assert!(!perms.check("fs:/etc/passwd", PermissionOp::Read));
    }

    #[test]
    fn test_deny_overrides_grant() {
        let mut perms = PermissionSet::new();
        perms.grant("fs:/home/user/".into(), true, true, false, None);
        perms.deny("fs:/home/user/.ssh/".into());

        // Grant covers /home/user/docs
        assert!(perms.check("fs:/home/user/docs/report.md", PermissionOp::Read));
        // Deny overrides for .ssh
        assert!(!perms.check("fs:/home/user/.ssh/id_rsa", PermissionOp::Read));
        assert!(!perms.check("fs:/home/user/.ssh/", PermissionOp::Write));
    }

    #[test]
    fn test_ssrf_protection_blocks_private_ranges() {
        let mut perms = PermissionSet::new();
        perms.grant("net:".into(), true, false, true, None);

        // Public addresses should be allowed
        assert!(perms.check(
            "net:https://api.anthropic.com/v1/messages",
            PermissionOp::Read
        ));
        // Private ranges should be blocked (SSRF protection)
        assert!(!perms.check("net:https://192.168.1.1/admin", PermissionOp::Read));
        assert!(!perms.check("net:http://10.0.0.1/internal", PermissionOp::Read));
        assert!(!perms.check("net:http://127.0.0.1:8080/", PermissionOp::Execute));
        assert!(!perms.check("net:http://169.254.169.254/metadata", PermissionOp::Read));
        assert!(!perms.check("net:localhost:3000", PermissionOp::Read));
        assert!(!perms.check("network:http://172.16.0.1/", PermissionOp::Read));
    }

    #[test]
    fn test_deny_list_serialization() {
        let mut perms = PermissionSet::new();
        perms.grant("fs:/tmp/".into(), true, true, false, None);
        perms.deny("fs:/etc/".into());
        perms.deny("fs:~/.env".into());

        assert_eq!(perms.deny_entries.len(), 2);
        assert!(perms.is_denied("fs:/etc/passwd"));
        assert!(perms.is_denied("fs:~/.env"));
        assert!(!perms.is_denied("fs:/tmp/data.txt"));
    }
}
