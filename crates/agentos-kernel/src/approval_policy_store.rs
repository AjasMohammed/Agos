//! SQLite-backed persistence for learned "allow always" approval policy
//! entries. These augment the legacy in-memory [`crate::hooks::AutoApprovePolicy`]
//! with operator-curated overrides that survive kernel restart.
//!
//! Each entry says: "for this tool (optionally scoped to one agent, optionally
//! gated on a payload `path` glob), automatically lift `Prompt → Allow` even
//! when the global approval mode would otherwise escalate."
//!
//! Schema mirrors [`crate::workspace_grant_store`] for consistency: soft-delete
//! via `revoked_at`, partial-unique index on the live set, lexical normalization
//! of the path glob at insert time.

use agentos_types::{AgentID, AgentOSError};
use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

const LATEST_MIGRATION_VERSION: i64 = 1;

/// Sentinel resource value on `PermissionDenied` for duplicate policy entries.
pub const POLICY_DUPLICATE_RESOURCE: &str = "approval.policy.duplicate";

/// A persisted "allow always" override.
///
/// Matching semantics:
/// - `tool_name` is required and matched exactly.
/// - `path_glob` is optional; when set, the payload's `path` field (if any)
///   must match the glob.
/// - `agent_id == None` means the entry applies to every agent.
///
/// If `expires_at` is in the past the entry is treated as inactive (the
/// background `TimeoutChecker` sweep also revokes it permanently).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPolicyEntry {
    pub id: i64,
    pub tool_name: String,
    pub path_glob: Option<String>,
    pub agent_id: Option<AgentID>,
    pub granted_at: DateTime<Utc>,
    pub granted_by: String,
    pub source: String,
    pub expires_at: Option<DateTime<Utc>>,
}

pub struct ApprovalPolicyStore {
    conn: Arc<Mutex<Connection>>,
}

impl ApprovalPolicyStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for approval policy DB: {}",
                        parent.display()
                    )
                })?;
            }
            let conn = Connection::open(&path_for_open).with_context(|| {
                format!(
                    "Failed to open approval policy DB at {}",
                    path_for_open.display()
                )
            })?;
            Self::configure_connection(&conn)?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("Approval policy DB open task failed")??;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);",
        )?;
        let current: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if current >= LATEST_MIGRATION_VERSION {
            return Ok(());
        }
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS approval_policy_entries (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                tool_name   TEXT    NOT NULL,
                path_glob   TEXT,
                agent_id    TEXT,
                granted_at  TEXT    NOT NULL,
                granted_by  TEXT    NOT NULL DEFAULT 'local-cli',
                source      TEXT    NOT NULL DEFAULT 'cli',
                expires_at  TEXT,
                revoked_at  TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_policy_tool ON approval_policy_entries(tool_name);
            CREATE INDEX IF NOT EXISTS idx_policy_agent ON approval_policy_entries(agent_id);
            CREATE INDEX IF NOT EXISTS idx_policy_active ON approval_policy_entries(revoked_at);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_policy_unique_active
                ON approval_policy_entries(
                    tool_name,
                    COALESCE(path_glob, ''),
                    COALESCE(agent_id, '')
                )
                WHERE revoked_at IS NULL;
            "#,
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_version(version) VALUES (?1)",
            params![LATEST_MIGRATION_VERSION],
        )?;
        Ok(())
    }

    pub fn add(
        &self,
        tool_name: &str,
        path_glob: Option<&str>,
        agent_id: Option<AgentID>,
        granted_by: &str,
        source: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<ApprovalPolicyEntry, AgentOSError> {
        if tool_name.is_empty() {
            return Err(AgentOSError::PermissionDenied {
                resource: "approval.policy".into(),
                operation: "tool_name must not be empty".into(),
            });
        }
        let now = Utc::now();
        let guard = self
            .conn
            .lock()
            .map_err(|_| AgentOSError::StorageError("approval policy DB mutex poisoned".into()))?;
        let agent_str = agent_id.as_ref().map(|a| a.to_string());
        let result = guard.execute(
            "INSERT INTO approval_policy_entries
                (tool_name, path_glob, agent_id, granted_at, granted_by, source, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                tool_name,
                path_glob,
                agent_str,
                now.to_rfc3339(),
                granted_by,
                source,
                expires_at.map(|d| d.to_rfc3339()),
            ],
        );
        match result {
            Ok(_) => {
                let id = guard.last_insert_rowid();
                drop(guard);
                Ok(ApprovalPolicyEntry {
                    id,
                    tool_name: tool_name.to_string(),
                    path_glob: path_glob.map(str::to_string),
                    agent_id,
                    granted_at: now,
                    granted_by: granted_by.to_string(),
                    source: source.to_string(),
                    expires_at,
                })
            }
            Err(e) => {
                if let rusqlite::Error::SqliteFailure(err, _) = &e {
                    if err.code == rusqlite::ErrorCode::ConstraintViolation {
                        return Err(AgentOSError::PermissionDenied {
                            resource: POLICY_DUPLICATE_RESOURCE.into(),
                            operation: format!(
                                "approval policy already exists for tool '{}' \
                                 (path_glob, agent_id) scope",
                                tool_name
                            ),
                        });
                    }
                }
                Err(AgentOSError::StorageError(format!(
                    "approval policy insert failed: {}",
                    e
                )))
            }
        }
    }

    pub fn revoke(&self, id: i64) -> Result<bool, AgentOSError> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| AgentOSError::StorageError("approval policy DB mutex poisoned".into()))?;
        let n = guard
            .execute(
                "UPDATE approval_policy_entries SET revoked_at = ?1
                 WHERE id = ?2 AND revoked_at IS NULL",
                params![Utc::now().to_rfc3339(), id],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("approval policy revoke failed: {}", e))
            })?;
        Ok(n > 0)
    }

    pub fn list_active(&self) -> Result<Vec<ApprovalPolicyEntry>, AgentOSError> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| AgentOSError::StorageError("approval policy DB mutex poisoned".into()))?;
        let mut stmt = guard
            .prepare(
                "SELECT id, tool_name, path_glob, agent_id, granted_at, granted_by, source, expires_at
                 FROM approval_policy_entries
                 WHERE revoked_at IS NULL
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentOSError::StorageError(format!("approval policy list prepare: {}", e)))?;
        let rows = stmt.query_map([], row_to_entry).map_err(|e| {
            AgentOSError::StorageError(format!("approval policy list query: {}", e))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(
                r.map_err(|e| AgentOSError::StorageError(format!("approval policy row: {}", e)))?,
            );
        }
        Ok(out)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApprovalPolicyEntry> {
    let id: i64 = row.get(0)?;
    let tool_name: String = row.get(1)?;
    let path_glob: Option<String> = row.get(2)?;
    let agent_str: Option<String> = row.get(3)?;
    let granted_at: String = row.get(4)?;
    let granted_by: String = row.get(5)?;
    let source: String = row.get(6)?;
    let expires_at: Option<String> = row.get(7)?;
    let parse_dt = |s: &str| {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
    };
    let granted_at = parse_dt(&granted_at)?;
    let expires_at = match expires_at {
        Some(s) => Some(parse_dt(&s)?),
        None => None,
    };
    Ok(ApprovalPolicyEntry {
        id,
        tool_name,
        path_glob,
        agent_id: agent_str.and_then(|s| s.parse::<AgentID>().ok()),
        granted_at,
        granted_by,
        source,
        expires_at,
    })
}

/// In-memory matcher built from the persisted [`ApprovalPolicyStore`].
///
/// Consulted by [`crate::hooks::ApprovalHook`] when the mode-driven decision
/// is `Prompt`: if any active entry matches the call, the prompt is lifted
/// to `Allow`. Matching is fail-safe — on cache poison or matcher error the
/// caller proceeds to the normal escalation path (no silent auto-approve).
pub struct ApprovalPolicyMatcher {
    store: Arc<ApprovalPolicyStore>,
    cache: RwLock<Vec<ApprovalPolicyEntry>>,
}

impl ApprovalPolicyMatcher {
    pub fn load(store: Arc<ApprovalPolicyStore>) -> Result<Self, AgentOSError> {
        let initial = store.list_active()?;
        Ok(Self {
            store,
            cache: RwLock::new(initial),
        })
    }

    /// Does any active policy entry lift this call's prompt to allow?
    /// `path_in_payload` is the `path` field extracted from the tool payload
    /// by the caller (None if the tool has no path field).
    ///
    /// Iterates under the cache read guard with no clone — the prompt branch
    /// is the hot path inside `ApprovalHook::on_event` and we want to keep
    /// per-call allocation off it.
    pub fn allows(
        &self,
        tool_name: &str,
        agent_id: &AgentID,
        path_in_payload: Option<&str>,
    ) -> bool {
        let cache = match self.cache.read() {
            Ok(g) => g,
            Err(_) => {
                tracing::error!(
                    "approval policy matcher cache poisoned; failing safe (no auto-approve)"
                );
                return false;
            }
        };
        let now = Utc::now();
        for entry in cache.iter() {
            if entry.tool_name != tool_name {
                continue;
            }
            if let Some(expiry) = entry.expires_at {
                if expiry < now {
                    continue;
                }
            }
            if let Some(scope) = &entry.agent_id {
                if scope != agent_id {
                    continue;
                }
            }
            if let Some(glob) = &entry.path_glob {
                let payload_path = match path_in_payload {
                    Some(p) => p,
                    None => continue, // glob requires a path; none provided
                };
                if !glob_match(glob, payload_path) {
                    continue;
                }
            }
            return true;
        }
        false
    }

    pub fn add(
        &self,
        tool_name: &str,
        path_glob: Option<&str>,
        agent_id: Option<AgentID>,
        granted_by: &str,
        source: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<ApprovalPolicyEntry, AgentOSError> {
        let mut cache = self
            .cache
            .write()
            .map_err(|_| AgentOSError::StorageError("approval policy matcher poisoned".into()))?;
        let entry = self.store.add(
            tool_name, path_glob, agent_id, granted_by, source, expires_at,
        )?;
        cache.push(entry.clone());
        Ok(entry)
    }

    pub fn revoke(&self, id: i64) -> Result<bool, AgentOSError> {
        let mut cache = self
            .cache
            .write()
            .map_err(|_| AgentOSError::StorageError("approval policy matcher poisoned".into()))?;
        let ok = self.store.revoke(id)?;
        if ok {
            cache.retain(|e| e.id != id);
        }
        Ok(ok)
    }

    /// Snapshot of every active entry. Returns an error on cache poison so
    /// operator-facing callers (CLI list, web view) can surface a "policy
    /// system unhealthy" message rather than silently report "no policies".
    pub fn list_all(&self) -> Result<Vec<ApprovalPolicyEntry>, AgentOSError> {
        match self.cache.read() {
            Ok(g) => Ok(g.clone()),
            Err(_) => Err(AgentOSError::StorageError(
                "approval policy matcher cache poisoned".into(),
            )),
        }
    }
}

/// Match a payload path against an entry path_glob. Supports `*` (any
/// non-separator chars) and `**` (any chars including separators). Anchored
/// match (no implicit prefix/suffix).
fn glob_match(pattern: &str, candidate: &str) -> bool {
    // Quick paths for common cases — exact match and prefix `dir/**` are
    // the lion's share of operator-authored globs.
    if pattern == candidate {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return candidate == prefix || candidate.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        // pattern like `/tmp/*` — single-segment wildcard
        if !prefix.contains('*') {
            if let Some(rest) = candidate.strip_prefix(prefix) {
                return !rest.contains('/');
            }
            return false;
        }
    }
    // Fall back to a small NFA-style matcher for arbitrary `*` / `**` mixes.
    glob_match_impl(pattern.as_bytes(), candidate.as_bytes())
}

fn glob_match_impl(pat: &[u8], cand: &[u8]) -> bool {
    fn rec(pat: &[u8], cand: &[u8]) -> bool {
        if pat.is_empty() {
            return cand.is_empty();
        }
        // `**` — zero or more of any char.
        if pat.starts_with(b"**") {
            let rest = &pat[2..];
            // Strip optional `/` immediately after the `**`.
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            for i in 0..=cand.len() {
                if rec(rest, &cand[i..]) {
                    return true;
                }
            }
            return false;
        }
        // `*` — zero or more non-`/` chars.
        if pat[0] == b'*' {
            for i in 0..=cand.len() {
                if cand[..i].contains(&b'/') {
                    break;
                }
                if rec(&pat[1..], &cand[i..]) {
                    return true;
                }
            }
            return false;
        }
        if cand.is_empty() {
            return false;
        }
        if pat[0] == cand[0] {
            return rec(&pat[1..], &cand[1..]);
        }
        false
    }
    rec(pat, cand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn fresh() -> (TempDir, Arc<ApprovalPolicyStore>) {
        let d = TempDir::new().unwrap();
        let s = ApprovalPolicyStore::open(d.path().join("policy.db"))
            .await
            .unwrap();
        (d, Arc::new(s))
    }

    #[tokio::test]
    async fn add_list_revoke() {
        let (_t, store) = fresh().await;
        let e = store
            .add(
                "file-writer",
                Some("/home/alice/proj/**"),
                None,
                "tester",
                "test",
                None,
            )
            .unwrap();
        let list = store.list_active().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].tool_name, "file-writer");
        assert!(store.revoke(e.id).unwrap());
        assert!(store.list_active().unwrap().is_empty());
        // Revoke of already-revoked entry returns false (idempotent).
        assert!(!store.revoke(e.id).unwrap());
    }

    #[tokio::test]
    async fn duplicate_returns_sentinel_resource() {
        let (_t, store) = fresh().await;
        store
            .add("file-writer", None, None, "t", "test", None)
            .unwrap();
        let err = store
            .add("file-writer", None, None, "t", "test", None)
            .unwrap_err();
        match err {
            AgentOSError::PermissionDenied { resource, .. } => {
                assert_eq!(resource, POLICY_DUPLICATE_RESOURCE);
            }
            other => panic!("expected POLICY_DUPLICATE_RESOURCE, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn matcher_allows_with_path_glob() {
        let (_t, store) = fresh().await;
        let m = ApprovalPolicyMatcher::load(store.clone()).unwrap();
        m.add(
            "file-writer",
            Some("/home/alice/proj/**"),
            None,
            "tester",
            "test",
            None,
        )
        .unwrap();
        let agent = AgentID::new();
        assert!(m.allows("file-writer", &agent, Some("/home/alice/proj/src/main.rs")));
        assert!(!m.allows("file-writer", &agent, Some("/home/bob/proj/main.rs")));
        // Glob present but no path in payload → no match (fail-safe).
        assert!(!m.allows("file-writer", &agent, None));
        // Wrong tool name → no match.
        assert!(!m.allows("file-reader", &agent, Some("/home/alice/proj/x")));
    }

    #[tokio::test]
    async fn matcher_respects_agent_scope() {
        let (_t, store) = fresh().await;
        let m = ApprovalPolicyMatcher::load(store.clone()).unwrap();
        let scoped = AgentID::new();
        let other = AgentID::new();
        m.add("file-writer", None, Some(scoped), "tester", "test", None)
            .unwrap();
        assert!(m.allows("file-writer", &scoped, None));
        assert!(!m.allows("file-writer", &other, None));
    }

    #[tokio::test]
    async fn matcher_skips_expired_entries() {
        let (_t, store) = fresh().await;
        let m = ApprovalPolicyMatcher::load(store.clone()).unwrap();
        let agent = AgentID::new();
        let yesterday = Utc::now() - chrono::Duration::days(1);
        m.add("file-writer", None, None, "tester", "test", Some(yesterday))
            .unwrap();
        assert!(!m.allows("file-writer", &agent, None));
    }

    #[test]
    fn glob_match_basics() {
        assert!(glob_match("/tmp/*", "/tmp/foo"));
        assert!(!glob_match("/tmp/*", "/tmp/foo/bar"));
        assert!(glob_match("/tmp/**", "/tmp/foo/bar"));
        assert!(glob_match("/tmp/**", "/tmp"));
        assert!(glob_match(
            "/home/alice/proj/**",
            "/home/alice/proj/src/main.rs"
        ));
        assert!(!glob_match("/home/alice/proj/**", "/home/bob/proj/main.rs"));
        assert!(glob_match("*.txt", "foo.txt"));
        assert!(!glob_match("*.txt", "foo/bar.txt"));
    }
}
