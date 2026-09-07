//! User-grantable host filesystem access for agents.
//!
//! A `WorkspaceGrant` lets an operator authorise an agent (or all agents) to
//! read, write, and/or execute commands inside a specific host directory tree.
//! Grants are persisted by the kernel and consulted by file tools and the
//! shell sandbox at execution time.

use crate::AgentID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Permission bits attached to a [`WorkspaceGrant`].
///
/// - `read` allows `file-reader` and similar read-only tools to access the path.
/// - `write` additionally allows `file-writer`, `file-append`, and friends.
/// - `exec` additionally allows `shell-exec` to bind-mount the path inside its
///   sandbox so commands can act on the real on-disk files.
///
/// Persisted as a `u8` (`read=0b001`, `write=0b010`, `exec=0b100`) for compact
/// SQLite storage; deserialised back via [`WorkspaceGrantMode::from_bits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
pub struct WorkspaceGrantMode {
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub exec: bool,
}

impl WorkspaceGrantMode {
    pub const NONE: Self = Self {
        read: false,
        write: false,
        exec: false,
    };
    pub const READ: Self = Self {
        read: true,
        write: false,
        exec: false,
    };
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
        exec: false,
    };
    pub const READ_WRITE_EXEC: Self = Self {
        read: true,
        write: true,
        exec: true,
    };

    pub fn to_bits(self) -> u8 {
        (if self.read { 0b001 } else { 0 })
            | (if self.write { 0b010 } else { 0 })
            | (if self.exec { 0b100 } else { 0 })
    }

    pub fn from_bits(bits: u8) -> Self {
        Self {
            read: bits & 0b001 != 0,
            write: bits & 0b010 != 0,
            exec: bits & 0b100 != 0,
        }
    }

    /// True iff this grant has every bit set in `required`.
    pub fn covers(self, required: WorkspaceGrantMode) -> bool {
        (!required.read || self.read)
            && (!required.write || self.write)
            && (!required.exec || self.exec)
    }

    /// Parse a short mode string like "r", "rw", "rwx" (case-insensitive).
    pub fn parse(s: &str) -> Result<Self, String> {
        if s.is_empty() {
            return Err("empty mode string".into());
        }
        let mut m = Self::default();
        for c in s.chars() {
            match c {
                'r' | 'R' => m.read = true,
                'w' | 'W' => m.write = true,
                'x' | 'X' => m.exec = true,
                other => return Err(format!("invalid mode char '{}' in '{}'", other, s)),
            }
        }
        Ok(m)
    }
}

impl std::fmt::Display for WorkspaceGrantMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.read {
            f.write_str("r")?;
        }
        if self.write {
            f.write_str("w")?;
        }
        if self.exec {
            f.write_str("x")?;
        }
        if !self.read && !self.write && !self.exec {
            f.write_str("-")?;
        }
        Ok(())
    }
}

/// A persisted grant authorising filesystem access to one or all agents.
///
/// `agent_id == None` means the grant applies globally to every agent;
/// `agent_id == Some(_)` scopes it to that agent. Lookups prefer the
/// agent-specific grant over the global one when both exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceGrant {
    pub id: i64,
    pub path: PathBuf,
    pub agent_id: Option<AgentID>,
    pub mode: WorkspaceGrantMode,
    pub granted_at: DateTime<Utc>,
    pub source: String,
    pub granted_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_roundtrip() {
        assert_eq!(
            WorkspaceGrantMode::parse("rw").unwrap(),
            WorkspaceGrantMode::READ_WRITE
        );
        assert_eq!(
            WorkspaceGrantMode::parse("RWX").unwrap(),
            WorkspaceGrantMode::READ_WRITE_EXEC
        );
        assert_eq!(
            WorkspaceGrantMode::parse("r").unwrap(),
            WorkspaceGrantMode::READ
        );
    }

    #[test]
    fn mode_parse_rejects_invalid() {
        assert!(WorkspaceGrantMode::parse("rwa").is_err());
        assert!(WorkspaceGrantMode::parse("").is_err());
    }

    #[test]
    fn mode_bits_roundtrip() {
        for r in [false, true] {
            for w in [false, true] {
                for x in [false, true] {
                    let m = WorkspaceGrantMode {
                        read: r,
                        write: w,
                        exec: x,
                    };
                    assert_eq!(WorkspaceGrantMode::from_bits(m.to_bits()), m);
                }
            }
        }
    }

    #[test]
    fn mode_covers() {
        assert!(WorkspaceGrantMode::READ_WRITE.covers(WorkspaceGrantMode::READ));
        assert!(WorkspaceGrantMode::READ_WRITE_EXEC.covers(WorkspaceGrantMode::READ_WRITE));
        assert!(!WorkspaceGrantMode::READ.covers(WorkspaceGrantMode::READ_WRITE));
        assert!(!WorkspaceGrantMode::READ_WRITE.covers(WorkspaceGrantMode::READ_WRITE_EXEC));
    }

    #[test]
    fn mode_display() {
        assert_eq!(WorkspaceGrantMode::READ.to_string(), "r");
        assert_eq!(WorkspaceGrantMode::READ_WRITE.to_string(), "rw");
        assert_eq!(WorkspaceGrantMode::READ_WRITE_EXEC.to_string(), "rwx");
        assert_eq!(WorkspaceGrantMode::NONE.to_string(), "-");
    }
}
