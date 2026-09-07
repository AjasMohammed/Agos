//! Canonical SQLite store primitives for AgentOS.
//!
//! Provides [`open_store`] and [`StoreHandle`] so every SQLite store in the
//! workspace uses the same PRAGMA configuration, migration runner, and
//! `spawn_blocking` dispatch pattern.

use std::path::Path;
use std::sync::{Arc, Mutex};

/// A slice of SQL DDL statements applied as migrations, indexed by version.
///
/// `MIGRATIONS[0]` is applied when `user_version == 0`, bringing the DB to
/// version 1.  `MIGRATIONS[1]` is applied when `user_version == 1`, and so on.
/// The length of the slice determines the final `user_version` of a fully
/// migrated database.
pub type Migrations = &'static [&'static str];

/// Error type for all storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("join error: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A cheaply-cloneable handle to an open SQLite connection.
///
/// All access goes through [`exec`][StoreHandle::exec] or
/// [`exec_mut`][StoreHandle::exec_mut], which dispatch the closure to a
/// `spawn_blocking` thread so the async runtime is never blocked.
#[derive(Clone)]
pub struct StoreHandle {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl StoreHandle {
    /// Wrap an already-opened [`rusqlite::Connection`] in a `StoreHandle`.
    ///
    /// This is provided for stores that must keep a synchronous `new()` API for
    /// backward compatibility.  The caller is responsible for setting PRAGMAs
    /// and applying any schema migrations before calling this function.
    pub fn from_conn(conn: rusqlite::Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    /// Run a read-only closure on the connection.
    pub async fn exec<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().expect("StoreHandle mutex poisoned");
            f(&guard).map_err(StoreError::Sqlite)
        })
        .await?
    }

    /// Run a mutable closure on the connection (e.g. INSERT/UPDATE/DELETE).
    pub async fn exec_mut<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().expect("StoreHandle mutex poisoned");
            f(&mut guard).map_err(StoreError::Sqlite)
        })
        .await?
    }
}

/// Open (or create) a SQLite database at `path`, apply any pending migrations,
/// and return a [`StoreHandle`].
///
/// # Behaviour
///
/// 1. Parent directories are created if they do not exist.
/// 2. The connection is opened with these PRAGMAs:
///    - `journal_mode = WAL`
///    - `synchronous = NORMAL`
///    - `foreign_keys = ON`
///    - `busy_timeout = 5000` (ms)
///    - `temp_store = MEMORY`
/// 3. Migrations are applied in order using `PRAGMA user_version` tracking.
///    Each entry in `migrations` is a complete SQL statement (or batch) that
///    advances the database by one version.  Already-applied migrations are
///    skipped, so this function is idempotent.
pub async fn open_store(path: &Path, migrations: Migrations) -> Result<StoreHandle, StoreError> {
    let path = path.to_path_buf();
    let conn = tokio::task::spawn_blocking(move || -> Result<rusqlite::Connection, StoreError> {
        // Create parent directories if needed.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = rusqlite::Connection::open(&path)?;

        // Restrict the DB file to owner read/write. These stores hold task,
        // schedule, workspace, and other operational state in plaintext;
        // without this they inherit the process umask (often world-readable).
        // Best-effort — a permissions failure must not block opening the store.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            {
                eprintln!(
                    "warning: failed to set 0600 on store {}: {e}",
                    path.display()
                );
            }
        }

        // Configure connection-level PRAGMAs.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             PRAGMA temp_store=MEMORY;",
        )?;

        // Apply pending migrations.
        let current_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let mut version = current_version as usize;

        for migration in migrations.iter().skip(version) {
            tracing::debug!(
                path = %path.display(),
                version = version,
                "applying migration"
            );
            conn.execute_batch(migration)?;
            version += 1;
            // user_version must be set with a literal; rusqlite doesn't support
            // binding parameters in PRAGMA statements.
            conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
        }

        Ok(conn)
    })
    .await??;

    Ok(StoreHandle {
        conn: Arc::new(Mutex::new(conn)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const MIGRATIONS: super::Migrations = &[
        "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        "ALTER TABLE kv ADD COLUMN updated_at TEXT;",
    ];

    #[tokio::test]
    async fn open_and_migrate() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.db");
        let store = open_store(&path, MIGRATIONS).await.unwrap();

        // Should be able to insert after migrations.
        store
            .exec_mut(|conn| {
                conn.execute(
                    "INSERT INTO kv (key, value) VALUES (?1, ?2)",
                    rusqlite::params!["hello", "world"],
                )
            })
            .await
            .unwrap();

        let val: String = store
            .exec(|conn| {
                conn.query_row(
                    "SELECT value FROM kv WHERE key = ?1",
                    rusqlite::params!["hello"],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(val, "world");
    }

    #[tokio::test]
    async fn migration_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("idem.db");

        // Open twice — second open must not fail on already-applied migrations.
        open_store(&path, MIGRATIONS).await.unwrap();
        open_store(&path, MIGRATIONS).await.unwrap();
    }

    #[tokio::test]
    async fn creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("deep/nested/dir/test.db");
        open_store(&path, &[]).await.unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn store_handle_clone_shares_connection() {
        let tmp = TempDir::new().unwrap();
        let store = open_store(
            &tmp.path().join("shared.db"),
            &["CREATE TABLE t (n INTEGER);"],
        )
        .await
        .unwrap();

        let store2 = store.clone();
        store
            .exec_mut(|conn| conn.execute("INSERT INTO t VALUES (42)", []))
            .await
            .unwrap();

        let n: i64 = store2
            .exec(|conn| conn.query_row("SELECT n FROM t", [], |r| r.get(0)))
            .await
            .unwrap();
        assert_eq!(n, 42);
    }
}
