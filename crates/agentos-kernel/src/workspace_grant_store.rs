//! SQLite-backed persistence and in-memory registry for user filesystem grants.
//!
//! A `WorkspaceGrant` records that a particular host directory tree has been
//! opened to one agent (or to every agent) with a specific permission mode.
//! The store is the durable source of truth; the registry caches active grants
//! in memory for fast lookup on the file-tool hot path.

use crate::config::validate_workspace_path;
use agentos_types::{AgentID, AgentOSError, WorkspaceGrant, WorkspaceGrantMode};
use anyhow::Context;
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

const LATEST_MIGRATION_VERSION: i64 = 1;

/// Sentinel `resource` value on [`AgentOSError::PermissionDenied`] indicating
/// the failure was a "this grant already exists" duplicate rather than a
/// hard policy denial. Callers (kernel boot import, CLI) match on this to
/// treat duplicates as benign.
pub const GRANT_DUPLICATE_RESOURCE: &str = "fs.workspace_grant.duplicate";

/// Lexically normalize a host path for grant storage so cosmetic variants
/// don't create distinct rows: strips trailing separators (except for `/`)
/// and collapses redundant `.` components. Does NOT resolve symlinks — that
/// would require the path to exist on disk and would change semantics for
/// pre-grant-then-create workflows.
fn lexically_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::RootDir => out.push("/"),
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::Normal(part) => out.push(part),
            // `..` is rejected by `validate_workspace_path` upstream; treat
            // defensively here as a pop of the last `Normal` component.
            Component::ParentDir => {
                out.pop();
            }
        }
    }
    out
}

pub struct WorkspaceGrantStore {
    conn: Arc<Mutex<Connection>>,
}

impl WorkspaceGrantStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for workspace grant DB: {}",
                        parent.display()
                    )
                })?;
            }
            let conn = Connection::open(&path_for_open).with_context(|| {
                format!(
                    "Failed to open workspace grant DB at {}",
                    path_for_open.display()
                )
            })?;
            Self::configure_connection(&conn)?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("Workspace grant DB open task failed")??;

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
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            );",
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
            CREATE TABLE IF NOT EXISTS workspace_grants (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                path        TEXT    NOT NULL,
                agent_id    TEXT,
                mode_bits   INTEGER NOT NULL,
                granted_at  TEXT    NOT NULL,
                revoked_at  TEXT,
                source      TEXT    NOT NULL DEFAULT 'cli',
                granted_by  TEXT    NOT NULL DEFAULT 'local-cli'
            );
            CREATE INDEX IF NOT EXISTS idx_wg_agent  ON workspace_grants(agent_id);
            CREATE INDEX IF NOT EXISTS idx_wg_active ON workspace_grants(revoked_at);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_wg_unique_active
                ON workspace_grants(path, COALESCE(agent_id, ''))
                WHERE revoked_at IS NULL;
            "#,
        )?;
        // INSERT OR IGNORE so a corrupted/partial migration row from a prior
        // crash doesn't create a duplicate-version row on the retry path.
        conn.execute(
            "INSERT OR IGNORE INTO schema_version(version) VALUES (?1)",
            params![LATEST_MIGRATION_VERSION],
        )?;
        Ok(())
    }

    /// Insert a new active grant. The path is lexically normalized (trailing
    /// `/` stripped, `.` components collapsed) before storage so cosmetic
    /// variants of the same directory don't yield distinct rows.
    pub fn grant(
        &self,
        path: &Path,
        agent_id: Option<AgentID>,
        mode: WorkspaceGrantMode,
        source: &str,
        granted_by: &str,
    ) -> Result<WorkspaceGrant, AgentOSError> {
        let normalized = lexically_normalize(path);
        let path_str = normalized.to_string_lossy().to_string();
        validate_workspace_path(&path_str).map_err(|e| AgentOSError::PermissionDenied {
            resource: "fs.workspace_grant".into(),
            operation: e.to_string(),
        })?;
        let now = Utc::now();
        let guard = self
            .conn
            .lock()
            .map_err(|_| AgentOSError::StorageError("workspace grant DB mutex poisoned".into()))?;
        let agent_str = agent_id.as_ref().map(|a| a.to_string());
        let _affected = guard
            .execute(
                "INSERT INTO workspace_grants
                    (path, agent_id, mode_bits, granted_at, source, granted_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    path_str,
                    agent_str,
                    mode.to_bits() as i64,
                    now.to_rfc3339(),
                    source,
                    granted_by,
                ],
            )
            .map_err(|e| map_constraint_error(e, &path_str))?;
        let id = guard.last_insert_rowid();
        drop(guard);
        Ok(WorkspaceGrant {
            id,
            path: normalized,
            agent_id,
            mode,
            granted_at: now,
            source: source.to_string(),
            granted_by: granted_by.to_string(),
        })
    }

    /// Mark all active grants matching `(path, agent_id)` as revoked. The
    /// path is lexically normalized the same way as in `grant()` so the
    /// revoke matches regardless of trailing-slash or `.` variants.
    /// Returns the number of rows updated (0 if no active grant matched).
    pub fn revoke(&self, path: &Path, agent_id: Option<&AgentID>) -> Result<u64, AgentOSError> {
        let normalized = lexically_normalize(path);
        let path_str = normalized.to_string_lossy().to_string();
        let agent_str = agent_id.map(|a| a.to_string());
        let now = Utc::now().to_rfc3339();
        let guard = self
            .conn
            .lock()
            .map_err(|_| AgentOSError::StorageError("workspace grant DB mutex poisoned".into()))?;
        let n = match agent_str {
            Some(a) => guard
                .execute(
                    "UPDATE workspace_grants SET revoked_at = ?1
                     WHERE path = ?2 AND agent_id = ?3 AND revoked_at IS NULL",
                    params![now, path_str, a],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("workspace grant revoke failed: {}", e))
                })?,
            None => guard
                .execute(
                    "UPDATE workspace_grants SET revoked_at = ?1
                     WHERE path = ?2 AND agent_id IS NULL AND revoked_at IS NULL",
                    params![now, path_str],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("workspace grant revoke failed: {}", e))
                })?,
        };
        Ok(n as u64)
    }

    /// List every active grant in insertion order.
    pub fn list_active(&self) -> Result<Vec<WorkspaceGrant>, AgentOSError> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| AgentOSError::StorageError("workspace grant DB mutex poisoned".into()))?;
        let mut stmt = guard
            .prepare(
                "SELECT id, path, agent_id, mode_bits, granted_at, source, granted_by
                 FROM workspace_grants
                 WHERE revoked_at IS NULL
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentOSError::StorageError(format!("workspace grant prepare: {}", e)))?;
        let rows = stmt
            .query_map([], row_to_grant)
            .map_err(|e| AgentOSError::StorageError(format!("workspace grant query: {}", e)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(
                r.map_err(|e| AgentOSError::StorageError(format!("workspace grant row: {}", e)))?,
            );
        }
        Ok(out)
    }
}

/// Translate a rusqlite write error to a typed [`AgentOSError`]. UNIQUE
/// constraint violations are mapped to a `PermissionDenied` with the sentinel
/// resource [`GRANT_DUPLICATE_RESOURCE`] so callers can match on the error
/// kind structurally instead of string-grepping the message.
fn map_constraint_error(e: rusqlite::Error, path_str: &str) -> AgentOSError {
    if let rusqlite::Error::SqliteFailure(err, _) = &e {
        if err.code == rusqlite::ErrorCode::ConstraintViolation {
            return AgentOSError::PermissionDenied {
                resource: GRANT_DUPLICATE_RESOURCE.into(),
                operation: format!(
                    "grant already exists for path '{}' and this scope",
                    path_str
                ),
            };
        }
    }
    AgentOSError::StorageError(format!("workspace grant insert failed: {}", e))
}

fn row_to_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceGrant> {
    let id: i64 = row.get(0)?;
    let path: String = row.get(1)?;
    let agent_str: Option<String> = row.get(2)?;
    let mode_bits: i64 = row.get(3)?;
    let granted_at: String = row.get(4)?;
    let source: String = row.get(5)?;
    let granted_by: String = row.get(6)?;
    let agent_id = agent_str.and_then(|s| s.parse::<AgentID>().ok());
    let granted_at = chrono::DateTime::parse_from_rfc3339(&granted_at)
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?
        .with_timezone(&Utc);
    Ok(WorkspaceGrant {
        id,
        path: PathBuf::from(path),
        agent_id,
        mode: WorkspaceGrantMode::from_bits(mode_bits as u8),
        granted_at,
        source,
        granted_by,
    })
}

/// In-memory registry layered on top of [`WorkspaceGrantStore`] for fast
/// path-resolution lookups.
///
/// **Consistency contract:** `grant()` and `revoke()` hold the cache write
/// lock for the duration of the matching store mutation. The store has its
/// own connection mutex, so the lock order is always
/// `cache.write -> store.conn.lock`. Read-side methods (`list_for_agent`,
/// `paths_for_agent`, `list_all_active`) take only the cache read lock and
/// fail-closed on poison (return an empty list and log) so a poisoned
/// writer can't tear down the tool-execution hot path.
pub struct WorkspaceGrantRegistry {
    store: Arc<WorkspaceGrantStore>,
    cache: RwLock<Vec<WorkspaceGrant>>,
}

impl WorkspaceGrantRegistry {
    pub fn load(store: Arc<WorkspaceGrantStore>) -> Result<Self, AgentOSError> {
        let initial = store.list_active()?;
        Ok(Self {
            store,
            cache: RwLock::new(initial),
        })
    }

    /// Read the cache, returning an empty list (and logging) on poison so the
    /// caller's tool execution doesn't panic. Used by every read-side method.
    fn read_snapshot(&self) -> Vec<WorkspaceGrant> {
        match self.cache.read() {
            Ok(g) => g.clone(),
            Err(_) => {
                tracing::error!(
                    "workspace grant cache RwLock poisoned; returning empty snapshot (fail-closed)"
                );
                Vec::new()
            }
        }
    }

    /// List every grant that applies to `agent_id` (agent-scoped + global).
    pub fn list_for_agent(&self, agent_id: &AgentID) -> Vec<WorkspaceGrant> {
        self.read_snapshot()
            .into_iter()
            .filter(|g| g.agent_id.is_none() || g.agent_id.as_ref() == Some(agent_id))
            .collect()
    }

    /// List the path roots that `agent_id` may use at least at `required` mode.
    pub fn paths_for_agent(
        &self,
        agent_id: &AgentID,
        required: WorkspaceGrantMode,
    ) -> Vec<PathBuf> {
        self.read_snapshot()
            .into_iter()
            .filter(|g| {
                (g.agent_id.is_none() || g.agent_id.as_ref() == Some(agent_id))
                    && g.mode.covers(required)
            })
            .map(|g| g.path)
            .collect()
    }

    /// Snapshot of all active grants regardless of scope.
    pub fn list_all_active(&self) -> Vec<WorkspaceGrant> {
        self.read_snapshot()
    }

    /// Hold the cache write lock across the store insert so two concurrent
    /// `grant()` calls (or a concurrent `grant()` + `revoke()`) cannot
    /// produce a cache that disagrees with the on-disk store.
    pub fn grant(
        &self,
        path: &Path,
        agent_id: Option<AgentID>,
        mode: WorkspaceGrantMode,
        source: &str,
        granted_by: &str,
    ) -> Result<WorkspaceGrant, AgentOSError> {
        let mut cache = self
            .cache
            .write()
            .map_err(|_| AgentOSError::StorageError("workspace cache poisoned".into()))?;
        let g = self.store.grant(path, agent_id, mode, source, granted_by)?;
        cache.push(g.clone());
        Ok(g)
    }

    pub fn revoke(&self, path: &Path, agent_id: Option<&AgentID>) -> Result<u64, AgentOSError> {
        let mut cache = self
            .cache
            .write()
            .map_err(|_| AgentOSError::StorageError("workspace cache poisoned".into()))?;
        let n = self.store.revoke(path, agent_id)?;
        if n > 0 {
            // Use the same lexical normalization the store applies so the
            // cache retain matches the row(s) the SQL UPDATE just touched.
            let normalized = lexically_normalize(path);
            cache.retain(|g| !(g.path == normalized && g.agent_id.as_ref() == agent_id));
        }
        Ok(n)
    }

    /// Replace the entire cache with whatever the store currently reports as
    /// active. Useful after bulk operations or as a periodic consistency
    /// check. `reload()` is the natural recovery path when the cache is
    /// poisoned, so it must not itself panic on poison — it returns a
    /// `StorageError` instead.
    pub fn reload(&self) -> Result<(), AgentOSError> {
        let fresh = self.store.list_active()?;
        let mut guard = self
            .cache
            .write()
            .map_err(|_| AgentOSError::StorageError("workspace cache poisoned".into()))?;
        *guard = fresh;
        Ok(())
    }

    pub fn store(&self) -> Arc<WorkspaceGrantStore> {
        self.store.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::AgentID;
    use tempfile::TempDir;

    fn agent() -> AgentID {
        AgentID::new()
    }

    async fn fresh_store() -> (TempDir, Arc<WorkspaceGrantStore>) {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("workspace_grants.db");
        let store = WorkspaceGrantStore::open(db).await.unwrap();
        (dir, Arc::new(store))
    }

    #[tokio::test]
    async fn grant_and_list() {
        let (_t, store) = fresh_store().await;
        let g = store
            .grant(
                Path::new("/home/alice/project"),
                None,
                WorkspaceGrantMode::READ_WRITE,
                "test",
                "tester",
            )
            .unwrap();
        assert_eq!(g.mode, WorkspaceGrantMode::READ_WRITE);
        let list = store.list_active().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, PathBuf::from("/home/alice/project"));
        assert!(list[0].agent_id.is_none());
    }

    #[tokio::test]
    async fn duplicate_active_grant_rejected_with_sentinel_resource() {
        let (_t, store) = fresh_store().await;
        store
            .grant(
                Path::new("/home/alice/p"),
                None,
                WorkspaceGrantMode::READ,
                "t",
                "u",
            )
            .unwrap();
        let err = store
            .grant(
                Path::new("/home/alice/p"),
                None,
                WorkspaceGrantMode::READ_WRITE,
                "t",
                "u",
            )
            .unwrap_err();
        match err {
            AgentOSError::PermissionDenied { resource, .. } => {
                assert_eq!(resource, GRANT_DUPLICATE_RESOURCE);
            }
            other => panic!("expected GRANT_DUPLICATE_RESOURCE, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn trailing_slash_variant_is_same_grant() {
        let (_t, store) = fresh_store().await;
        store
            .grant(
                Path::new("/home/alice/proj"),
                None,
                WorkspaceGrantMode::READ_WRITE,
                "t",
                "u",
            )
            .unwrap();
        // A trailing-slash variant of the same directory should collide on
        // the unique index (because both are normalized to the same key).
        let err = store
            .grant(
                Path::new("/home/alice/proj/"),
                None,
                WorkspaceGrantMode::READ,
                "t",
                "u",
            )
            .unwrap_err();
        match err {
            AgentOSError::PermissionDenied { resource, .. } => {
                assert_eq!(resource, GRANT_DUPLICATE_RESOURCE);
            }
            other => panic!("expected GRANT_DUPLICATE_RESOURCE, got {other:?}"),
        }
        // Revoke also matches the trailing-slash variant.
        let n = store.revoke(Path::new("/home/alice/proj/"), None).unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn revoke_then_regrant() {
        let (_t, store) = fresh_store().await;
        store
            .grant(
                Path::new("/home/alice/p"),
                None,
                WorkspaceGrantMode::READ,
                "t",
                "u",
            )
            .unwrap();
        let n = store.revoke(Path::new("/home/alice/p"), None).unwrap();
        assert_eq!(n, 1);
        assert!(store.list_active().unwrap().is_empty());
        store
            .grant(
                Path::new("/home/alice/p"),
                None,
                WorkspaceGrantMode::READ_WRITE_EXEC,
                "t",
                "u",
            )
            .unwrap();
        let l = store.list_active().unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].mode, WorkspaceGrantMode::READ_WRITE_EXEC);
    }

    #[tokio::test]
    async fn agent_scoped_grant_excluded_from_other_agents() {
        let (_t, store) = fresh_store().await;
        let a = agent();
        let b = agent();
        store
            .grant(
                Path::new("/home/alice/private"),
                Some(a),
                WorkspaceGrantMode::READ,
                "t",
                "u",
            )
            .unwrap();
        let reg = WorkspaceGrantRegistry::load(store).unwrap();
        assert_eq!(reg.list_for_agent(&a).len(), 1);
        assert_eq!(reg.list_for_agent(&b).len(), 0);
    }

    #[tokio::test]
    async fn registry_grant_and_revoke_keeps_cache_consistent() {
        let (_t, store) = fresh_store().await;
        let reg = WorkspaceGrantRegistry::load(store).unwrap();
        let a = agent();
        reg.grant(
            Path::new("/home/alice/wsp"),
            Some(a),
            WorkspaceGrantMode::READ_WRITE,
            "test",
            "tester",
        )
        .unwrap();
        let paths = reg.paths_for_agent(&a, WorkspaceGrantMode::READ);
        assert_eq!(paths, vec![PathBuf::from("/home/alice/wsp")]);
        // Write-only grant doesn't cover exec
        let exec_paths = reg.paths_for_agent(&a, WorkspaceGrantMode::READ_WRITE_EXEC);
        assert!(exec_paths.is_empty());
        let n = reg.revoke(Path::new("/home/alice/wsp"), Some(&a)).unwrap();
        assert_eq!(n, 1);
        assert!(reg.list_for_agent(&a).is_empty());
    }

    #[tokio::test]
    async fn grant_rejects_forbidden_path() {
        let (_t, store) = fresh_store().await;
        let err = store
            .grant(Path::new("/etc"), None, WorkspaceGrantMode::READ, "t", "u")
            .unwrap_err();
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }
}
