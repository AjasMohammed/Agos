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
}

/// A set of resource permissions in rwx format.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionSet {
    pub entries: Vec<PermissionEntry>,
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

impl PermissionSet {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn entries(&self) -> &[PermissionEntry] {
        &self.entries
    }

    /// Check if a specific operation on a resource is allowed.
    pub fn check(&self, resource: &str, operation: PermissionOp) -> bool {
        self.entries.iter().any(|e| {
            e.resource == resource && match operation {
                PermissionOp::Read => e.read,
                PermissionOp::Write => e.write,
                PermissionOp::Execute => e.execute,
            }
        })
    }

    pub fn grant(&mut self, resource: String, read: bool, write: bool, execute: bool, expires_at: Option<chrono::DateTime<chrono::Utc>>) {
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
            self.entries.push(PermissionEntry { resource, read, write, execute, expires_at });
        }
    }

    pub fn revoke(&mut self, resource: &str, read: bool, write: bool, execute: bool) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.resource == resource) {
            if read { entry.read = false; }
            if write { entry.write = false; }
            if execute { entry.execute = false; }
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

#[derive(Debug, Clone, Copy)]
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
}
