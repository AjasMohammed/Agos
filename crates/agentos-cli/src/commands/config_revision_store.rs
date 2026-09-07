//! Offline SQLite revision store for `agentos config set` / `rollback`.
//!
//! Every `config set` snapshots the **pre-write** file content here before
//! editing, so a bad change can be reverted with `config rollback <rev>`. The
//! CLI runs offline (like `tool keygen`), so the store lives with the CLI command
//! rather than the kernel. Rollback writes a stored snapshot back to the config
//! path; the kernel's `ConfigWatcher` hot-reloads it like any manual edit — no
//! new reload mechanism.

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::PathBuf;

/// Maximum number of revisions retained; older rows are pruned on each snapshot.
const MAX_REVISIONS: i64 = 200;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS config_revisions (
    rev        INTEGER PRIMARY KEY AUTOINCREMENT,
    content    TEXT NOT NULL,
    key        TEXT,
    old_value  TEXT,
    new_value  TEXT,
    created_at TEXT NOT NULL
);";

/// One row of config revision history.
#[derive(Debug, Clone)]
pub struct RevisionRow {
    pub rev: i64,
    pub key: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub created_at: String,
}

/// Resolve the revisions DB path: `$AGENTOS_CONFIG_REVISIONS`, else
/// `config_revisions.db` next to the config file.
pub fn revisions_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("AGENTOS_CONFIG_REVISIONS") {
        return PathBuf::from(p);
    }
    let cfg = super::config_cmd::config_path();
    cfg.parent()
        .map(|d| d.join("config_revisions.db"))
        .unwrap_or_else(|| PathBuf::from("config_revisions.db"))
}

fn open() -> anyhow::Result<Connection> {
    let path = revisions_db_path();
    let conn = Connection::open(&path)
        .with_context(|| format!("open config revisions DB at {}", path.display()))?;
    conn.execute_batch("PRAGMA busy_timeout = 5000;")
        .context("revisions PRAGMA setup")?;
    conn.execute_batch(SCHEMA)
        .context("revisions schema creation")?;
    Ok(conn)
}

/// Snapshot the **pre-write** config `content` along with the change metadata.
/// Returns the new revision id.
pub fn snapshot(content: &str, key: &str, old: Option<&str>, new: &str) -> anyhow::Result<i64> {
    let conn = open()?;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO config_revisions (content, key, old_value, new_value, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![content, key, old, new, now],
    )
    .context("insert config revision")?;
    let rev = conn.last_insert_rowid();
    // Cap history so the DB can't grow without bound (each row holds a full file
    // copy). Config files are tiny, so retaining the newest MAX_REVISIONS is ample.
    conn.execute(
        "DELETE FROM config_revisions WHERE rev NOT IN
           (SELECT rev FROM config_revisions ORDER BY rev DESC LIMIT ?1)",
        params![MAX_REVISIONS],
    )
    .context("prune old config revisions")?;
    Ok(rev)
}

/// List revisions, newest first, up to `limit`.
pub fn list(limit: usize) -> anyhow::Result<Vec<RevisionRow>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT rev, key, old_value, new_value, created_at
         FROM config_revisions ORDER BY rev DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit as i64], |r: &Row| {
            Ok(RevisionRow {
                rev: r.get(0)?,
                key: r.get(1)?,
                old_value: r.get(2)?,
                new_value: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("read config revisions")?;
    Ok(rows)
}

/// Fetch the stored file content for a revision, if present.
pub fn get(rev: i64) -> anyhow::Result<Option<String>> {
    let conn = open()?;
    let content = conn
        .query_row(
            "SELECT content FROM config_revisions WHERE rev = ?1",
            params![rev],
            |r: &Row| r.get::<_, String>(0),
        )
        .optional()
        .context("read config revision content")?;
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Point the store at a temp DB for the duration of a test.
    struct TmpDb {
        _dir: tempfile::TempDir,
    }
    impl TmpDb {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::env::set_var("AGENTOS_CONFIG_REVISIONS", dir.path().join("rev.db"));
            Self { _dir: dir }
        }
    }
    impl Drop for TmpDb {
        fn drop(&mut self) {
            std::env::remove_var("AGENTOS_CONFIG_REVISIONS");
        }
    }

    #[test]
    #[serial]
    fn snapshot_then_list_newest_first() {
        let _g = TmpDb::new();
        let r1 = snapshot("a = 1\n", "a", Some("0"), "1").unwrap();
        let r2 = snapshot("a = 2\n", "a", Some("1"), "2").unwrap();
        assert!(r2 > r1);
        let rows = list(10).unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].rev, r2);
        assert_eq!(rows[0].new_value.as_deref(), Some("2"));
        assert_eq!(rows[1].rev, r1);
        // Stored content is the pre-write snapshot passed in.
        assert_eq!(get(r1).unwrap().as_deref(), Some("a = 1\n"));
        assert_eq!(get(r2).unwrap().as_deref(), Some("a = 2\n"));
    }

    #[test]
    #[serial]
    fn get_missing_revision_is_none() {
        let _g = TmpDb::new();
        assert!(get(9999).unwrap().is_none());
    }

    #[test]
    #[serial]
    fn snapshot_prunes_to_cap() {
        let _g = TmpDb::new();
        let extra = 5;
        for i in 0..(MAX_REVISIONS + extra) {
            snapshot(&format!("v = {i}\n"), "v", None, &i.to_string()).unwrap();
        }
        // Only the newest MAX_REVISIONS rows survive.
        let rows = list((MAX_REVISIONS + extra) as usize).unwrap();
        assert_eq!(rows.len() as i64, MAX_REVISIONS);
        // The oldest `extra` revisions (revs 1..=extra) were pruned.
        for old in 1..=extra {
            assert!(get(old).unwrap().is_none(), "rev {old} should be pruned");
        }
        // The most recent revision is still present.
        assert!(get(MAX_REVISIONS + extra).unwrap().is_some());
    }
}
