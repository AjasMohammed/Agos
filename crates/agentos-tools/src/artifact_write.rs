use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::Duration;

/// 2 MiB cap. An artifact is a document a human will read, not a data dump;
/// anything larger is a file, and `file-writer` already covers files.
const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 120;
/// Ceiling on live artifacts per agent.
///
/// The manifest's `write_agent_state` class auto-approves under the default
/// approval mode, so the operator prompt no longer bounds a publish loop, and
/// nothing sweeps artifact rows (`FileStore::prune_derived` only touches
/// `scope = 'derived'`; these are `'global'`). At the 2 MiB body cap this
/// bounds an agent at ~500 MiB. Revising in place (passing `artifact_id`) is
/// unaffected — only new ids count against it.
///
/// ponytail: a flat per-agent count, not a byte budget or a TTL sweep. Swap in
/// a reaper if artifacts ever need to expire on their own.
const MAX_ARTIFACTS_PER_AGENT: usize = 250;

const TOOL_NAME: &str = "artifact-write";

/// Publish a document the user can open at `/artifacts/<id>`.
///
/// An artifact is a tagged row in the same upload registry the web UI uses for
/// user files, plus a blob under `data_dir/uploads/artifacts/`. The tool holds
/// no `FileStore` handle — `ToolExecutionContext` carries none — so it opens a
/// short-lived connection on the registry inside `spawn_blocking`, the same way
/// [`crate::user_file_reader::UserFileReader`] reads it.
///
/// Requires `fs.artifacts:w` permission.
pub struct ArtifactWriteTool;

impl ArtifactWriteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ArtifactWriteTool {
    fn default() -> Self {
        Self::new()
    }
}

fn failed(reason: impl Into<String>) -> AgentOSError {
    AgentOSError::ToolExecutionFailed {
        tool_name: TOOL_NAME.into(),
        reason: reason.into(),
    }
}

/// Mirror of `agentos_kernel::file_store::sanitize_display_name`. This crate
/// cannot depend on the kernel (the kernel depends on it), and the `name`
/// column has to carry the same shape the web uploader writes.
pub(crate) fn sanitize_display_name(name: &str) -> String {
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
    let capped: String = s.trim_matches('_').chars().take(255).collect();
    if capped.is_empty() {
        "artifact".to_string()
    } else {
        capped
    }
}

/// Mirror of `FileStore::open`'s schema bootstrap. An agent can publish the
/// first artifact before any user has ever uploaded a file, in which case the
/// registry does not exist yet and `INSERT` into a missing table is not a
/// useful error for the model.
pub(crate) fn ensure_registry_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
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
    // Columns added by later migrations in `FileStore::open` — a registry
    // created by an older build will be missing them.
    for (column, ddl) in [
        (
            "owner_principal",
            "ALTER TABLE uploaded_files ADD COLUMN owner_principal TEXT;",
        ),
        (
            "scope",
            "ALTER TABLE uploaded_files ADD COLUMN scope TEXT DEFAULT 'global';",
        ),
    ] {
        let present: bool = conn
            .prepare("PRAGMA table_info(uploaded_files)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .any(|c| c.as_deref() == Ok(column));
        if !present {
            conn.execute_batch(ddl)?;
        }
    }
    Ok(())
}

#[async_trait]
impl AgentTool for ArtifactWriteTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.artifacts".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let title_raw = payload
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("artifact-write requires 'title' field".into())
            })?;

        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("artifact-write requires 'content' field".into())
            })?;

        let kind = payload
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown");

        // The extension is derived from `kind`, never from the payload, so the
        // enum check is also a filesystem check.
        let (ext, mime) = match kind {
            "html" => (".html", "text/html"),
            "markdown" | "slides" => (".md", "text/markdown"),
            other => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "artifact-write 'kind' must be one of html, markdown, slides — got '{}'",
                    truncate_for_error(other)
                )));
            }
        };

        if content.len() > MAX_ARTIFACT_BYTES {
            return Err(AgentOSError::SchemaValidation(format!(
                "artifact-write 'content' is {} bytes, over the {MAX_ARTIFACT_BYTES} byte limit — trim it, or write the full data to a file with file-writer",
                content.len()
            )));
        }

        let title: String = title_raw.trim().chars().take(MAX_TITLE_CHARS).collect();
        if title.is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "artifact-write 'title' is empty — give the artifact a short human heading".into(),
            ));
        }

        // SECURITY: the id is the only value that reaches the filesystem path.
        // A caller-supplied id must parse as a UUID and is re-serialized to its
        // canonical form, so braced/urn spellings cannot vary the filename and
        // nothing resembling a path component can survive.
        let id = match payload.get("artifact_id").and_then(|v| v.as_str()) {
            Some(raw) => uuid::Uuid::parse_str(raw.trim())
                .map_err(|_| {
                    AgentOSError::SchemaValidation(format!(
                        "artifact-write 'artifact_id' must be a UUID returned by an earlier artifact-write call — got '{}'",
                        truncate_for_error(raw)
                    ))
                })?
                .to_string(),
            None => uuid::Uuid::new_v4().to_string(),
        };

        let uploads_dir = context.data_dir.join("uploads");
        let dir = uploads_dir.join("artifacts");
        let db_path = uploads_dir.join("file_registry.db");
        let path = dir.join(format!("{id}{ext}"));
        let path_str = path.to_string_lossy().to_string();

        let agent_tag = format!("agent:{}", context.agent_id);
        let tags = format!("artifact,kind:{kind},{agent_tag}");
        let display_name = sanitize_display_name(&title);
        let bytes = content.len();

        let (id_task, title_task, content_task) = (id.clone(), title.clone(), content.to_string());

        tokio::task::spawn_blocking(move || -> Result<(), AgentOSError> {
            std::fs::create_dir_all(&dir)
                .map_err(|e| failed(format!("Cannot create artifacts directory: {e}")))?;

            let conn = Connection::open(&db_path)
                .map_err(|e| failed(format!("Cannot open file registry: {e}")))?;
            // The web server holds a long-lived writer on the same registry.
            conn.busy_timeout(Duration::from_secs(5))
                .map_err(|e| failed(format!("Cannot configure file registry: {e}")))?;
            ensure_registry_schema(&conn)
                .map_err(|e| failed(format!("Cannot prepare file registry: {e}")))?;

            let existing: Option<(String, String)> = conn
                .query_row(
                    "SELECT tags, path FROM uploaded_files WHERE id = ?1",
                    params![&id_task],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|e| failed(format!("Cannot read file registry: {e}")))?;

            // A row that already exists must belong to the calling agent — one
            // agent must not silently replace another's artifact. No row means
            // the caller is re-creating an id it already holds (idempotent
            // re-write after a crash), which is allowed.
            let previous_path = match existing {
                Some((existing_tags, existing_path)) => {
                    // Both tags are required. `agent:<id>` alone would let this
                    // tool rewrite ANY row carrying that tag — INSERT OR REPLACE
                    // rewrites path/mime/name wholesale, so a non-artifact row
                    // tagged by the same agent could be repointed at agent-authored
                    // content. Nothing else writes an `agent:` tag today; this
                    // keeps it true if something starts.
                    let mut owned = false;
                    let mut is_artifact = false;
                    for tag in existing_tags.split(',').map(str::trim) {
                        if tag == agent_tag {
                            owned = true;
                        } else if tag == "artifact" {
                            is_artifact = true;
                        }
                    }
                    if !owned || !is_artifact {
                        return Err(failed(format!(
                            "Artifact {id_task} belongs to another agent and cannot be replaced"
                        )));
                    }
                    Some(existing_path)
                }
                None => None,
            };

            // A new id (not a revision) counts against the per-agent ceiling.
            // Checked inside the same connection as the INSERT below; two
            // concurrent writes can both observe `count == MAX - 1` and land
            // one over, which is fine for a disk-usage bound.
            if previous_path.is_none() {
                let live: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM uploaded_files \
                         WHERE tags LIKE '%artifact%' AND tags LIKE ?1",
                        params![format!("%{agent_tag}%")],
                        |row| row.get(0),
                    )
                    .map_err(|e| failed(format!("Cannot count artifacts: {e}")))?;
                if live as usize >= MAX_ARTIFACTS_PER_AGENT {
                    return Err(failed(format!(
                        "You already hold {live} artifacts, the per-agent limit is {MAX_ARTIFACTS_PER_AGENT}. \
                         Pass an existing 'artifact_id' to revise one in place, or ask the user to delete some."
                    )));
                }
            }

            // SECURITY: the filename is server-shaped, but `artifact_id` lets the
            // caller CHOOSE which name it lands on — so a symlink planted at that
            // name would turn `fs::write` into an arbitrary write as the kernel
            // user. Path traversal is structurally impossible here; writing
            // *through* a link is not, so refuse it.
            if std::fs::symlink_metadata(&path)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err(failed(format!(
                    "Artifact path for {id_task} is a symlink; refusing to write through it"
                )));
            }
            std::fs::write(&path, content_task.as_bytes())
                .map_err(|e| failed(format!("Cannot write artifact: {e}")))?;

            // ponytail: empty owner = visible to any authed operator, matching
            // channel attachments (`FileStore::get_file` treats '' as unowned).
            // Per-user artifact ownership when multi-tenant lands.
            conn.execute(
                "INSERT OR REPLACE INTO uploaded_files
                     (id, name, original_name, mime, size, path, tags, uploaded_at, owner_principal, scope)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), '', 'global')",
                params![
                    &id_task,
                    &display_name,
                    &title_task,
                    mime,
                    bytes as i64,
                    &path_str,
                    &tags
                ],
            )
            .map_err(|e| failed(format!("Cannot register artifact: {e}")))?;

            // A kind change moves the extension, so drop the row's old blob
            // rather than leave an unreachable file in the artifacts dir.
            //
            // SECURITY: compare the PARENT, not `starts_with`. `Path::starts_with`
            // is component-wise and keeps `..` literally, so
            // `<artifacts>/../../../etc/cron.d/x` passes a `starts_with(&dir)`
            // check and would be unlinked as the kernel user. This tool only ever
            // writes `dir/<uuid><ext>`, so the parent is knowable exactly — and
            // `parent()` of a traversal path is `dir/..`, which is not `dir`.
            if let Some(old) = previous_path {
                let old_path = std::path::Path::new(&old);
                if old != path_str && old_path.parent() == Some(dir.as_path()) {
                    let _ = std::fs::remove_file(old_path);
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| failed(format!("spawn_blocking panicked: {e}")))??;

        Ok(serde_json::json!({
            "artifact_id": id,
            "url": format!("/artifacts/{id}"),
            "title": title,
            "kind": kind,
            "bytes": bytes,
            "note": "Share the url with the user as a markdown link.",
        }))
    }
}

/// Keep a rejected payload value out of an unbounded error string.
fn truncate_for_error(value: &str) -> String {
    value.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn ctx(data_dir: &Path, agent_id: AgentID) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            agent_id,
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
            shared_dir: None,
        }
    }

    fn registry(data_dir: &Path) -> Connection {
        Connection::open(data_dir.join("uploads").join("file_registry.db")).expect("open registry")
    }

    /// `(tags, path, size)` for one row, or `None` when the id is absent.
    fn row(data_dir: &Path, id: &str) -> Option<(String, String, i64)> {
        registry(data_dir)
            .query_row(
                "SELECT tags, path, size FROM uploaded_files WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .expect("query row")
    }

    /// Insert a registry row directly, bypassing the tool — used to stage rows the
    /// tool itself would never write (e.g. a non-artifact row carrying an
    /// `agent:` tag).
    fn seed_row(tmp: &TempDir, id: &str, tags: &str, path: &str) {
        let uploads = tmp.path().join("uploads");
        std::fs::create_dir_all(&uploads).expect("uploads dir");
        let conn = Connection::open(uploads.join("file_registry.db")).expect("open registry");
        ensure_registry_schema(&conn).expect("schema");
        conn.execute(
            "INSERT OR REPLACE INTO uploaded_files
                 (id, name, original_name, mime, size, path, tags, uploaded_at, owner_principal, scope)
             VALUES (?1, ?2, ?2, 'application/octet-stream', 8, ?3, ?4,
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), '', 'global')",
            params![id, "victim.bin", path, tags],
        )
        .expect("seed row");
    }

    async fn write_artifact(
        tmp: &TempDir,
        agent: AgentID,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, AgentOSError> {
        ArtifactWriteTool::new()
            .execute(payload, ctx(tmp.path(), agent))
            .await
    }

    #[tokio::test]
    async fn create_writes_file_and_row() {
        let tmp = TempDir::new().expect("tempdir");
        let agent = AgentID::new();
        let out = write_artifact(
            &tmp,
            agent,
            serde_json::json!({"title": "Q3 Review", "content": "# Hello", "kind": "markdown"}),
        )
        .await
        .expect("create artifact");

        let id = out["artifact_id"]
            .as_str()
            .expect("artifact_id")
            .to_string();
        assert_eq!(out["url"], format!("/artifacts/{id}"));
        assert_eq!(out["kind"], "markdown");
        assert_eq!(out["bytes"], 7);
        assert!(out["note"].as_str().expect("note").contains("url"));

        let disk = tmp
            .path()
            .join("uploads")
            .join("artifacts")
            .join(format!("{id}.md"));
        assert_eq!(
            std::fs::read_to_string(&disk).expect("read blob"),
            "# Hello"
        );

        let (tags, path, size) = row(tmp.path(), &id).expect("registry row");
        assert!(tags.split(',').any(|t| t == "artifact"), "tags: {tags}");
        assert!(
            tags.split(',').any(|t| t == "kind:markdown"),
            "tags: {tags}"
        );
        assert!(
            tags.split(',').any(|t| t == format!("agent:{agent}")),
            "tags: {tags}"
        );
        assert_eq!(path, disk.to_string_lossy());
        assert_eq!(size, 7);
    }

    #[tokio::test]
    async fn rejects_unknown_kind() {
        let tmp = TempDir::new().expect("tempdir");
        let err = write_artifact(
            &tmp,
            AgentID::new(),
            serde_json::json!({"title": "t", "content": "c", "kind": "pdf"}),
        )
        .await
        .expect_err("pdf is not a kind");
        assert!(
            matches!(err, AgentOSError::SchemaValidation(_)),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_oversize_content() {
        let tmp = TempDir::new().expect("tempdir");
        let big = "a".repeat(3 * 1024 * 1024);
        let err = write_artifact(
            &tmp,
            AgentID::new(),
            serde_json::json!({"title": "t", "content": big}),
        )
        .await
        .expect_err("3 MiB is over the cap");
        let msg = err.to_string();
        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
        assert!(
            msg.contains(&(3 * 1024 * 1024).to_string()),
            "error must name the actual size: {msg}"
        );
        // Nothing reached disk.
        assert!(!tmp.path().join("uploads").join("artifacts").exists());
    }

    #[tokio::test]
    async fn rejects_empty_title() {
        let tmp = TempDir::new().expect("tempdir");
        let err = write_artifact(
            &tmp,
            AgentID::new(),
            serde_json::json!({"title": "   ", "content": "c"}),
        )
        .await
        .expect_err("whitespace title");
        assert!(
            matches!(err, AgentOSError::SchemaValidation(_)),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn update_replaces_in_place() {
        let tmp = TempDir::new().expect("tempdir");
        let agent = AgentID::new();
        let first = write_artifact(
            &tmp,
            agent,
            serde_json::json!({"title": "Draft", "content": "v1"}),
        )
        .await
        .expect("create");
        let id = first["artifact_id"].as_str().expect("id").to_string();

        let second = write_artifact(
            &tmp,
            agent,
            serde_json::json!({"title": "Draft", "content": "v2", "artifact_id": id}),
        )
        .await
        .expect("update");
        assert_eq!(second["artifact_id"], id);

        let disk = tmp
            .path()
            .join("uploads")
            .join("artifacts")
            .join(format!("{id}.md"));
        assert_eq!(std::fs::read_to_string(&disk).expect("read blob"), "v2");

        let count: i64 = registry(tmp.path())
            .query_row("SELECT COUNT(*) FROM uploaded_files", [], |r| r.get(0))
            .expect("count rows");
        assert_eq!(count, 1, "update must replace, not append");
    }

    #[tokio::test]
    async fn update_by_other_agent_denied() {
        let tmp = TempDir::new().expect("tempdir");
        let owner = AgentID::new();
        let created = write_artifact(
            &tmp,
            owner,
            serde_json::json!({"title": "Owned", "content": "mine"}),
        )
        .await
        .expect("create");
        let id = created["artifact_id"].as_str().expect("id").to_string();

        let err = write_artifact(
            &tmp,
            AgentID::new(),
            serde_json::json!({"title": "Stolen", "content": "theirs", "artifact_id": id}),
        )
        .await
        .expect_err("other agent must not replace");
        assert!(
            matches!(err, AgentOSError::ToolExecutionFailed { .. }),
            "unexpected error: {err}"
        );

        let disk = tmp
            .path()
            .join("uploads")
            .join("artifacts")
            .join(format!("{id}.md"));
        assert_eq!(
            std::fs::read_to_string(&disk).expect("read blob"),
            "mine",
            "denied update must not touch the blob"
        );
    }

    /// The update gate requires BOTH `agent:<id>` and `artifact`. Matching on the
    /// agent tag alone would let this tool repoint any row that agent happens to
    /// own at agent-authored content, since the write is INSERT OR REPLACE.
    #[tokio::test]
    async fn update_requires_the_artifact_tag_too() {
        let tmp = TempDir::new().expect("tempdir");
        let agent = AgentID::new();
        let id = uuid::Uuid::new_v4().to_string();

        // A non-artifact row tagged by the same agent — e.g. a future attachment
        // sink that starts stamping `agent:` on inbound media.
        let uploads = tmp.path().join("uploads");
        std::fs::create_dir_all(&uploads).expect("uploads");
        let victim = uploads.join("victim.bin");
        std::fs::write(&victim, "original").expect("seed victim");
        seed_row(
            &tmp,
            &id,
            &format!("upload,agent:{agent}"),
            victim.to_string_lossy().as_ref(),
        );

        let err = write_artifact(
            &tmp,
            agent,
            serde_json::json!({"title": "Hijack", "content": "pwn", "artifact_id": id}),
        )
        .await
        .expect_err("non-artifact row must not be replaceable");
        assert!(
            matches!(err, AgentOSError::ToolExecutionFailed { .. }),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read victim"),
            "original",
            "victim row's blob must be untouched"
        );
    }

    /// The cleanup unlink must compare the PARENT, not `starts_with`.
    /// `Path::starts_with` is component-wise and keeps `..` literally, so
    /// `<artifacts>/../../../tmp/victim` passes a `starts_with(&dir)` check and
    /// would be deleted as the kernel user. Nothing in the ownership gate above
    /// catches this, so without this test a refactor back to `starts_with` passes
    /// the whole suite.
    #[tokio::test]
    async fn cleanup_unlink_rejects_traversal_path() {
        let tmp = TempDir::new().expect("tempdir");
        let agent = AgentID::new();
        let id = uuid::Uuid::new_v4().to_string();

        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "untouched").expect("seed victim");

        // An artifact-tagged, agent-owned row whose stored path escapes via `..`.
        // The kind change (md -> html) is what triggers the cleanup branch.
        let dir = tmp.path().join("uploads").join("artifacts");
        std::fs::create_dir_all(&dir).expect("artifacts dir");
        let traversal = format!("{}/../../victim.txt", dir.to_string_lossy());
        seed_row(
            &tmp,
            &id,
            &format!("artifact,kind:markdown,agent:{agent}"),
            &traversal,
        );

        write_artifact(
            &tmp,
            agent,
            serde_json::json!({
                "title": "Rewrite", "content": "<b>x</b>",
                "kind": "html", "artifact_id": id
            }),
        )
        .await
        .expect("owned artifact rewrites");

        assert_eq!(
            std::fs::read_to_string(&victim).expect("victim must survive"),
            "untouched",
            "traversal path was unlinked by the cleanup branch"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_to_write_through_a_symlink() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path().join("uploads").join("artifacts");
        std::fs::create_dir_all(&dir).expect("artifacts dir");

        // The caller picks the id, so it picks which filename it lands on — a
        // planted link would otherwise turn this into an arbitrary write.
        let id = uuid::Uuid::new_v4().to_string();
        let target = tmp.path().join("outside.txt");
        std::fs::write(&target, "untouched").expect("seed target");
        std::os::unix::fs::symlink(&target, dir.join(format!("{id}.md"))).expect("symlink");

        let err = write_artifact(
            &tmp,
            AgentID::new(),
            serde_json::json!({"title": "Sneak", "content": "pwn", "artifact_id": id}),
        )
        .await
        .expect_err("must refuse to write through a symlink");
        assert!(
            matches!(err, AgentOSError::ToolExecutionFailed { .. }),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read target"),
            "untouched",
            "symlink target was written through"
        );
    }

    #[tokio::test]
    async fn id_is_server_generated() {
        let tmp = TempDir::new().expect("tempdir");
        let err = write_artifact(
            &tmp,
            AgentID::new(),
            serde_json::json!({
                "title": "Traversal",
                "content": "x",
                "artifact_id": "../../etc/passwd",
            }),
        )
        .await
        .expect_err("non-UUID id must be rejected outright");
        assert!(
            matches!(err, AgentOSError::SchemaValidation(_)),
            "unexpected error: {err}"
        );
        // No blob anywhere: the rejection happens before any path is built.
        assert!(!tmp.path().join("uploads").exists());
    }
}
