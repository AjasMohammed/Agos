//! SQLite-backed persistence for managed workspaces.
//!
//! `EnvProvider` keeps an in-memory map of workspaces, but kernel restarts
//! used to drop that state while the on-disk directories survived. This store
//! provides a write-through mirror so the in-memory map can be rebuilt at
//! boot. Pattern mirrors `checkpoint_store.rs` (WAL mode, single connection
//! behind `Arc<Mutex<Connection>>`, blocking I/O via `spawn_blocking`).

use crate::managed_env::{Ecosystem, InstalledPackage, ManagedWorkspace};
use agentos_types::AgentID;
use anyhow::{anyhow, Context};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

const WORKSPACE_SCHEMA_VERSION: i64 = 1;

/// SQLite-backed store for managed workspace metadata.
pub struct WorkspaceStore {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl WorkspaceStore {
    /// Open (or create) the store at `path`.
    ///
    /// Runs schema migrations on the connection. Parent directory is created
    /// if missing so the kernel can bootstrap a fresh data directory.
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for workspace DB: {}",
                        parent.display()
                    )
                })?;
            }
            let conn = Connection::open(&path_for_open).with_context(|| {
                format!("Failed to open workspace DB at {}", path_for_open.display())
            })?;
            Self::configure_connection(&conn)?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("Workspace DB open task failed")??;

        Ok(Self {
            path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Path on disk.
    pub fn db_path(&self) -> &Path {
        &self.path
    }

    fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;",
        )
        .context("Failed to configure workspace DB pragmas")?;
        Ok(())
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspaces (
                agent_id      TEXT NOT NULL,
                name          TEXT NOT NULL,
                root_path     TEXT NOT NULL,
                ecosystem     TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                packages_json TEXT NOT NULL,
                schema_version INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (agent_id, name)
            );",
        )
        .context("Failed to run workspace DB migrations")?;
        Ok(())
    }

    /// Insert or replace a workspace row.
    pub async fn upsert(&self, ws: &ManagedWorkspace) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let agent_id = ws.agent_id.to_string();
        let name = ws.name.clone();
        let root_path = ws.root_path.to_string_lossy().to_string();
        let ecosystem = ecosystem_to_str(ws.ecosystem).to_string();
        let created_at = ws.created_at.to_rfc3339();
        let packages_json = serde_json::to_string(&ws.packages_installed)
            .context("Failed to serialize packages_installed")?;

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Workspace DB mutex poisoned"))?;
            guard
                .execute(
                    "INSERT INTO workspaces (
                        agent_id, name, root_path, ecosystem,
                        created_at, packages_json, schema_version
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    ON CONFLICT(agent_id, name) DO UPDATE SET
                        root_path = excluded.root_path,
                        ecosystem = excluded.ecosystem,
                        created_at = excluded.created_at,
                        packages_json = excluded.packages_json,
                        schema_version = excluded.schema_version",
                    params![
                        agent_id,
                        name,
                        root_path,
                        ecosystem,
                        created_at,
                        packages_json,
                        WORKSPACE_SCHEMA_VERSION,
                    ],
                )
                .context("Failed to upsert workspace row")?;
            Ok(())
        })
        .await
        .context("Workspace upsert task failed")?
    }

    /// Delete a workspace row by (agent_id, name).
    pub async fn remove(&self, agent_id: AgentID, name: String) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let agent_id_str = agent_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Workspace DB mutex poisoned"))?;
            guard
                .execute(
                    "DELETE FROM workspaces WHERE agent_id = ?1 AND name = ?2",
                    params![agent_id_str, name],
                )
                .context("Failed to delete workspace row")?;
            Ok(())
        })
        .await
        .context("Workspace remove task failed")?
    }

    /// Load every workspace row from the DB. Called once at boot.
    ///
    /// Rows with parse failures (malformed UUID, missing ecosystem, broken
    /// JSON) are skipped with a warning rather than aborting boot — the
    /// kernel should come up even if a single workspace row is corrupted.
    pub async fn load_all(&self) -> anyhow::Result<Vec<ManagedWorkspace>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ManagedWorkspace>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Workspace DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT agent_id, name, root_path, ecosystem,
                            created_at, packages_json
                     FROM workspaces
                     ORDER BY agent_id, name",
                )
                .context("Failed to prepare workspace SELECT")?;

            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .context("Failed to query workspaces")?;

            let mut out = Vec::new();
            for row in rows {
                let (agent_id_s, name, root_path, eco_s, created_at_s, packages_json) =
                    row.context("Failed to read workspace row")?;
                let agent_id = match AgentID::from_str(&agent_id_s) {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(agent_id = %agent_id_s, error = %e, "skipping workspace row with invalid agent_id");
                        continue;
                    }
                };
                let ecosystem = match str_to_ecosystem(&eco_s) {
                    Some(e) => e,
                    None => {
                        tracing::warn!(ecosystem = %eco_s, "skipping workspace row with unknown ecosystem");
                        continue;
                    }
                };
                let created_at = match chrono::DateTime::parse_from_rfc3339(&created_at_s) {
                    Ok(dt) => dt.with_timezone(&chrono::Utc),
                    Err(e) => {
                        tracing::warn!(created_at = %created_at_s, error = %e, "skipping workspace row with invalid created_at");
                        continue;
                    }
                };
                let packages_installed: Vec<InstalledPackage> =
                    match serde_json::from_str(&packages_json) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(workspace = %name, error = %e, "skipping workspace row with malformed packages_json");
                            continue;
                        }
                    };

                out.push(ManagedWorkspace {
                    name,
                    agent_id,
                    root_path: PathBuf::from(root_path),
                    ecosystem,
                    created_at,
                    packages_installed,
                });
            }
            Ok(out)
        })
        .await
        .context("Workspace load_all task failed")?
    }
}

fn ecosystem_to_str(e: Ecosystem) -> &'static str {
    match e {
        Ecosystem::Python => "python",
        Ecosystem::NodeJs => "nodejs",
        Ecosystem::Rust => "rust",
        Ecosystem::System => "system",
        Ecosystem::Generic => "generic",
    }
}

fn str_to_ecosystem(s: &str) -> Option<Ecosystem> {
    match s {
        "python" => Some(Ecosystem::Python),
        "nodejs" => Some(Ecosystem::NodeJs),
        "rust" => Some(Ecosystem::Rust),
        "system" => Some(Ecosystem::System),
        "generic" => Some(Ecosystem::Generic),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::TempDir;

    fn fixture_workspace(name: &str, eco: Ecosystem) -> ManagedWorkspace {
        ManagedWorkspace {
            name: name.to_string(),
            agent_id: AgentID::new(),
            root_path: PathBuf::from(format!("/tmp/agentos/workspaces/{name}")),
            ecosystem: eco,
            created_at: Utc::now(),
            packages_installed: vec![InstalledPackage {
                name: "flask".into(),
                version: "3.0.0".into(),
                ecosystem: eco,
                installed_at: Utc::now(),
            }],
        }
    }

    #[tokio::test]
    async fn open_creates_schema_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("workspaces.db");
        let _store = WorkspaceStore::open(path.clone()).await.unwrap();
        // Open again on the same path — must not error.
        let _store2 = WorkspaceStore::open(path).await.unwrap();
    }

    #[tokio::test]
    async fn upsert_then_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = WorkspaceStore::open(tmp.path().join("workspaces.db"))
            .await
            .unwrap();
        let ws_a = fixture_workspace("py-a", Ecosystem::Python);
        let ws_b = fixture_workspace("node-b", Ecosystem::NodeJs);
        store.upsert(&ws_a).await.unwrap();
        store.upsert(&ws_b).await.unwrap();

        let loaded = store.load_all().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().any(|w| w.name == "py-a"));
        assert!(loaded.iter().any(|w| w.name == "node-b"));
    }

    #[tokio::test]
    async fn upsert_replaces_existing_row() {
        let tmp = TempDir::new().unwrap();
        let store = WorkspaceStore::open(tmp.path().join("workspaces.db"))
            .await
            .unwrap();
        let mut ws = fixture_workspace("py", Ecosystem::Python);
        store.upsert(&ws).await.unwrap();

        ws.packages_installed.push(InstalledPackage {
            name: "requests".into(),
            version: "2.31".into(),
            ecosystem: Ecosystem::Python,
            installed_at: Utc::now(),
        });
        store.upsert(&ws).await.unwrap();

        let loaded = store.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].packages_installed.len(), 2);
    }

    #[tokio::test]
    async fn remove_deletes_only_matching_row() {
        let tmp = TempDir::new().unwrap();
        let store = WorkspaceStore::open(tmp.path().join("workspaces.db"))
            .await
            .unwrap();
        let ws_a = fixture_workspace("py-a", Ecosystem::Python);
        let ws_b = fixture_workspace("py-b", Ecosystem::Python);
        store.upsert(&ws_a).await.unwrap();
        store.upsert(&ws_b).await.unwrap();

        store
            .remove(ws_a.agent_id, "py-a".to_string())
            .await
            .unwrap();

        let loaded = store.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "py-b");
    }

    #[tokio::test]
    async fn empty_packages_serializes() {
        let tmp = TempDir::new().unwrap();
        let store = WorkspaceStore::open(tmp.path().join("workspaces.db"))
            .await
            .unwrap();
        let mut ws = fixture_workspace("empty", Ecosystem::Generic);
        ws.packages_installed.clear();
        store.upsert(&ws).await.unwrap();

        let loaded = store.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].packages_installed.is_empty());
    }
}
