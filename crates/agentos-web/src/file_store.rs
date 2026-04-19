use rusqlite::{params, params_from_iter, Connection};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct FileStore {
    conn: Mutex<Connection>,
    /// Absolute path to the uploads directory (data_dir/uploads/).
    pub uploads_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct UploadedFile {
    pub id: String,
    /// Sanitized name for display and @mention matching.
    pub name: String,
    /// Original filename as uploaded by the user.
    pub original_name: String,
    pub mime: String,
    pub size: u64,
    /// Absolute path on disk.
    pub path: String,
    pub tags: Vec<String>,
    pub uploaded_at: String,
}

impl FileStore {
    pub fn open(data_dir: &Path) -> Result<Self, rusqlite::Error> {
        let uploads_dir = data_dir.join("uploads");
        std::fs::create_dir_all(&uploads_dir).ok();

        let db_path = uploads_dir.join("file_registry.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS uploaded_files (
                 id            TEXT PRIMARY KEY,
                 name          TEXT NOT NULL,
                 original_name TEXT NOT NULL,
                 mime          TEXT NOT NULL DEFAULT 'application/octet-stream',
                 size          INTEGER NOT NULL DEFAULT 0,
                 path          TEXT NOT NULL,
                 tags          TEXT NOT NULL DEFAULT '',
                 uploaded_at   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_uploaded_files_name ON uploaded_files(name);",
        )?;

        let has_owner: bool = conn
            .prepare("PRAGMA table_info(uploaded_files)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .any(|col| col.as_deref() == Ok("owner_principal"));
        if !has_owner {
            conn.execute_batch(
                "ALTER TABLE uploaded_files ADD COLUMN owner_principal TEXT;",
            )?;
        }

        Ok(Self {
            conn: Mutex::new(conn),
            uploads_dir,
        })
    }

    pub fn register_file(
        &self,
        id: &str,
        original_name: &str,
        mime: &str,
        size: u64,
        path: &str,
        tags: &str,
        owner_principal: &str,
    ) -> Result<(), rusqlite::Error> {
        let name = sanitize_display_name(original_name);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO uploaded_files (id, name, original_name, mime, size, path, tags, uploaded_at, owner_principal)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), ?8)",
            params![
                id,
                name,
                original_name,
                mime,
                size as i64,
                path,
                tags,
                owner_principal
            ],
        )?;
        Ok(())
    }

    pub fn list_files(&self, owner_principal: &str) -> Result<Vec<UploadedFile>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, name, original_name, mime, size, path, tags, uploaded_at
             FROM uploaded_files
             WHERE (owner_principal = ?1 OR owner_principal IS NULL OR owner_principal = '')
             ORDER BY uploaded_at DESC",
        )?;
        let files = stmt
            .query_map(params![owner_principal], |row| {
                Ok(UploadedFile {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    original_name: row.get(2)?,
                    mime: row.get(3)?,
                    size: row.get::<_, i64>(4)? as u64,
                    path: row.get(5)?,
                    tags: parse_tags(row.get::<_, String>(6)?),
                    uploaded_at: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(files)
    }

    pub fn get_file(
        &self,
        id: &str,
        owner_principal: &str,
    ) -> Result<Option<UploadedFile>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, name, original_name, mime, size, path, tags, uploaded_at
             FROM uploaded_files
             WHERE id = ?1
               AND (owner_principal = ?2 OR owner_principal IS NULL OR owner_principal = '')",
        )?;
        let mut rows = stmt.query_map(params![id, owner_principal], |row| {
            Ok(UploadedFile {
                id: row.get(0)?,
                name: row.get(1)?,
                original_name: row.get(2)?,
                mime: row.get(3)?,
                size: row.get::<_, i64>(4)? as u64,
                path: row.get(5)?,
                tags: parse_tags(row.get::<_, String>(6)?),
                uploaded_at: row.get(7)?,
            })
        })?;
        rows.next().transpose()
    }

    /// Look up a file by display name or original name (for @mention resolution).
    pub fn find_by_name(
        &self,
        name: &str,
        owner_principal: &str,
    ) -> Result<Option<UploadedFile>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, name, original_name, mime, size, path, tags, uploaded_at
             FROM uploaded_files
             WHERE (name = ?1 OR original_name = ?1)
               AND (owner_principal = ?2 OR owner_principal IS NULL OR owner_principal = '')
             ORDER BY uploaded_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![name, owner_principal], |row| {
            Ok(UploadedFile {
                id: row.get(0)?,
                name: row.get(1)?,
                original_name: row.get(2)?,
                mime: row.get(3)?,
                size: row.get::<_, i64>(4)? as u64,
                path: row.get(5)?,
                tags: parse_tags(row.get::<_, String>(6)?),
                uploaded_at: row.get(7)?,
            })
        })?;
        rows.next().transpose()
    }

    /// Look up multiple files by IDs (for chat attachment resolution).
    pub fn get_files_by_ids(
        &self,
        ids: &[String],
        owner_principal: &str,
    ) -> Result<Vec<UploadedFile>, rusqlite::Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let n = ids.len();
        let placeholders = (1..=n)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let owner_ph = n + 1;
        let scope = format!(
            "(owner_principal = ?{owner_ph} OR owner_principal IS NULL OR owner_principal = '')"
        );
        let sql = format!(
            "SELECT id, name, original_name, mime, size, path, tags, uploaded_at
             FROM uploaded_files WHERE id IN ({placeholders}) AND ({scope}) ORDER BY uploaded_at DESC",
        );
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(&sql)?;
        let mut bind: Vec<String> = ids.to_vec();
        bind.push(owner_principal.to_string());
        let rows = stmt.query_map(params_from_iter(bind), |row| {
            Ok(UploadedFile {
                id: row.get(0)?,
                name: row.get(1)?,
                original_name: row.get(2)?,
                mime: row.get(3)?,
                size: row.get::<_, i64>(4)? as u64,
                path: row.get(5)?,
                tags: parse_tags(row.get::<_, String>(6)?),
                uploaded_at: row.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// Remove a file record and return the on-disk path so the caller can delete it.
    pub fn delete_file(
        &self,
        id: &str,
        owner_principal: &str,
    ) -> Result<Option<String>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let path: Option<String> = match conn.query_row(
            "SELECT path FROM uploaded_files
             WHERE id = ?1
               AND (owner_principal = ?2 OR owner_principal IS NULL OR owner_principal = '')",
            params![id, owner_principal],
            |row| row.get(0),
        ) {
            Ok(p) => Some(p),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e),
        };
        if path.is_some() {
            conn.execute(
                "DELETE FROM uploaded_files
                 WHERE id = ?1
                   AND (owner_principal = ?2 OR owner_principal IS NULL OR owner_principal = '')",
                params![id, owner_principal],
            )?;
        }
        Ok(path)
    }
}

fn parse_tags(raw: String) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Produce a URL/mention-safe display name from an original filename.
pub fn sanitize_display_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = s.trim_matches('_');
    let capped: String = trimmed.chars().take(255).collect();
    if capped.is_empty() {
        "upload".to_string()
    } else {
        capped
    }
}

/// Produce a filesystem-safe stored filename component.
///
/// Rules enforced:
/// - Only alphanumeric, `-`, `_`, `.` pass through; everything else becomes `_`.
/// - Leading and trailing dots are stripped (avoids hidden-file and Windows dot-strip bugs).
/// - Capped at 128 chars so the full `{uuid}_{safe}` path component stays well under 255 bytes.
/// - Empty result falls back to `"file"`.
pub fn sanitize_storage_name(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('.');
    let capped: String = trimmed.chars().take(128).collect();
    if capped.is_empty() {
        "file".to_string()
    } else {
        capped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn get_files_by_ids_scopes_to_owner_principal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStore::open(dir.path()).expect("open");
        let id = "11111111-1111-1111-1111-111111111111";
        let path = store.uploads_dir.join(format!("{id}_t.txt"));
        fs::write(&path, b"x").expect("write");
        let path_str = path.to_string_lossy().to_string();
        let owner_a = "owner_a_hash_value________________";
        let owner_b = "owner_b_hash_value________________";
        store
            .register_file(id, "t.txt", "text/plain", 1, &path_str, "", owner_a)
            .expect("register");

        let got_a = store
            .get_files_by_ids(&[id.to_string()], owner_a)
            .expect("query a");
        assert_eq!(got_a.len(), 1);

        let got_b = store
            .get_files_by_ids(&[id.to_string()], owner_b)
            .expect("query b");
        assert!(
            got_b.is_empty(),
            "other principal must not see non-legacy uploads"
        );
    }
}
