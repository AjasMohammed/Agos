use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::models::{ToolEntry, ToolSearchResult};

/// Thread-safe handle to the registry SQLite database.
#[derive(Clone)]
pub struct RegistryDb {
    conn: Arc<Mutex<Connection>>,
}

impl RegistryDb {
    /// Open (or create) the registry database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open registry database")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database (for tests).
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| anyhow::anyhow!("database lock poisoned: {}", e))
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tools (
                name          TEXT NOT NULL,
                version       TEXT NOT NULL,
                description   TEXT NOT NULL DEFAULT '',
                author        TEXT NOT NULL DEFAULT '',
                author_pubkey TEXT NOT NULL DEFAULT '',
                signature     TEXT NOT NULL DEFAULT '',
                tags          TEXT NOT NULL DEFAULT '[]',
                manifest_toml TEXT NOT NULL,
                downloads     INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                PRIMARY KEY (name, version)
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS tools_fts USING fts5(
                name, description, tags, author,
                content=tools,
                content_rowid=rowid
            );

            CREATE TRIGGER IF NOT EXISTS tools_ai AFTER INSERT ON tools BEGIN
                INSERT INTO tools_fts(rowid, name, description, tags, author)
                VALUES (new.rowid, new.name, new.description, new.tags, new.author);
            END;

            CREATE TRIGGER IF NOT EXISTS tools_ad AFTER DELETE ON tools BEGIN
                INSERT INTO tools_fts(tools_fts, rowid, name, description, tags, author)
                VALUES ('delete', old.rowid, old.name, old.description, old.tags, old.author);
            END;

            CREATE TRIGGER IF NOT EXISTS tools_au AFTER UPDATE ON tools BEGIN
                INSERT INTO tools_fts(tools_fts, rowid, name, description, tags, author)
                VALUES ('delete', old.rowid, old.name, old.description, old.tags, old.author);
                INSERT INTO tools_fts(rowid, name, description, tags, author)
                VALUES (new.rowid, new.name, new.description, new.tags, new.author);
            END;",
        )?;
        Ok(())
    }

    /// Insert a new tool version. Returns an error if the (name, version) already exists.
    pub fn insert_tool(&self, entry: &ToolEntry) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tools (name, version, description, author, author_pubkey, signature, tags, manifest_toml, downloads, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?9)",
            params![
                entry.name,
                entry.version,
                entry.description,
                entry.author,
                entry.author_pubkey,
                entry.signature,
                serde_json::to_string(&entry.tags).unwrap_or_default(),
                entry.manifest_toml,
                entry.created_at,
            ],
        )
        .context("Failed to insert tool (name+version may already exist)")?;
        Ok(())
    }

    /// Full-text search across tool name, description, tags, and author.
    /// Sanitizes the query to prevent FTS5 operator injection.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<ToolSearchResult>> {
        if query.trim().is_empty() {
            return self.list_tools(limit, 0);
        }
        // Escape FTS5 special syntax: wrap each token in double-quotes,
        // escaping any embedded double-quotes by doubling them.
        let safe_query: String = query
            .split_whitespace()
            .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");

        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT t.name, t.version, t.description, t.author, t.downloads, t.tags
             FROM tools_fts f
             JOIN tools t ON f.rowid = t.rowid
             WHERE tools_fts MATCH ?1
             ORDER BY t.downloads DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![safe_query, limit], |row| {
            let tags_str: String = row.get(5)?;
            Ok(ToolSearchResult {
                name: row.get(0)?,
                version: row.get(1)?,
                description: row.get(2)?,
                author: row.get(3)?,
                downloads: row.get(4)?,
                tags: serde_json::from_str(&tags_str).unwrap_or_default(),
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// List all tools, paginated, sorted by downloads descending.
    /// Returns only the latest version of each tool.
    pub fn list_tools(&self, limit: u32, offset: u32) -> Result<Vec<ToolSearchResult>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT name, version, description, author, downloads, tags
             FROM tools
             WHERE rowid IN (SELECT MAX(rowid) FROM tools GROUP BY name)
             ORDER BY downloads DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit, offset], |row| {
            let tags_str: String = row.get(5)?;
            Ok(ToolSearchResult {
                name: row.get(0)?,
                version: row.get(1)?,
                description: row.get(2)?,
                author: row.get(3)?,
                downloads: row.get(4)?,
                tags: serde_json::from_str(&tags_str).unwrap_or_default(),
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Get a specific tool by name (latest version by insertion order).
    pub fn get_tool(&self, name: &str) -> Result<Option<ToolEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT name, version, description, author, author_pubkey, signature, tags, manifest_toml, downloads, created_at, updated_at
             FROM tools
             WHERE name = ?1
             ORDER BY rowid DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            let tags_str: String = row.get(6)?;
            Ok(ToolEntry {
                name: row.get(0)?,
                version: row.get(1)?,
                description: row.get(2)?,
                author: row.get(3)?,
                author_pubkey: row.get(4)?,
                signature: row.get(5)?,
                tags: serde_json::from_str(&tags_str).unwrap_or_default(),
                manifest_toml: row.get(7)?,
                downloads: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })?;

        match rows.next() {
            Some(Ok(entry)) => Ok(Some(entry)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Get a specific tool version.
    pub fn get_tool_version(&self, name: &str, version: &str) -> Result<Option<ToolEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT name, version, description, author, author_pubkey, signature, tags, manifest_toml, downloads, created_at, updated_at
             FROM tools
             WHERE name = ?1 AND version = ?2",
        )?;
        let mut rows = stmt.query_map(params![name, version], |row| {
            let tags_str: String = row.get(6)?;
            Ok(ToolEntry {
                name: row.get(0)?,
                version: row.get(1)?,
                description: row.get(2)?,
                author: row.get(3)?,
                author_pubkey: row.get(4)?,
                signature: row.get(5)?,
                tags: serde_json::from_str(&tags_str).unwrap_or_default(),
                manifest_toml: row.get(7)?,
                downloads: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })?;

        match rows.next() {
            Some(Ok(entry)) => Ok(Some(entry)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Increment the download counter for a tool version.
    pub fn increment_downloads(&self, name: &str, version: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE tools SET downloads = downloads + 1 WHERE name = ?1 AND version = ?2",
            params![name, version],
        )?;
        Ok(())
    }

    /// List all versions of a tool.
    pub fn list_versions(&self, name: &str) -> Result<Vec<ToolSearchResult>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT name, version, description, author, downloads, tags
             FROM tools
             WHERE name = ?1
             ORDER BY rowid DESC",
        )?;
        let rows = stmt.query_map(params![name], |row| {
            let tags_str: String = row.get(5)?;
            Ok(ToolSearchResult {
                name: row.get(0)?,
                version: row.get(1)?,
                description: row.get(2)?,
                author: row.get(3)?,
                downloads: row.get(4)?,
                tags: serde_json::from_str(&tags_str).unwrap_or_default(),
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ToolEntry;

    fn test_entry() -> ToolEntry {
        ToolEntry {
            name: "test-tool".into(),
            version: "1.0.0".into(),
            description: "A test tool".into(),
            author: "alice".into(),
            author_pubkey: "ed25519:abc123".into(),
            signature: "ed25519:sig456".into(),
            tags: vec!["testing".into(), "example".into()],
            manifest_toml: "[manifest]\nname = \"test-tool\"".into(),
            downloads: 0,
            created_at: "2026-03-26T00:00:00Z".into(),
            updated_at: "2026-03-26T00:00:00Z".into(),
        }
    }

    #[test]
    fn insert_and_get() {
        let db = RegistryDb::open_memory().unwrap();
        let entry = test_entry();
        db.insert_tool(&entry).unwrap();

        let fetched = db.get_tool("test-tool").unwrap().unwrap();
        assert_eq!(fetched.name, "test-tool");
        assert_eq!(fetched.version, "1.0.0");
        assert_eq!(fetched.author, "alice");
    }

    #[test]
    fn search_finds_tool() {
        let db = RegistryDb::open_memory().unwrap();
        db.insert_tool(&test_entry()).unwrap();

        let results = db.search("test", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "test-tool");
    }

    #[test]
    fn search_empty_query_returns_all() {
        let db = RegistryDb::open_memory().unwrap();
        db.insert_tool(&test_entry()).unwrap();
        let results = db.search("", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_special_chars_do_not_crash() {
        let db = RegistryDb::open_memory().unwrap();
        db.insert_tool(&test_entry()).unwrap();
        // These would crash with raw FTS5 MATCH without sanitization.
        assert!(db.search("\"", 10).is_ok());
        assert!(db.search("AND AND", 10).is_ok());
        assert!(db.search("*", 10).is_ok());
        assert!(db.search("name:", 10).is_ok());
    }

    #[test]
    fn duplicate_insert_fails() {
        let db = RegistryDb::open_memory().unwrap();
        let entry = test_entry();
        db.insert_tool(&entry).unwrap();
        assert!(db.insert_tool(&entry).is_err());
    }

    #[test]
    fn increment_downloads_works() {
        let db = RegistryDb::open_memory().unwrap();
        db.insert_tool(&test_entry()).unwrap();
        db.increment_downloads("test-tool", "1.0.0").unwrap();
        let fetched = db.get_tool("test-tool").unwrap().unwrap();
        assert_eq!(fetched.downloads, 1);
    }
}
