use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Persists per-agent tool usage and computes recency-weighted rank scores.
///
/// Score formula: count * exp(-age_hours / 168)  (1-week half-life)
///
/// All rusqlite calls go through spawn_blocking — never called directly in async context.
pub struct ToolUsageStore {
    conn: Arc<Mutex<Connection>>,
}

impl ToolUsageStore {
    pub fn open(path: &Path) -> Result<Self, anyhow::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS tool_usage (
                 agent_id     TEXT NOT NULL,
                 tool_name    TEXT NOT NULL,
                 count        INTEGER NOT NULL DEFAULT 0,
                 last_used_at INTEGER NOT NULL,
                 PRIMARY KEY (agent_id, tool_name)
             );
             CREATE INDEX IF NOT EXISTS idx_tool_usage_agent ON tool_usage(agent_id);",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Record a tool invocation. Non-blocking — queues a write to be committed immediately.
    pub async fn record(&self, agent_id: &str, tool_name: &str) {
        let conn = Arc::clone(&self.conn);
        let agent_id = agent_id.to_string();
        let tool_name = tool_name.to_string();
        let now = chrono::Utc::now().timestamp();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = conn.lock() {
                let _ = conn.execute(
                    "INSERT INTO tool_usage (agent_id, tool_name, count, last_used_at)
                     VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT(agent_id, tool_name) DO UPDATE SET
                         count = count + 1,
                         last_used_at = excluded.last_used_at",
                    params![agent_id, tool_name, now],
                );
            }
        })
        .await;
    }

    /// Snapshot of score = count * exp(-age_hours / 168) for all tools used by agent.
    pub async fn rank_snapshot(&self, agent_id: &str) -> HashMap<String, f64> {
        let conn = Arc::clone(&self.conn);
        let agent_id = agent_id.to_string();
        let now = chrono::Utc::now().timestamp() as f64;
        tokio::task::spawn_blocking(move || {
            let Ok(conn) = conn.lock() else {
                return HashMap::new();
            };
            let Ok(mut stmt) = conn.prepare(
                "SELECT tool_name, count, last_used_at FROM tool_usage WHERE agent_id = ?1",
            ) else {
                return HashMap::new();
            };
            let Ok(rows) = stmt.query_map(params![agent_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            }) else {
                return HashMap::new();
            };
            let mut scores = HashMap::new();
            for row in rows.flatten() {
                let (tool_name, count, last_used_at) = row;
                let age_hours = ((now - last_used_at as f64).max(0.0)) / 3600.0;
                let score = (count as f64) * f64::exp(-age_hours / 168.0);
                scores.insert(tool_name, score);
            }
            scores
        })
        .await
        .unwrap_or_default()
    }

    /// Flush — used by tests to ensure writes are visible before querying.
    pub async fn flush(&self) -> Result<(), anyhow::Error> {
        // All writes are synchronous in this implementation; flush is a no-op.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn record_and_rank_snapshot() {
        let dir = tempdir().unwrap();
        let store = ToolUsageStore::open(&dir.path().join("u.db")).unwrap();
        store.record("a1", "file-read").await;
        store.record("a1", "file-read").await;
        store.record("a1", "web-fetch").await;
        store.flush().await.unwrap();
        let snap = store.rank_snapshot("a1").await;
        assert!(snap["file-read"] > snap["web-fetch"]);
    }

    #[tokio::test]
    async fn parameterized_sql_no_injection() {
        let dir = tempdir().unwrap();
        let store = ToolUsageStore::open(&dir.path().join("u.db")).unwrap();
        let evil = "x'; DROP TABLE tool_usage; --";
        store.record("a1", evil).await;
        store.flush().await.unwrap();
        let snap = store.rank_snapshot("a1").await;
        assert!(snap.contains_key(evil), "evil key should be stored as-is");
    }
}
