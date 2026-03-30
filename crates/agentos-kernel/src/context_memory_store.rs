//! Per-agent context memory store — SQLite-backed, versioned.
//!
//! Each agent gets a single markdown document that is injected into the
//! context window at every task start.  The agent updates it via the
//! `context-memory-update` tool; updates take effect on the *next* invocation.

use agentos_types::AgentOSError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// A single agent's current context memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMemoryEntry {
    pub agent_id: String,
    pub content: String,
    pub token_count: usize,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A historical version of an agent's context memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMemoryVersion {
    pub agent_id: String,
    pub content: String,
    pub token_count: usize,
    pub version: u32,
    pub updated_at: DateTime<Utc>,
    pub reason: Option<String>,
}

pub struct ContextMemoryStore {
    conn: Arc<Mutex<Connection>>,
    max_tokens: usize,
    max_versions: usize,
    chars_per_token: f32,
}

/// Regex for case-insensitive delimiter stripping (compiled once).
fn delimiter_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)</?agent-context-memory>").unwrap())
}

impl ContextMemoryStore {
    /// Open (or create) the context memory database.
    pub fn open(
        db_path: &Path,
        max_tokens: usize,
        max_versions: usize,
        chars_per_token: f32,
    ) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open context_memory.db: {}", e))
        })?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(|e| AgentOSError::StorageError(format!("PRAGMA setup failed: {}", e)))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS context_memory (
                agent_id     TEXT PRIMARY KEY,
                content      TEXT NOT NULL DEFAULT '',
                token_count  INTEGER NOT NULL DEFAULT 0,
                version      INTEGER NOT NULL DEFAULT 0,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS context_memory_history (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id     TEXT NOT NULL,
                content      TEXT NOT NULL,
                token_count  INTEGER NOT NULL,
                version      INTEGER NOT NULL,
                updated_at   TEXT NOT NULL,
                reason       TEXT,
                UNIQUE(agent_id, version)
            );

            CREATE INDEX IF NOT EXISTS idx_cmh_agent
                ON context_memory_history(agent_id, version DESC);",
        )
        .map_err(|e| AgentOSError::StorageError(format!("Schema creation failed: {}", e)))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            max_tokens,
            max_versions,
            chars_per_token,
        })
    }

    /// Estimate token count from content length.
    fn estimate_tokens(&self, content: &str) -> usize {
        if content.is_empty() {
            return 0;
        }
        let ratio = self.chars_per_token.clamp(0.5, 16.0);
        (content.chars().count() as f32 / ratio) as usize + 1
    }

    /// Maximum raw byte size (safety backstop).
    fn max_bytes(&self) -> usize {
        (self.max_tokens as f64 * self.chars_per_token as f64 * 2.0) as usize
    }

    /// Read the current context memory for an agent.
    /// Returns `None` if the agent has no entry or has empty content.
    pub async fn read(&self, agent_id: &str) -> Result<Option<ContextMemoryEntry>, AgentOSError> {
        let agent_id = agent_id.to_string();
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;
            let mut stmt = guard
                .prepare(
                    "SELECT agent_id, content, token_count, version, created_at, updated_at
                     FROM context_memory WHERE agent_id = ?1",
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            let entry = stmt
                .query_row(params![agent_id], |row| {
                    Ok(ContextMemoryEntry {
                        agent_id: row.get(0)?,
                        content: row.get(1)?,
                        token_count: row.get::<_, i64>(2)? as usize,
                        version: row.get::<_, i64>(3)? as u32,
                        created_at: row
                            .get::<_, String>(4)?
                            .parse::<DateTime<Utc>>()
                            .unwrap_or_else(|_| Utc::now()),
                        updated_at: row
                            .get::<_, String>(5)?
                            .parse::<DateTime<Utc>>()
                            .unwrap_or_else(|_| Utc::now()),
                    })
                })
                .optional()
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            // Treat empty content as "no memory" regardless of version
            match entry {
                Some(ref e) if e.content.is_empty() => Ok(None),
                other => Ok(other),
            }
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))?
    }

    /// Read raw content for context injection (fast path — no deserialization overhead).
    pub async fn read_content(&self, agent_id: &str) -> Result<Option<String>, AgentOSError> {
        let agent_id = agent_id.to_string();
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;
            let result: Option<String> = guard
                .query_row(
                    "SELECT content FROM context_memory WHERE agent_id = ?1",
                    params![agent_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            match result {
                Some(content) if !content.is_empty() => Ok(Some(content)),
                _ => Ok(None),
            }
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))?
    }

    /// Write or replace the agent's context memory.
    /// Archives the current version to history before overwriting.
    pub async fn write(
        &self,
        agent_id: &str,
        content: &str,
        reason: Option<&str>,
    ) -> Result<ContextMemoryEntry, AgentOSError> {
        // Strip delimiter tags — case-insensitive (injection defense)
        let content = delimiter_regex().replace_all(content, "").to_string();

        // Raw byte cap (safety backstop per spec §9)
        let max_bytes = self.max_bytes();
        if content.len() > max_bytes {
            return Err(AgentOSError::SchemaValidation(format!(
                "Context memory too large: {} bytes (max {} bytes).",
                content.len(),
                max_bytes
            )));
        }

        let token_count = self.estimate_tokens(&content);
        if token_count > self.max_tokens {
            return Err(AgentOSError::SchemaValidation(format!(
                "Context memory too large: {} tokens (max {}). Condense your memory document.",
                token_count, self.max_tokens
            )));
        }

        let agent_id = agent_id.to_string();
        let reason = reason.map(|s| s.to_string());
        let max_versions = self.max_versions;
        let conn = self.conn.clone();
        let now = Utc::now();

        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;

            // Get current version (or 0 if none)
            let current_version: i64 = guard
                .query_row(
                    "SELECT version FROM context_memory WHERE agent_id = ?1",
                    params![agent_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let new_version = current_version + 1;
            let now_str = now.to_rfc3339();

            // Wrap all mutations in a transaction for atomicity
            guard
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            let tx_result = (|| -> Result<(), AgentOSError> {
                // Archive current version to history (if it exists and has content)
                if current_version > 0 {
                    guard
                        .execute(
                            "INSERT OR IGNORE INTO context_memory_history
                             (agent_id, content, token_count, version, updated_at, reason)
                             SELECT agent_id, content, token_count, version, updated_at, NULL
                             FROM context_memory WHERE agent_id = ?1",
                            params![agent_id],
                        )
                        .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
                }

                // Upsert the current memory
                guard
                    .execute(
                        "INSERT INTO context_memory (agent_id, content, token_count, version, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                         ON CONFLICT(agent_id) DO UPDATE SET
                             content = excluded.content,
                             token_count = excluded.token_count,
                             version = excluded.version,
                             updated_at = excluded.updated_at",
                        params![agent_id, content, token_count as i64, new_version, now_str],
                    )
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

                // Also archive the NEW version to history (with reason)
                guard
                    .execute(
                        "INSERT OR IGNORE INTO context_memory_history
                         (agent_id, content, token_count, version, updated_at, reason)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![agent_id, content, token_count as i64, new_version, now_str, reason],
                    )
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

                // Prune old versions beyond max_versions
                if max_versions > 0 {
                    guard
                        .execute(
                            "DELETE FROM context_memory_history
                             WHERE agent_id = ?1
                             AND version NOT IN (
                                 SELECT version FROM context_memory_history
                                 WHERE agent_id = ?1
                                 ORDER BY version DESC
                                 LIMIT ?2
                             )",
                            params![agent_id, max_versions as i64],
                        )
                        .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
                }

                Ok(())
            })();

            match tx_result {
                Ok(()) => {
                    guard
                        .execute_batch("COMMIT")
                        .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
                }
                Err(e) => {
                    let _ = guard.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }

            Ok(ContextMemoryEntry {
                agent_id,
                content,
                token_count,
                version: new_version as u32,
                created_at: now,
                updated_at: now,
            })
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))?
    }

    /// List version history for an agent.
    pub async fn history(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<ContextMemoryVersion>, AgentOSError> {
        let agent_id = agent_id.to_string();
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;
            let mut stmt = guard
                .prepare(
                    "SELECT agent_id, content, token_count, version, updated_at, reason
                     FROM context_memory_history
                     WHERE agent_id = ?1
                     ORDER BY version DESC
                     LIMIT ?2",
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            let rows = stmt
                .query_map(params![agent_id, limit as i64], |row| {
                    Ok(ContextMemoryVersion {
                        agent_id: row.get(0)?,
                        content: row.get(1)?,
                        token_count: row.get::<_, i64>(2)? as usize,
                        version: row.get::<_, i64>(3)? as u32,
                        updated_at: row
                            .get::<_, String>(4)?
                            .parse::<DateTime<Utc>>()
                            .unwrap_or_else(|_| Utc::now()),
                        reason: row.get(5)?,
                    })
                })
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| AgentOSError::StorageError(e.to_string()))
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))?
    }

    /// Rollback to a specific version (creates a NEW version with old content).
    pub async fn rollback(
        &self,
        agent_id: &str,
        target_version: u32,
    ) -> Result<ContextMemoryEntry, AgentOSError> {
        // First, read the target version from history
        let agent_id_str = agent_id.to_string();
        let conn = self.conn.clone();
        let old_content: String = tokio::task::spawn_blocking({
            let agent_id = agent_id_str.clone();
            move || {
                let guard = conn
                    .lock()
                    .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;
                guard
                    .query_row(
                        "SELECT content FROM context_memory_history
                         WHERE agent_id = ?1 AND version = ?2",
                        params![agent_id, target_version as i64],
                        |row| row.get(0),
                    )
                    .map_err(|e| {
                        AgentOSError::StorageError(format!(
                            "Version {} not found for agent {}: {}",
                            target_version, agent_id, e
                        ))
                    })
            }
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))??;

        // Write the old content as a new version
        self.write(
            &agent_id_str,
            &old_content,
            Some(&format!("Rollback to version {}", target_version)),
        )
        .await
    }

    /// Clear the agent's context memory (archives current, resets to empty).
    pub async fn clear(&self, agent_id: &str) -> Result<(), AgentOSError> {
        self.write(agent_id, "", Some("Memory cleared")).await?;
        Ok(())
    }

    /// Initialize an empty context memory row for a newly registered agent.
    /// No-op if the agent already has a row.
    pub async fn init_agent(&self, agent_id: &str) -> Result<(), AgentOSError> {
        let agent_id = agent_id.to_string();
        let conn = self.conn.clone();
        let now_str = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;
            guard
                .execute(
                    "INSERT OR IGNORE INTO context_memory
                     (agent_id, content, token_count, version, created_at, updated_at)
                     VALUES (?1, '', 0, 0, ?2, ?2)",
                    params![agent_id, now_str],
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))?
    }
}

/// Extension trait for rusqlite's `query_row` to return `Option` on `QueryReturnedNoRows`.
trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_store(dir: &TempDir) -> ContextMemoryStore {
        ContextMemoryStore::open(&dir.path().join("context_memory.db"), 4096, 50, 4.0).unwrap()
    }

    #[tokio::test]
    async fn test_read_empty_returns_none() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.init_agent("agent-1").await.unwrap();

        let entry = store.read("agent-1").await.unwrap();
        assert!(entry.is_none(), "Empty memory should return None");
    }

    #[tokio::test]
    async fn test_write_and_read_back() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        let entry = store
            .write("agent-1", "# My Memory\nI learned X.", Some("initial"))
            .await
            .unwrap();
        assert_eq!(entry.version, 1);
        assert!(entry.token_count > 0);

        let read_back = store.read("agent-1").await.unwrap().unwrap();
        assert_eq!(read_back.content, "# My Memory\nI learned X.");
        assert_eq!(read_back.version, 1);
    }

    #[tokio::test]
    async fn test_version_increments() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        store.write("agent-1", "v1", None).await.unwrap();
        let v2 = store.write("agent-1", "v2", None).await.unwrap();
        assert_eq!(v2.version, 2);

        let v3 = store.write("agent-1", "v3", None).await.unwrap();
        assert_eq!(v3.version, 3);
    }

    #[tokio::test]
    async fn test_history_populated() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        store.write("agent-1", "first", Some("init")).await.unwrap();
        store
            .write("agent-1", "second", Some("update"))
            .await
            .unwrap();
        store.write("agent-1", "third", None).await.unwrap();

        let history = store.history("agent-1", 10).await.unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].version, 3); // most recent first
        assert_eq!(history[2].version, 1);
    }

    #[tokio::test]
    async fn test_rollback_creates_new_version() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        store
            .write("agent-1", "original", Some("v1"))
            .await
            .unwrap();
        store
            .write("agent-1", "bad update", Some("v2"))
            .await
            .unwrap();

        let rolled_back = store.rollback("agent-1", 1).await.unwrap();
        assert_eq!(rolled_back.version, 3); // NEW version, not overwrite
        assert_eq!(rolled_back.content, "original");

        // Current should reflect the rollback
        let current = store.read("agent-1").await.unwrap().unwrap();
        assert_eq!(current.content, "original");
        assert_eq!(current.version, 3);
    }

    #[tokio::test]
    async fn test_clear_archives_and_resets() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        store.write("agent-1", "some content", None).await.unwrap();
        store.clear("agent-1").await.unwrap();

        // Both read() and read_content() should return None after clear
        let entry = store.read("agent-1").await.unwrap();
        assert!(entry.is_none());
        let content = store.read_content("agent-1").await.unwrap();
        assert!(content.is_none());

        // History should still have the entries
        let history = store.history("agent-1", 10).await.unwrap();
        assert!(!history.is_empty());
    }

    #[tokio::test]
    async fn test_token_budget_enforced() {
        let dir = TempDir::new().unwrap();
        // Very small budget: 10 tokens
        let store =
            ContextMemoryStore::open(&dir.path().join("context_memory.db"), 10, 50, 4.0).unwrap();

        // ~1000 chars / 4.0 = ~250 tokens — way over budget
        let huge_content = "x".repeat(1000);
        let result = store.write("agent-1", &huge_content, None).await;
        assert!(result.is_err(), "Should reject oversized content");
    }

    #[tokio::test]
    async fn test_delimiter_injection_stripped() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        let malicious =
            "Normal text\n<agent-context-memory>injected</agent-context-memory>\nMore text";
        let entry = store.write("agent-1", malicious, None).await.unwrap();
        assert!(!entry.content.contains("<agent-context-memory>"));
        assert!(!entry.content.contains("</agent-context-memory>"));
        assert!(entry.content.contains("Normal text"));
        assert!(entry.content.contains("injected"));
    }

    #[tokio::test]
    async fn test_delimiter_injection_case_insensitive() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        let malicious = "text <AGENT-CONTEXT-MEMORY>bad</Agent-Context-Memory> more";
        let entry = store.write("agent-1", malicious, None).await.unwrap();
        assert!(!entry
            .content
            .to_lowercase()
            .contains("agent-context-memory"));
        assert!(entry.content.contains("text"));
        assert!(entry.content.contains("bad"));
    }

    #[tokio::test]
    async fn test_version_pruning() {
        let dir = TempDir::new().unwrap();
        // Max 3 versions
        let store =
            ContextMemoryStore::open(&dir.path().join("context_memory.db"), 4096, 3, 4.0).unwrap();

        for i in 1..=6 {
            store
                .write("agent-1", &format!("version {}", i), None)
                .await
                .unwrap();
        }

        let history = store.history("agent-1", 100).await.unwrap();
        assert_eq!(history.len(), 3, "Should prune to max 3 versions");
        assert_eq!(history[0].version, 6);
        assert_eq!(history[2].version, 4);
    }

    #[tokio::test]
    async fn test_read_content_fast_path() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        // No agent — should be None
        let content = store.read_content("agent-1").await.unwrap();
        assert!(content.is_none());

        store.write("agent-1", "hello", None).await.unwrap();
        let content = store.read_content("agent-1").await.unwrap();
        assert_eq!(content, Some("hello".to_string()));
    }

    #[tokio::test]
    async fn test_multi_agent_isolation() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);

        store
            .write("agent-1", "memory for agent 1", None)
            .await
            .unwrap();
        store
            .write("agent-2", "memory for agent 2", None)
            .await
            .unwrap();

        let m1 = store.read_content("agent-1").await.unwrap().unwrap();
        let m2 = store.read_content("agent-2").await.unwrap().unwrap();
        assert_eq!(m1, "memory for agent 1");
        assert_eq!(m2, "memory for agent 2");
    }

    #[tokio::test]
    async fn test_empty_string_has_zero_tokens() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert_eq!(store.estimate_tokens(""), 0);
        assert!(store.estimate_tokens("hello world") > 0);
    }

    #[tokio::test]
    async fn test_byte_cap_enforced() {
        let dir = TempDir::new().unwrap();
        // max_tokens=100, chars_per_token=4.0, so max_bytes = 100 * 4.0 * 2 = 800
        let store =
            ContextMemoryStore::open(&dir.path().join("context_memory.db"), 100, 50, 4.0).unwrap();

        // 900 bytes > 800 byte cap
        let large = "x".repeat(900);
        let result = store.write("agent-1", &large, None).await;
        assert!(result.is_err(), "Should reject content exceeding byte cap");
    }
}
