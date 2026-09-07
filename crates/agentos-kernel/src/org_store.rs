//! Durable agent-org registry (`org.db`).
//!
//! Persists [`OrgNode`]s so the coordinator/worker structure survives restarts
//! and is queryable. The security-critical invariant — a node's capability scope
//! must be a subset of its manager's — is enforced here at write time via
//! [`PermissionSet::is_subset_of`], reusing the same `check()` path as live
//! capability enforcement.
//!
//! Modeled on [`crate::checkpoint_store::CheckpointStore`]: an `Arc<Mutex<Connection>>`
//! with async methods that hop to `spawn_blocking`, WAL mode, parameterized
//! queries (no string interpolation).

use agentos_types::{AgentBudget, OrgNode, OrgNodeID, PermissionSet, TeamRole};
use anyhow::{anyhow, Context};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const LATEST_MIGRATION_VERSION: i64 = 1;

/// Error returned when an upsert would violate the org's invariants.
#[derive(Debug, thiserror::Error)]
pub enum OrgStoreError {
    #[error("manager node {0} not found")]
    ManagerNotFound(String),
    #[error(
        "node scope for agent '{agent}' exceeds its manager's scope (downward-only violation)"
    )]
    ScopeEscalation { agent: String },
    #[error("a node may not be its own manager")]
    SelfManager,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct OrgStore {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl OrgStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for org DB: {}",
                        parent.display()
                    )
                })?;
            }
            let conn = Connection::open(&path_for_open)
                .with_context(|| format!("Failed to open org DB at {}", path_for_open.display()))?;
            Self::configure_connection(&conn)?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("Org DB open task failed")??;

        Ok(Self {
            path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;",
        )
        .context("Failed to configure org DB pragmas")?;
        Ok(())
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .context("Failed to create org meta table")?;

        let version: i64 = conn
            .query_row(
                "SELECT value FROM org_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("Failed to read org schema version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);

        if version > LATEST_MIGRATION_VERSION {
            anyhow::bail!(
                "Org DB schema version {} is newer than supported version {}",
                version,
                LATEST_MIGRATION_VERSION
            );
        }

        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS org_nodes (
                    node_id        TEXT PRIMARY KEY,
                    org_id         TEXT NOT NULL,
                    agent_name     TEXT NOT NULL,
                    manager_id     TEXT,
                    role           TEXT NOT NULL,
                    title          TEXT NOT NULL DEFAULT '',
                    cap_scope_json TEXT NOT NULL,
                    budget_json    TEXT,
                    created_at     TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_org_nodes_org ON org_nodes(org_id);
                CREATE INDEX IF NOT EXISTS idx_org_nodes_mgr ON org_nodes(manager_id);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_org_nodes_org_agent
                    ON org_nodes(org_id, agent_name);
                INSERT INTO org_meta(key, value) VALUES ('schema_version', '1')
                    ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
            )
            .context("Failed to run org schema migration v1")?;
        }
        Ok(())
    }

    /// Insert or update a node. Enforces the downward-only invariant: the node's
    /// `cap_scope` must be a subset of its manager's `cap_scope`. A node with no
    /// manager (the CEO) is unconstrained. Validation runs inside the same lock
    /// as the write so a concurrent manager change can't race the check.
    pub async fn upsert_node(&self, node: OrgNode) -> Result<(), OrgStoreError> {
        if node.manager_id.as_ref() == Some(&node.node_id) {
            return Err(OrgStoreError::SelfManager);
        }
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), OrgStoreError> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Org DB mutex poisoned"))
                .map_err(OrgStoreError::Other)?;

            if let Some(manager_id) = &node.manager_id {
                let manager_scope = Self::read_scope(&guard, manager_id)?
                    .ok_or_else(|| OrgStoreError::ManagerNotFound(manager_id.to_string()))?;
                if !node.cap_scope.is_subset_of(&manager_scope) {
                    return Err(OrgStoreError::ScopeEscalation {
                        agent: node.agent_name.clone(),
                    });
                }
            }

            let cap_scope_json = serde_json::to_string(&node.cap_scope)
                .context("serialize cap_scope")
                .map_err(OrgStoreError::Other)?;
            let budget_json = match &node.budget {
                Some(b) => Some(
                    serde_json::to_string(b)
                        .context("serialize budget")
                        .map_err(OrgStoreError::Other)?,
                ),
                None => None,
            };

            guard
                .execute(
                    "INSERT INTO org_nodes (
                        node_id, org_id, agent_name, manager_id, role, title,
                        cap_scope_json, budget_json, created_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    ON CONFLICT(node_id) DO UPDATE SET
                        org_id = excluded.org_id,
                        agent_name = excluded.agent_name,
                        manager_id = excluded.manager_id,
                        role = excluded.role,
                        title = excluded.title,
                        cap_scope_json = excluded.cap_scope_json,
                        budget_json = excluded.budget_json",
                    params![
                        node.node_id.to_string(),
                        node.org_id.to_string(),
                        node.agent_name,
                        node.manager_id.as_ref().map(|m| m.to_string()),
                        role_to_str(&node.role),
                        node.title,
                        cap_scope_json,
                        budget_json,
                        chrono::Utc::now().to_rfc3339(),
                    ],
                )
                .context("upsert org node")
                .map_err(OrgStoreError::Other)?;
            Ok(())
        })
        .await
        .context("Org upsert task failed")
        .map_err(OrgStoreError::Other)?
    }

    /// Remove a node by id. Returns the number of rows deleted (0 or 1).
    pub async fn remove_node(&self, node_id: &OrgNodeID) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let id = node_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn.lock().map_err(|_| anyhow!("Org DB mutex poisoned"))?;
            let n = guard
                .execute("DELETE FROM org_nodes WHERE node_id = ?1", params![id])
                .context("delete org node")?;
            Ok(n)
        })
        .await
        .context("Org remove task failed")?
    }

    /// All direct reports of `manager_id`.
    pub async fn children_of(&self, manager_id: &OrgNodeID) -> anyhow::Result<Vec<OrgNode>> {
        let conn = self.conn.clone();
        let id = manager_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<OrgNode>> {
            let guard = conn.lock().map_err(|_| anyhow!("Org DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare("SELECT node_id, org_id, agent_name, manager_id, role, title, cap_scope_json, budget_json FROM org_nodes WHERE manager_id = ?1")
                .context("prepare children query")?;
            let rows = stmt
                .query_map(params![id], row_to_node)
                .context("query children")?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("collect children")
        })
        .await
        .context("Org children task failed")?
    }

    /// Look up a node by (org, agent name). `None` if the agent isn't in the org.
    pub async fn node_by_agent(
        &self,
        org_id: &agentos_types::OrgID,
        agent_name: &str,
    ) -> anyhow::Result<Option<OrgNode>> {
        let conn = self.conn.clone();
        let org = org_id.to_string();
        let agent = agent_name.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<OrgNode>> {
            let guard = conn.lock().map_err(|_| anyhow!("Org DB mutex poisoned"))?;
            guard
                .query_row(
                    "SELECT node_id, org_id, agent_name, manager_id, role, title, cap_scope_json, budget_json FROM org_nodes WHERE org_id = ?1 AND agent_name = ?2",
                    params![org, agent],
                    row_to_node,
                )
                .optional()
                .context("query node_by_agent")
        })
        .await
        .context("Org node_by_agent task failed")?
    }

    /// Every node in an org.
    pub async fn load_org(&self, org_id: &agentos_types::OrgID) -> anyhow::Result<Vec<OrgNode>> {
        let conn = self.conn.clone();
        let org = org_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<OrgNode>> {
            let guard = conn.lock().map_err(|_| anyhow!("Org DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare("SELECT node_id, org_id, agent_name, manager_id, role, title, cap_scope_json, budget_json FROM org_nodes WHERE org_id = ?1")
                .context("prepare load_org query")?;
            let rows = stmt
                .query_map(params![org], row_to_node)
                .context("query load_org")?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("collect load_org")
        })
        .await
        .context("Org load_org task failed")?
    }

    /// All capability scopes attached to `agent_name` across every org it belongs
    /// to. Used by the spawn-time clamp: a child agent's effective scope is
    /// intersected with *all* of these, so it can never exceed any node the agent
    /// occupies (fail-closed for the rare multi-org case). Empty = not in any org.
    pub async fn scopes_for_agent(&self, agent_name: &str) -> anyhow::Result<Vec<PermissionSet>> {
        let conn = self.conn.clone();
        let agent = agent_name.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<PermissionSet>> {
            let guard = conn.lock().map_err(|_| anyhow!("Org DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare("SELECT cap_scope_json FROM org_nodes WHERE agent_name = ?1")
                .context("prepare scopes_for_agent query")?;
            let rows = stmt
                .query_map(params![agent], |row| row.get::<_, String>(0))
                .context("query scopes_for_agent")?;
            let mut scopes = Vec::new();
            for json in rows {
                let json = json.context("read scope json")?;
                let scope: PermissionSet =
                    serde_json::from_str(&json).context("deserialize node scope")?;
                scopes.push(scope);
            }
            Ok(scopes)
        })
        .await
        .context("Org scopes_for_agent task failed")?
    }

    /// The budget configured on `agent_name`'s org node, if any. When an agent
    /// occupies multiple nodes, the oldest node carrying a budget wins (stable
    /// across DB rebuilds). `None` means "no org budget" → the caller falls back
    /// to the global config budget.
    pub async fn budget_for_agent(&self, agent_name: &str) -> anyhow::Result<Option<AgentBudget>> {
        let conn = self.conn.clone();
        let agent = agent_name.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<AgentBudget>> {
            let guard = conn.lock().map_err(|_| anyhow!("Org DB mutex poisoned"))?;
            let budget_json: Option<String> = guard
                .query_row(
                    "SELECT budget_json FROM org_nodes WHERE agent_name = ?1 AND budget_json IS NOT NULL ORDER BY created_at ASC LIMIT 1",
                    params![agent],
                    |row| row.get(0),
                )
                .optional()
                .context("query budget_for_agent")?;
            match budget_json {
                Some(json) => Ok(Some(
                    serde_json::from_str(&json).context("deserialize node budget")?,
                )),
                None => Ok(None),
            }
        })
        .await
        .context("Org budget_for_agent task failed")?
    }

    /// Read just the capability scope of a node (used for the subset check).
    fn read_scope(
        conn: &Connection,
        node_id: &OrgNodeID,
    ) -> Result<Option<PermissionSet>, OrgStoreError> {
        let scope_json: Option<String> = conn
            .query_row(
                "SELECT cap_scope_json FROM org_nodes WHERE node_id = ?1",
                params![node_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .context("read manager scope")
            .map_err(OrgStoreError::Other)?;
        match scope_json {
            Some(json) => {
                let scope = serde_json::from_str(&json)
                    .context("deserialize manager scope")
                    .map_err(OrgStoreError::Other)?;
                Ok(Some(scope))
            }
            None => Ok(None),
        }
    }
}

fn role_to_str(role: &TeamRole) -> &'static str {
    match role {
        TeamRole::Coordinator => "Coordinator",
        TeamRole::Worker => "Worker",
    }
}

fn role_from_str(s: &str) -> TeamRole {
    match s {
        "Coordinator" => TeamRole::Coordinator,
        _ => TeamRole::Worker,
    }
}

/// Map a `org_nodes` row to an [`OrgNode`]. Column order must match the SELECTs above.
fn row_to_node(row: &rusqlite::Row) -> rusqlite::Result<OrgNode> {
    let node_id_s: String = row.get(0)?;
    let org_id_s: String = row.get(1)?;
    let agent_name: String = row.get(2)?;
    let manager_id_s: Option<String> = row.get(3)?;
    let role_s: String = row.get(4)?;
    let title: String = row.get(5)?;
    let cap_scope_json: String = row.get(6)?;
    let budget_json: Option<String> = row.get(7)?;

    let to_err = |e: String| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    };

    let cap_scope: PermissionSet =
        serde_json::from_str(&cap_scope_json).map_err(|e| to_err(e.to_string()))?;
    let budget: Option<AgentBudget> = match budget_json {
        Some(j) => Some(serde_json::from_str(&j).map_err(|e| to_err(e.to_string()))?),
        None => None,
    };

    Ok(OrgNode {
        node_id: node_id_s
            .parse::<OrgNodeID>()
            .map_err(|e| to_err(e.to_string()))?,
        org_id: org_id_s
            .parse::<agentos_types::OrgID>()
            .map_err(|e| to_err(e.to_string()))?,
        agent_name,
        manager_id: match manager_id_s {
            Some(s) => Some(s.parse::<OrgNodeID>().map_err(|e| to_err(e.to_string()))?),
            None => None,
        },
        role: role_from_str(&role_s),
        title,
        cap_scope,
        budget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::OrgID;

    async fn store() -> OrgStore {
        // In-memory via a temp path keeps tests hermetic.
        let dir = tempfile::tempdir().unwrap();
        OrgStore::open(dir.path().join("org.db")).await.unwrap()
    }

    fn scope(resource: &str, write: bool) -> PermissionSet {
        let mut p = PermissionSet::new();
        p.grant(resource.to_string(), true, write, false, None);
        p
    }

    #[tokio::test]
    async fn ceo_node_persists_and_loads() {
        let s = store().await;
        let org = OrgID::new();
        let ceo = OrgNode::root(org, "ceo", scope("fs:/home/user/", true));
        let ceo_id = ceo.node_id;
        s.upsert_node(ceo).await.unwrap();

        let loaded = s.node_by_agent(&org, "ceo").await.unwrap().unwrap();
        assert_eq!(loaded.node_id, ceo_id);
        assert!(loaded.manager_id.is_none());
        assert_eq!(s.load_org(&org).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn worker_within_manager_scope_is_accepted() {
        let s = store().await;
        let org = OrgID::new();
        let ceo = OrgNode::root(org, "ceo", scope("fs:/home/user/", true));
        let ceo_id = ceo.node_id;
        s.upsert_node(ceo).await.unwrap();

        let mut worker = OrgNode::root(org, "researcher", scope("fs:/home/user/docs/", false));
        worker.manager_id = Some(ceo_id);
        worker.role = TeamRole::Worker;
        s.upsert_node(worker).await.unwrap();

        assert_eq!(s.children_of(&ceo_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn worker_exceeding_manager_scope_is_rejected() {
        let s = store().await;
        let org = OrgID::new();
        let ceo = OrgNode::root(org, "ceo", scope("fs:/home/user/", false)); // read-only
        let ceo_id = ceo.node_id;
        s.upsert_node(ceo).await.unwrap();

        // Worker requests WRITE the CEO doesn't have → escalation, must be rejected.
        let mut rogue = OrgNode::root(org, "rogue", scope("fs:/home/user/", true));
        rogue.manager_id = Some(ceo_id);
        rogue.role = TeamRole::Worker;
        let err = s.upsert_node(rogue).await.unwrap_err();
        assert!(matches!(err, OrgStoreError::ScopeEscalation { .. }));

        // And a resource entirely outside the CEO's scope is rejected too.
        let mut outsider = OrgNode::root(org, "outsider", scope("fs:/etc/", false));
        outsider.manager_id = Some(ceo_id);
        let err = s.upsert_node(outsider).await.unwrap_err();
        assert!(matches!(err, OrgStoreError::ScopeEscalation { .. }));
    }

    #[tokio::test]
    async fn unknown_manager_is_rejected() {
        let s = store().await;
        let org = OrgID::new();
        let mut orphan = OrgNode::root(org, "orphan", scope("fs:/home/user/", false));
        orphan.manager_id = Some(OrgNodeID::new());
        let err = s.upsert_node(orphan).await.unwrap_err();
        assert!(matches!(err, OrgStoreError::ManagerNotFound(_)));
    }

    #[tokio::test]
    async fn scopes_for_agent_returns_all_nodes() {
        let s = store().await;
        let org = OrgID::new();
        let ceo = OrgNode::root(org, "ceo", scope("fs:/home/user/", true));
        let ceo_id = ceo.node_id;
        s.upsert_node(ceo).await.unwrap();
        let mut worker = OrgNode::root(org, "worker", scope("fs:/home/user/docs/", false));
        worker.manager_id = Some(ceo_id);
        worker.role = TeamRole::Worker;
        s.upsert_node(worker).await.unwrap();

        // The worker's scope is returned and is read-only under the docs subtree.
        let scopes = s.scopes_for_agent("worker").await.unwrap();
        assert_eq!(scopes.len(), 1);
        assert!(scopes[0].check("fs:/home/user/docs/x", agentos_types::PermissionOp::Read));
        assert!(!scopes[0].check("fs:/home/user/docs/x", agentos_types::PermissionOp::Write));

        // An agent in no org yields an empty vec (→ no clamp at spawn time).
        assert!(s.scopes_for_agent("stranger").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_node_deletes() {
        let s = store().await;
        let org = OrgID::new();
        let ceo = OrgNode::root(org, "ceo", scope("fs:/home/user/", true));
        let ceo_id = ceo.node_id;
        s.upsert_node(ceo).await.unwrap();
        assert_eq!(s.remove_node(&ceo_id).await.unwrap(), 1);
        assert!(s.node_by_agent(&org, "ceo").await.unwrap().is_none());
    }
}
