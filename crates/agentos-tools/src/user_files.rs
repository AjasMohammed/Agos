//! The agent-facing view of the user upload registry.
//!
//! Every tool that reaches the registry goes through here, and the access
//! policy lives here once. Before this module, `user-file-reader` opened
//! `uploads/file_registry.db` itself and hand-wrote its own `scope`/`tags`
//! clauses while `agentos_kernel::file_store::FileStore` hand-wrote a different
//! set for the same table. Two copies of an access rule is how the `inbound`
//! asymmetry below went unnoticed for as long as it did.
//!
//! ## Why this is not `FileStore`
//!
//! `FileStore` serves an *authenticated operator* typing in their own UI, and
//! scopes rows by `owner_principal` and chat session. This module serves an
//! *agent* acting on a model's guess. The two trust contexts genuinely differ —
//! most visibly in `find_by_name` — so they are separate, named, and the
//! difference is documented rather than hidden behind a boolean parameter.

use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

/// One row of the upload registry, as an agent may see it.
#[derive(Debug, Clone)]
pub struct UserFileRecord {
    pub id: String,
    /// Sanitized display name — what the `@mention` typeahead shows.
    pub name: String,
    /// The filename as uploaded. Operator-chosen for uploads, **sender-chosen**
    /// for inbound channel media, so treat it as untrusted text.
    pub original_name: String,
    pub mime: String,
    pub size: u64,
    /// Absolute path inside the kernel state dir. Never hand this to a model:
    /// `audit.db`, `api_keys.db` and `chat.db` are its siblings. `user-file-reader`
    /// materializes a copy under the agent's own root instead.
    pub path: String,
    pub tags: Vec<String>,
    pub uploaded_at: String,
}

impl UserFileRecord {
    /// True when the kernel's attachment sink wrote this row for media a channel
    /// sender pushed, rather than an operator uploading a file themselves.
    pub fn is_inbound(&self) -> bool {
        self.tags.iter().any(|t| t == "inbound")
    }

    /// What an agent can actually do with this file, so it stops discovering the
    /// answer by calling the wrong tool first.
    ///
    /// `text` — readable into the conversation, directly or via a converter.
    /// `image` — only via the vision path on a user turn, never a tool result.
    /// `binary` — take a handle and pass the path to another tool.
    pub fn readable_as(&self) -> &'static str {
        let ext = Path::new(&self.original_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if crate::extract::is_text_mime(&self.mime)
            || crate::extract::converter_for(&self.mime, ext).is_some()
        {
            "text"
        } else if self.mime.to_ascii_lowercase().starts_with("image/") {
            "image"
        } else {
            "binary"
        }
    }

    /// A single, safe path component to file a materialized copy under.
    ///
    /// `name` is *supposed* to be sanitized already (the kernel runs
    /// `sanitize_display_name` at registration), but this is the filename half
    /// of a path a tool is about to create, and rows written by older builds —
    /// or by anything that inserts directly — carry whatever the sender chose.
    /// Re-sanitizing here costs nothing and is the difference between a
    /// contained copy and a write outside the agent's root.
    pub fn handle_file_name(&self) -> String {
        let s: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // `.` and `..` are the two names that are valid characters yet not valid
        // components; a leading dot only hides the file.
        let s = s.trim_start_matches('.').to_string();
        if s.is_empty() {
            "file".to_string()
        } else {
            s.chars().take(120).collect()
        }
    }

    /// `"channel"` for media a sender pushed, `"upload"` for an operator upload.
    /// Exposed by `user-file-list` so an agent choosing between two files with
    /// the same name does it in the open, by id.
    pub fn source(&self) -> &'static str {
        if self.is_inbound() {
            "channel"
        } else {
            "upload"
        }
    }
}

/// Handle on the registry. Holds a path, not a connection: callers open a
/// short-lived connection inside `spawn_blocking`, because the web server writes
/// this WAL database concurrently.
#[derive(Debug, Clone)]
pub struct UserFiles {
    db_path: PathBuf,
}

/// Columns every query selects, in the order `row_to_record` reads them.
const COLUMNS: &str = "id, name, original_name, mime, size, path, tags, uploaded_at";

/// `scope <> 'derived'` guards every read: derived rows are PDF pages the web
/// server rendered from another upload, not files the user ever named.
const NOT_DERIVED: &str = "scope <> 'derived'";

/// Rows `artifact-write` created. They live in the same table, under the same
/// `global` scope, with an empty owner — so without this an agent that has
/// written a few reports gets a "what did the user upload?" answer made entirely
/// of its own output, and never sees the file the user actually sent. They are
/// still reachable by id; they are just not user uploads.
const NOT_ARTIFACT: &str = "','||COALESCE(tags,'')||',' NOT LIKE '%,artifact,%'";

impl UserFiles {
    /// Open the registry under `data_dir`. `Ok(None)` means no upload has ever
    /// been registered — a different thing from a lookup that found nothing, and
    /// callers phrase it differently.
    pub fn open(data_dir: &Path) -> Option<Self> {
        let db_path = data_dir.join("uploads").join("file_registry.db");
        db_path.exists().then_some(Self { db_path })
    }

    /// The uploads directory this registry belongs to.
    pub fn uploads_dir(&self) -> PathBuf {
        self.db_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    fn conn(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.db_path).map_err(|e| format!("open registry: {e}"))?;
        // The web server writes this WAL database concurrently; without a busy
        // timeout a chat upload in flight turns a lookup into SQLITE_BUSY, which
        // used to be reported to the agent as "file not found".
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("registry busy_timeout: {e}"))?;
        Ok(conn)
    }

    /// Look up by primary key. Inbound media is reachable here: a v4 UUID is an
    /// unguessable capability, and this is how the attachment note hands a
    /// channel file to the agent.
    pub fn get_by_id(&self, id: &str) -> Result<Option<UserFileRecord>, String> {
        let conn = self.conn()?;
        let sql = format!("SELECT {COLUMNS} FROM uploaded_files WHERE id = ?1 AND {NOT_DERIVED}");
        one(conn.prepare(&sql), params![id])
    }

    /// Look up by display or original name, newest first.
    ///
    /// Inbound channel media is excluded on purpose. The filename is chosen by
    /// whoever sent the message and newest wins, so without this anyone paired to
    /// a channel can make `invoice.pdf` resolve to their file instead of the
    /// operator's. Callers that need those rows use `list`, which returns them
    /// with distinct ids and a `source` field — disclosure rather than silent
    /// substitution.
    pub fn find_by_name(&self, name: &str) -> Result<Option<UserFileRecord>, String> {
        let conn = self.conn()?;
        let sql = format!(
            "SELECT {COLUMNS} FROM uploaded_files
             WHERE (name = ?1 OR original_name = ?1) AND {NOT_DERIVED}
               AND ','||COALESCE(tags,'')||',' NOT LIKE '%,inbound,%'
             ORDER BY uploaded_at DESC LIMIT 1"
        );
        one(conn.prepare(&sql), params![name])
    }

    /// Rows matching `name` that `find_by_name` refused to return. Used only to
    /// tell an agent *why* a name it can see in a transcript did not resolve,
    /// instead of reporting a file that exists as missing.
    pub fn inbound_matches_for_name(&self, name: &str) -> Result<usize, String> {
        let conn = self.conn()?;
        // Exactly the rows `find_by_name`'s `NOT LIKE` clause drops: whatever
        // that filter hides, this counts.
        let sql = format!(
            "SELECT COUNT(*) FROM uploaded_files
             WHERE (name = ?1 OR original_name = ?1) AND {NOT_DERIVED}
               AND ','||COALESCE(tags,'')||',' LIKE '%,inbound,%'"
        );
        let n: i64 = conn
            .query_row(&sql, params![name], |r| r.get(0))
            .map_err(|e| format!("registry lookup failed: {e}"))?;
        Ok(n as usize)
    }

    /// Discovery: newest first, optionally filtered by a name substring and/or a
    /// MIME prefix. Inbound rows **are** included — see `find_by_name` for why
    /// disclosing them is the safe direction.
    pub fn list(
        &self,
        query: Option<&str>,
        mime_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<UserFileRecord>, String> {
        let conn = self.conn()?;
        // LIKE patterns are bound as parameters, never interpolated. `\` is the
        // escape character so a literal `%` or `_` in a filename cannot widen
        // the match.
        let mut sql =
            format!("SELECT {COLUMNS} FROM uploaded_files WHERE {NOT_DERIVED} AND {NOT_ARTIFACT}");
        let mut binds: Vec<String> = Vec::new();
        if let Some(q) = query.filter(|q| !q.is_empty()) {
            binds.push(format!("%{}%", escape_like(q)));
            sql.push_str(&format!(
                " AND (name LIKE ?{n} ESCAPE '\\' OR original_name LIKE ?{n} ESCAPE '\\')",
                n = binds.len()
            ));
        }
        if let Some(p) = mime_prefix.filter(|p| !p.is_empty()) {
            binds.push(format!("{}%", escape_like(p)));
            sql.push_str(&format!(
                " AND LOWER(mime) LIKE ?{n} ESCAPE '\\'",
                n = binds.len()
            ));
        }
        // The limit is bound, not interpolated, and as an integer rather than
        // leaning on SQLite's string→int coercion.
        sql.push_str(" ORDER BY uploaded_at DESC, id DESC LIMIT ?");
        sql.push_str(&(binds.len() + 1).to_string());

        let mut params: Vec<Box<dyn rusqlite::ToSql>> = binds
            .into_iter()
            .map(|b| Box::new(b) as Box<dyn rusqlite::ToSql>)
            .collect();
        params.push(Box::new(limit as i64));

        let mut stmt = conn.prepare(&sql).map_err(prepare_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), row_to_record)
            .map_err(|e| format!("registry lookup failed: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("registry lookup failed: {e}"))
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn prepare_err(e: rusqlite::Error) -> String {
    format!("registry lookup failed: {e}")
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserFileRecord> {
    Ok(UserFileRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        original_name: row.get(2)?,
        mime: row.get(3)?,
        size: row.get::<_, i64>(4)?.max(0) as u64,
        path: row.get(5)?,
        tags: row
            .get::<_, Option<String>>(6)?
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        uploaded_at: row.get(7)?,
    })
}

/// Run a single-row query, mapping "no rows" to `None`.
///
/// A DB error is not a missing file. Collapsing the two once told an agent to
/// stop looking for a file that was there, so the two stay distinct all the way
/// out to the caller.
fn one(
    stmt: rusqlite::Result<rusqlite::Statement<'_>>,
    p: &[&dyn rusqlite::ToSql],
) -> Result<Option<UserFileRecord>, String> {
    let mut stmt = stmt.map_err(prepare_err)?;
    match stmt.query_row(p, row_to_record) {
        Ok(r) => Ok(Some(r)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(prepare_err(e)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Register an upload directly into a data dir's registry.
    pub(crate) fn register(
        data_dir: &Path,
        name: &str,
        mime: &str,
        bytes: &[u8],
        tags: &str,
    ) -> String {
        let uploads = data_dir.join("uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = uploads.join(format!("{id}_{name}"));
        std::fs::write(&path, bytes).unwrap();
        let conn = Connection::open(uploads.join("file_registry.db")).unwrap();
        crate::artifact_write::ensure_registry_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO uploaded_files
             (id, name, original_name, mime, size, path, tags, uploaded_at, scope)
             VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, 'global')",
            params![
                id,
                name,
                mime,
                bytes.len() as i64,
                path.to_string_lossy().to_string(),
                tags,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
        id
    }

    #[test]
    fn missing_registry_is_none_not_an_error() {
        let dir = TempDir::new().unwrap();
        assert!(UserFiles::open(dir.path()).is_none());
    }

    #[test]
    fn name_lookup_prefers_the_operator_upload_over_inbound_media() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "notes.txt", "text/plain", b"operator", "");
        // Registered later, so it would win `ORDER BY uploaded_at DESC`.
        register(dir.path(), "notes.txt", "text/plain", b"sender", "inbound");

        let files = UserFiles::open(dir.path()).unwrap();
        let hit = files.find_by_name("notes.txt").unwrap().unwrap();
        assert!(!hit.is_inbound());
        assert_eq!(hit.source(), "upload");
    }

    #[test]
    fn inbound_media_is_invisible_by_name_but_reachable_by_id() {
        let dir = TempDir::new().unwrap();
        let id = register(dir.path(), "sent.mp3", "audio/mpeg", b"x", "inbound");
        let files = UserFiles::open(dir.path()).unwrap();

        assert!(files.find_by_name("sent.mp3").unwrap().is_none());
        assert_eq!(files.get_by_id(&id).unwrap().unwrap().id, id);
        // …and the miss is explainable rather than reported as "no such file".
        assert_eq!(files.inbound_matches_for_name("sent.mp3").unwrap(), 1);
    }

    #[test]
    fn list_returns_inbound_rows_with_their_provenance() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "a.txt", "text/plain", b"a", "");
        register(dir.path(), "b.mp3", "audio/mpeg", b"b", "inbound");

        let files = UserFiles::open(dir.path()).unwrap();
        let all = files.list(None, None, 10).unwrap();
        assert_eq!(all.len(), 2);
        let mp3 = all.iter().find(|r| r.name == "b.mp3").unwrap();
        assert_eq!(mp3.source(), "channel");
        assert_eq!(mp3.readable_as(), "binary");
    }

    #[test]
    fn list_filters_by_query_and_mime_prefix() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "song.mp3", "audio/mpeg", b"a", "");
        register(dir.path(), "song.txt", "text/plain", b"b", "");
        register(dir.path(), "other.mp3", "audio/mpeg", b"c", "");

        let files = UserFiles::open(dir.path()).unwrap();
        assert_eq!(files.list(Some("song"), None, 10).unwrap().len(), 2);
        assert_eq!(files.list(None, Some("audio/"), 10).unwrap().len(), 2);
        assert_eq!(
            files.list(Some("song"), Some("audio/"), 10).unwrap().len(),
            1
        );
        assert_eq!(files.list(None, None, 2).unwrap().len(), 2);
    }

    /// A filename containing LIKE metacharacters must not widen its own search.
    #[test]
    fn like_wildcards_in_a_query_are_literal() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "a_b.txt", "text/plain", b"a", "");
        register(dir.path(), "axb.txt", "text/plain", b"b", "");

        let files = UserFiles::open(dir.path()).unwrap();
        let hits = files.list(Some("a_b"), None, 10).unwrap();
        assert_eq!(hits.len(), 1, "`_` must not match `x`");
        assert_eq!(hits[0].name, "a_b.txt");
    }

    #[test]
    fn readable_as_classifies_by_mime_and_extension() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "a.txt", "text/plain", b"a", "");
        register(dir.path(), "b.png", "image/png", b"b", "");
        register(dir.path(), "c.mp3", "audio/mpeg", b"c", "");
        // Browsers send .docx as octet-stream; the extension is the real signal.
        register(dir.path(), "d.docx", "application/octet-stream", b"d", "");

        let files = UserFiles::open(dir.path()).unwrap();
        let by = |n: &str| {
            files
                .list(Some(n), None, 1)
                .unwrap()
                .pop()
                .unwrap()
                .readable_as()
        };
        assert_eq!(by("a.txt"), "text");
        assert_eq!(by("b.png"), "image");
        assert_eq!(by("c.mp3"), "binary");
        assert_eq!(by("d.docx"), "text");
    }

    /// The filename half of a materialized path is a security boundary: a
    /// sender-chosen name must not escape the directory it is filed under.
    #[test]
    fn handle_file_name_is_always_one_safe_component() {
        let rec = |name: &str| UserFileRecord {
            id: "i".into(),
            name: name.into(),
            original_name: name.into(),
            mime: "application/octet-stream".into(),
            size: 0,
            path: String::new(),
            tags: vec![],
            uploaded_at: String::new(),
        };
        for bad in ["../../etc/passwd", "a/b.mp3", "..", ".", "", "  ", "/abs"] {
            let n = rec(bad).handle_file_name();
            assert!(!n.is_empty(), "{bad:?} → empty");
            assert_eq!(
                Path::new(&n).components().count(),
                1,
                "{bad:?} → {n:?} is not one component"
            );
            assert!(!n.starts_with('.'), "{bad:?} → {n:?} starts with a dot");
        }
        assert_eq!(rec("song.mp3").handle_file_name(), "song.mp3");
    }

    /// Derived rows are rendered PDF pages, not files the user named.
    #[test]
    fn derived_rows_are_never_returned() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "page.png", "image/png", b"p", "");
        let uploads = dir.path().join("uploads");
        Connection::open(uploads.join("file_registry.db"))
            .unwrap()
            .execute("UPDATE uploaded_files SET scope = 'derived'", [])
            .unwrap();

        let files = UserFiles::open(dir.path()).unwrap();
        assert!(files.list(None, None, 10).unwrap().is_empty());
        assert!(files.find_by_name("page.png").unwrap().is_none());
    }
}
