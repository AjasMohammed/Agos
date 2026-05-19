use agentos_types::{AgentID, TaskID};
use anyhow::{bail, Context};
use regex::Regex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub const MAX_EVIDENCE_ENTRIES: usize = 5;
pub const MAX_PENDING_PER_TASK: usize = 3;
pub const MIN_INSERT_CONFIDENCE: f32 = 0.5;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    Add,
    Replace,
    Delete,
}

impl ProposalKind {
    fn as_str(self) -> &'static str {
        match self {
            ProposalKind::Add => "add",
            ProposalKind::Replace => "replace",
            ProposalKind::Delete => "delete",
        }
    }

    fn parse(s: &str) -> anyhow::Result<Self> {
        Ok(match s {
            "add" => ProposalKind::Add,
            "replace" => ProposalKind::Replace,
            "delete" => ProposalKind::Delete,
            other => bail!("unknown proposal kind: {other}"),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Accepted,
    Rejected,
    Expired,
}

impl ProposalStatus {
    fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Pending => "pending",
            ProposalStatus::Accepted => "accepted",
            ProposalStatus::Rejected => "rejected",
            ProposalStatus::Expired => "expired",
        }
    }

    fn parse(s: &str) -> anyhow::Result<Self> {
        Ok(match s {
            "pending" => ProposalStatus::Pending,
            "accepted" => ProposalStatus::Accepted,
            "rejected" => ProposalStatus::Rejected,
            "expired" => ProposalStatus::Expired,
            other => bail!("unknown proposal status: {other}"),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPrefProposal {
    pub id: String,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub kind: ProposalKind,
    pub content: String,
    pub confidence: f32,
    pub evidence: Vec<String>,
    pub replaces: Option<String>,
    pub status: ProposalStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub reviewed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalStats {
    pub pending: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub expired: u64,
    pub stale_over_30d: u64,
}

#[derive(Debug)]
pub struct InsertOutcome {
    pub inserted: Vec<UserPrefProposal>,
    pub rejected: usize,
}

pub struct UserPrefProposalStore {
    conn: Arc<Mutex<Connection>>,
}

impl UserPrefProposalStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            let conn = Connection::open(&path)
                .with_context(|| format!("open user_pref_proposals.db at {}", path.display()))?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS user_pref_proposals (
                    id TEXT PRIMARY KEY,
                    task_id TEXT NOT NULL,
                    agent_id TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK (kind IN ('add','replace','delete')),
                    content TEXT NOT NULL,
                    confidence REAL NOT NULL CHECK (confidence BETWEEN 0.0 AND 1.0),
                    evidence_json TEXT NOT NULL,
                    replaces_id TEXT,
                    status TEXT NOT NULL CHECK (status IN ('pending','accepted','rejected','expired')),
                    created_at TEXT NOT NULL,
                    reviewed_at TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_user_pref_proposals_status_created
                  ON user_pref_proposals(status, created_at DESC);
                CREATE INDEX IF NOT EXISTS idx_user_pref_proposals_agent
                  ON user_pref_proposals(agent_id, created_at DESC);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_user_pref_proposals_unique_pending_replaces
                  ON user_pref_proposals(replaces_id)
                  WHERE status = 'pending' AND replaces_id IS NOT NULL;",
            )?;
            Ok(conn)
        })
        .await
        .context("spawn_blocking open user_pref_proposals")??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert a batch of proposals. Each row is independently validated against
    /// store invariants — confidence floor, evidence cap, per-task pending cap.
    /// Rows that violate an invariant are dropped (counted in `rejected`),
    /// not aborted at the batch level.
    pub async fn insert_many(
        &self,
        proposals: &[UserPrefProposal],
    ) -> anyhow::Result<InsertOutcome> {
        if proposals.is_empty() {
            return Ok(InsertOutcome {
                inserted: Vec::new(),
                rejected: 0,
            });
        }
        let conn = Arc::clone(&self.conn);
        let rows: Vec<UserPrefProposal> = proposals.to_vec();
        tokio::task::spawn_blocking(move || -> anyhow::Result<InsertOutcome> {
            let guard = conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let tx = guard.unchecked_transaction()?;
            let mut inserted = Vec::new();
            let mut rejected = 0usize;
            for mut p in rows {
                // Confidence floor — store-side enforcement, callers can't bypass.
                if p.confidence < MIN_INSERT_CONFIDENCE {
                    rejected += 1;
                    continue;
                }
                // Evidence cap.
                if p.evidence.len() > MAX_EVIDENCE_ENTRIES {
                    p.evidence.truncate(MAX_EVIDENCE_ENTRIES);
                }
                // Per-task pending cap (counts existing pending in this txn).
                let pending: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM user_pref_proposals
                     WHERE agent_id = ?1 AND task_id = ?2 AND status = 'pending'",
                    params![p.agent_id.to_string(), p.task_id.to_string()],
                    |r| r.get(0),
                )?;
                if (pending as usize) >= MAX_PENDING_PER_TASK {
                    rejected += 1;
                    continue;
                }
                let res = tx.execute(
                    "INSERT INTO user_pref_proposals
                     (id, task_id, agent_id, kind, content, confidence, evidence_json, replaces_id, status, created_at, reviewed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        p.id,
                        p.task_id.to_string(),
                        p.agent_id.to_string(),
                        p.kind.as_str(),
                        p.content,
                        p.confidence,
                        serde_json::to_string(&p.evidence)?,
                        p.replaces,
                        p.status.as_str(),
                        p.created_at.to_rfc3339(),
                        p.reviewed_at.map(|d| d.to_rfc3339()),
                    ],
                );
                match res {
                    Ok(_) => inserted.push(p),
                    Err(rusqlite::Error::SqliteFailure(err, _))
                        if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        // unique pending-replaces collision or CHECK rejection
                        rejected += 1;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            tx.commit()?;
            Ok(InsertOutcome { inserted, rejected })
        })
        .await
        .context("spawn_blocking insert user_pref_proposals")?
    }

    pub async fn list_pending(&self, limit: u32) -> anyhow::Result<Vec<UserPrefProposal>> {
        self.list_by_status(ProposalStatus::Pending, limit).await
    }

    pub async fn accept(&self, id: &str) -> anyhow::Result<bool> {
        self.transition(id, ProposalStatus::Accepted).await
    }

    pub async fn reject(&self, id: &str) -> anyhow::Result<bool> {
        self.transition(id, ProposalStatus::Rejected).await
    }

    pub async fn get(&self, id: &str) -> anyhow::Result<Option<UserPrefProposal>> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<UserPrefProposal>> {
            let guard = conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let mut stmt = guard.prepare(
                "SELECT id, task_id, agent_id, kind, content, confidence, evidence_json, replaces_id, status, created_at, reviewed_at
                 FROM user_pref_proposals WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            if let Some(row) = rows.next()? {
                Ok(Some(row_to_proposal(row)?))
            } else {
                Ok(None)
            }
        })
        .await
        .context("spawn_blocking get proposal")?
    }

    /// Mark pending proposals older than `days` as Expired (does not DELETE
    /// — review history is preserved). Returns the count of rows transitioned.
    pub async fn mark_expired_older_than_days(&self, days: i64) -> anyhow::Result<usize> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
            let now = chrono::Utc::now().to_rfc3339();
            let n = guard.execute(
                "UPDATE user_pref_proposals
                 SET status = 'expired', reviewed_at = ?1
                 WHERE status = 'pending' AND created_at < ?2",
                params![now, cutoff],
            )?;
            Ok(n)
        })
        .await
        .context("spawn_blocking mark_expired proposals")?
    }

    pub async fn stats(&self) -> anyhow::Result<ProposalStats> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<ProposalStats> {
            let guard = conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let count = |status: &str| -> anyhow::Result<u64> {
                Ok(guard.query_row(
                    "SELECT COUNT(*) FROM user_pref_proposals WHERE status = ?1",
                    params![status],
                    |r| r.get::<_, i64>(0),
                )? as u64)
            };
            let cutoff = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
            let stale = guard.query_row(
                "SELECT COUNT(*) FROM user_pref_proposals WHERE status = 'pending' AND created_at < ?1",
                params![cutoff],
                |r| r.get::<_, i64>(0),
            )? as u64;
            Ok(ProposalStats {
                pending: count("pending")?,
                accepted: count("accepted")?,
                rejected: count("rejected")?,
                expired: count("expired")?,
                stale_over_30d: stale,
            })
        })
        .await
        .context("spawn_blocking stats proposals")?
    }

    async fn transition(&self, id: &str, status: ProposalStatus) -> anyhow::Result<bool> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let updated = guard.execute(
                "UPDATE user_pref_proposals
                 SET status = ?1, reviewed_at = ?2
                 WHERE id = ?3 AND status = 'pending'",
                params![status.as_str(), chrono::Utc::now().to_rfc3339(), id],
            )?;
            Ok(updated > 0)
        })
        .await
        .context("spawn_blocking transition proposal")?
    }

    async fn list_by_status(
        &self,
        status: ProposalStatus,
        limit: u32,
    ) -> anyhow::Result<Vec<UserPrefProposal>> {
        let conn = Arc::clone(&self.conn);
        let status = status.as_str().to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<UserPrefProposal>> {
            let guard = conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let mut stmt = guard.prepare(
                "SELECT id, task_id, agent_id, kind, content, confidence, evidence_json, replaces_id, status, created_at, reviewed_at
                 FROM user_pref_proposals
                 WHERE status = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![status, limit])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(row_to_proposal(row)?);
            }
            Ok(out)
        })
        .await
        .context("spawn_blocking list proposals")?
    }
}

fn row_to_proposal(row: &rusqlite::Row<'_>) -> anyhow::Result<UserPrefProposal> {
    let task_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let kind: String = row.get(3)?;
    let status: String = row.get(8)?;
    let created_at: String = row.get(9)?;
    let reviewed_at: Option<String> = row.get(10)?;
    Ok(UserPrefProposal {
        id: row.get(0)?,
        task_id: task_id.parse()?,
        agent_id: agent_id.parse()?,
        kind: ProposalKind::parse(&kind)?,
        content: row.get(4)?,
        confidence: row.get(5)?,
        evidence: serde_json::from_str(&row.get::<_, String>(6)?)?,
        replaces: row.get(7)?,
        status: ProposalStatus::parse(&status)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&chrono::Utc),
        reviewed_at: reviewed_at
            .map(|s| {
                chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&chrono::Utc))
            })
            .transpose()?,
    })
}

fn pref_patterns() -> &'static [Regex] {
    static PATS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)\b(i prefer|i like|please|always|never|use)\b").unwrap(),
            Regex::new(r"(?i)\b(bullet points|concise|detailed|brief|step-by-step)\b").unwrap(),
            Regex::new(r"(?i)\b(timezone|tone|format|style)\b").unwrap(),
        ]
    })
}

pub fn heuristic_propose(
    task_id: TaskID,
    agent_id: AgentID,
    user_messages: &[String],
    max_per_task: usize,
) -> Vec<UserPrefProposal> {
    let mut out = Vec::new();
    for msg in user_messages.iter().rev().take(20) {
        let m = msg.trim();
        if m.len() < 12 || m.len() > 240 {
            continue;
        }
        if pref_patterns().iter().any(|r| r.is_match(m)) {
            out.push(UserPrefProposal {
                id: uuid::Uuid::new_v4().to_string(),
                task_id,
                agent_id,
                kind: ProposalKind::Add,
                content: m.to_string(),
                confidence: 0.62,
                evidence: vec![m.to_string()],
                replaces: None,
                status: ProposalStatus::Pending,
                created_at: chrono::Utc::now(),
                reviewed_at: None,
            });
        }
        if out.len() >= max_per_task {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(
        task_id: TaskID,
        agent_id: AgentID,
        confidence: f32,
        evidence: Vec<String>,
    ) -> UserPrefProposal {
        UserPrefProposal {
            id: uuid::Uuid::new_v4().to_string(),
            task_id,
            agent_id,
            kind: ProposalKind::Add,
            content: "User prefers terse answers".to_string(),
            confidence,
            evidence,
            replaces: None,
            status: ProposalStatus::Pending,
            created_at: chrono::Utc::now(),
            reviewed_at: None,
        }
    }

    async fn store() -> UserPrefProposalStore {
        let dir = tempdir().unwrap();
        let path = dir.path().join("proposals.db");
        let s = UserPrefProposalStore::open(path).await.unwrap();
        // Hold tempdir alive via a leaked box — fine for tests; per-test fresh dir.
        Box::leak(Box::new(dir));
        s
    }

    #[tokio::test]
    async fn insert_and_list() {
        let s = store().await;
        let p = sample(TaskID::new(), AgentID::new(), 0.8, vec!["t1".into()]);
        let out = s.insert_many(std::slice::from_ref(&p)).await.unwrap();
        assert_eq!(out.inserted.len(), 1);
        assert_eq!(out.rejected, 0);
        let pending = s.list_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, p.id);
    }

    #[tokio::test]
    async fn rejects_low_confidence() {
        let s = store().await;
        let p = sample(TaskID::new(), AgentID::new(), 0.3, vec!["t1".into()]);
        let out = s.insert_many(&[p]).await.unwrap();
        assert_eq!(out.inserted.len(), 0);
        assert_eq!(out.rejected, 1);
    }

    #[tokio::test]
    async fn evidence_capped() {
        let s = store().await;
        let evidence: Vec<String> = (0..10).map(|i| format!("turn {i}")).collect();
        let p = sample(TaskID::new(), AgentID::new(), 0.8, evidence);
        let out = s.insert_many(std::slice::from_ref(&p)).await.unwrap();
        assert_eq!(out.inserted.len(), 1);
        let stored = s.get(&p.id).await.unwrap().unwrap();
        assert_eq!(stored.evidence.len(), MAX_EVIDENCE_ENTRIES);
    }

    #[tokio::test]
    async fn per_task_pending_cap() {
        let s = store().await;
        let task = TaskID::new();
        let agent = AgentID::new();
        let batch: Vec<UserPrefProposal> = (0..5)
            .map(|_| sample(task, agent, 0.8, vec!["t".into()]))
            .collect();
        let out = s.insert_many(&batch).await.unwrap();
        assert_eq!(out.inserted.len(), MAX_PENDING_PER_TASK);
        assert_eq!(out.rejected, 5 - MAX_PENDING_PER_TASK);
    }

    #[tokio::test]
    async fn accept_marks_status_once() {
        let s = store().await;
        let p = sample(TaskID::new(), AgentID::new(), 0.8, vec!["t".into()]);
        s.insert_many(std::slice::from_ref(&p)).await.unwrap();
        assert!(s.accept(&p.id).await.unwrap());
        // Idempotent: second accept on already-accepted is a no-op (returns false).
        assert!(!s.accept(&p.id).await.unwrap());
        let stored = s.get(&p.id).await.unwrap().unwrap();
        assert_eq!(stored.status, ProposalStatus::Accepted);
        assert!(stored.reviewed_at.is_some());
    }

    #[tokio::test]
    async fn reject_marks_status() {
        let s = store().await;
        let p = sample(TaskID::new(), AgentID::new(), 0.8, vec!["t".into()]);
        s.insert_many(std::slice::from_ref(&p)).await.unwrap();
        assert!(s.reject(&p.id).await.unwrap());
        let stored = s.get(&p.id).await.unwrap().unwrap();
        assert_eq!(stored.status, ProposalStatus::Rejected);
    }

    #[tokio::test]
    async fn mark_expired_preserves_history() {
        let s = store().await;
        let mut p = sample(TaskID::new(), AgentID::new(), 0.8, vec!["t".into()]);
        p.created_at = chrono::Utc::now() - chrono::Duration::days(31);
        s.insert_many(std::slice::from_ref(&p)).await.unwrap();
        let n = s.mark_expired_older_than_days(30).await.unwrap();
        assert_eq!(n, 1);
        let stored = s.get(&p.id).await.unwrap().unwrap();
        assert_eq!(stored.status, ProposalStatus::Expired);
    }

    #[tokio::test]
    async fn unique_pending_per_replaces() {
        let s = store().await;
        let agent = AgentID::new();
        let prior_id = uuid::Uuid::new_v4().to_string();
        let mut a = sample(TaskID::new(), agent, 0.8, vec!["t".into()]);
        a.replaces = Some(prior_id.clone());
        let mut b = sample(TaskID::new(), agent, 0.8, vec!["t".into()]);
        b.replaces = Some(prior_id);
        let out = s.insert_many(&[a, b]).await.unwrap();
        assert_eq!(out.inserted.len(), 1);
        assert_eq!(out.rejected, 1);
    }
}
