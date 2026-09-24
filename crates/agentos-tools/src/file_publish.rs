use crate::artifact_write::{ensure_registry_schema, sanitize_display_name};
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use rusqlite::{params, Connection};
use std::time::Duration;

/// 50 MiB cap. The bytes are copied, so this bounds disk per call; the panel
/// previews far less than this and offers the rest as a download.
const MAX_PUBLISH_BYTES: u64 = 50 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 120;
/// Ceiling on live published files per agent. `write_agent_state` auto-approves
/// under the default approval mode, so nothing else bounds a publish loop.
///
/// ponytail: a flat per-agent count, same as `artifact-write`. Swap in a byte
/// budget or a reaper if published media ever needs to expire on its own.
const MAX_PUBLISHED_PER_AGENT: usize = 250;

const TOOL_NAME: &str = "file-publish";

/// Hand a file from the agent's workspace to the operator.
///
/// The bytes are **copied** into `data_dir/uploads/published/` and registered in
/// the same upload registry the panel reads, tagged `published,agent:<id>`.
///
/// SECURITY: the row deliberately does NOT carry the `artifact` tag. That tag is
/// what admits a row to the HTML render paths (`/artifacts/{id}/raw`, the panel's
/// `srcDoc` iframe); a published file is an ordinary file to every viewer —
/// download plus the read-only preview — whatever its bytes turn out to be.
///
/// Requires `fs.artifacts:w` and `fs.user_data:r`.
pub struct FilePublishTool;

impl FilePublishTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FilePublishTool {
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

/// `(mime, extension)` from leading magic bytes.
///
/// SECURITY: the type is never taken from the file name or from the agent. An
/// unrecognised file is `text/plain` when it is UTF-8 and `octet-stream`
/// otherwise, so HTML and SVG always register as inert text.
fn sniff(head: &[u8]) -> (&'static str, &'static str) {
    match head {
        [0x89, b'P', b'N', b'G', ..] => ("image/png", "png"),
        [0xFF, 0xD8, 0xFF, ..] => ("image/jpeg", "jpg"),
        [b'G', b'I', b'F', b'8', ..] => ("image/gif", "gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => ("image/webp", "webp"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'A', b'V', b'E', ..] => ("audio/wav", "wav"),
        [b'%', b'P', b'D', b'F', ..] => ("application/pdf", "pdf"),
        [b'O', b'g', b'g', b'S', ..] => ("audio/ogg", "ogg"),
        [b'f', b'L', b'a', b'C', ..] => ("audio/flac", "flac"),
        [b'I', b'D', b'3', ..] | [0xFF, 0xFB, ..] | [0xFF, 0xF3, ..] => ("audio/mpeg", "mp3"),
        [0x1A, 0x45, 0xDF, 0xA3, ..] => ("video/webm", "webm"),
        // ISO-BMFF: bytes 4..8 == "ftyp"; the brand separates audio from video.
        [_, _, _, _, b'f', b't', b'y', b'p', b'M', b'4', b'A', ..] => ("audio/mp4", "m4a"),
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => ("video/mp4", "mp4"),
        _ if !head.contains(&0) && std::str::from_utf8(head).is_ok() => ("text/plain", "txt"),
        _ => ("application/octet-stream", "bin"),
    }
}

/// True when the name promises media the bytes did not deliver — a download
/// that saved an HTML error page as `photo.jpg`.
fn extension_disagrees(name: &str, mime: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    let claims = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => "image/",
        "mp3" | "wav" | "ogg" | "flac" | "m4a" => "audio/",
        "mp4" | "webm" | "mov" | "mkv" => "video/",
        "pdf" => "application/pdf",
        _ => return false,
    };
    !mime.starts_with(claims)
}

#[async_trait]
impl AgentTool for FilePublishTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // `fs.user_data:r` because this reads the same files `file-reader`
        // does: revoking an agent's file read must revoke this path too.
        vec![
            ("fs.artifacts".to_string(), PermissionOp::Write),
            ("fs.user_data".to_string(), PermissionOp::Read),
        ]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let path_str = payload
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("file-publish requires 'path' field".into())
            })?;

        // SECURITY: the same gate as `file-reader` — relative paths resolve under
        // the agent's own home, and the canonical path must stay inside the home,
        // a granted workspace or a storage zone. An agent can only publish what
        // it could already read.
        let agent_root = context.agent_files_dir()?;
        let resolved =
            crate::traits::resolve_tool_path(path_str, &agent_root, &context.read_roots())
                .map_err(|e| context.with_path_hint(e))?;
        let canonical = resolved
            .canonicalize()
            .map_err(|e| failed(format!("Path not found: {path_str} ({e})")))?;
        let canonical_agent_root = agent_root
            .canonicalize()
            .map_err(|e| failed(format!("Data directory error: {e}")))?;

        let in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| canonical.starts_with(wp));
        let in_storage_zone = context
            .storage_zone_query
            .as_ref()
            .map(|q| q.is_path_in_zone(&context.agent_id, &canonical))
            .unwrap_or(false);
        if !canonical.starts_with(&canonical_agent_root) && !in_workspace && !in_storage_zone {
            tracing::warn!(path = path_str, "file-publish: path traversal blocked");
            return Err(context.deny_path(path_str));
        }
        if in_workspace
            && !context
                .permissions
                .check("fs.workspace", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.workspace".into(),
                operation: format!("Workspace read access denied: {path_str}"),
            });
        }

        let file_name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let title: String = payload
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(&file_name)
            .chars()
            .take(MAX_TITLE_CHARS)
            .collect();

        let id = uuid::Uuid::new_v4().to_string();
        let uploads_dir = context.data_dir.join("uploads");
        let dir = uploads_dir.join("published");
        let db_path = uploads_dir.join("file_registry.db");
        let agent_tag = format!("agent:{}", context.agent_id);

        let (id_task, title_task) = (id.clone(), title.clone());
        let (mime, size) = tokio::task::spawn_blocking(
            move || -> Result<(&'static str, u64), AgentOSError> {
                use std::io::{Read, Seek};
                // SECURITY: one open, and everything — type check, size, sniff,
                // copy — comes from that fd. `canonical` was validated on another
                // thread; re-opening by path (or `fs::copy`, which follows links)
                // would let the agent swap the last component for a symlink after
                // the containment check and publish any file the kernel can read.
                let mut opts = std::fs::OpenOptions::new();
                opts.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
                }
                let mut src = opts
                    .open(&canonical)
                    .map_err(|e| failed(format!("Cannot open file: {e}")))?;
                let meta = src
                    .metadata()
                    .map_err(|e| failed(format!("Cannot stat file: {e}")))?;
                if !meta.is_file() {
                    return Err(failed("Only a regular file can be published, not a directory"));
                }
                if meta.len() == 0 {
                    return Err(failed(
                        "File is empty (0 bytes) — the download or write that produced it failed",
                    ));
                }
                if meta.len() > MAX_PUBLISH_BYTES {
                    return Err(failed(format!(
                        "File is {} bytes, over the {MAX_PUBLISH_BYTES} byte publish limit",
                        meta.len()
                    )));
                }

                let mut head = Vec::with_capacity(512);
                (&mut src)
                    .take(512)
                    .read_to_end(&mut head)
                    .and_then(|_| src.rewind())
                    .map_err(|e| failed(format!("Cannot read file: {e}")))?;
                let (mime, ext) = sniff(&head);

                std::fs::create_dir_all(&dir)
                    .map_err(|e| failed(format!("Cannot create published directory: {e}")))?;
                let conn = Connection::open(&db_path)
                    .map_err(|e| failed(format!("Cannot open file registry: {e}")))?;
                // The web server holds a long-lived writer on the same registry.
                conn.busy_timeout(Duration::from_secs(5))
                    .map_err(|e| failed(format!("Cannot configure file registry: {e}")))?;
                ensure_registry_schema(&conn)
                    .map_err(|e| failed(format!("Cannot prepare file registry: {e}")))?;

                let live: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM uploaded_files \
                         WHERE ','||COALESCE(tags,'')||',' LIKE '%,published,%' \
                         AND ','||COALESCE(tags,'')||',' LIKE ?1",
                        params![format!("%,{agent_tag},%")],
                        |row| row.get(0),
                    )
                    .map_err(|e| failed(format!("Cannot count published files: {e}")))?;
                if live as usize >= MAX_PUBLISHED_PER_AGENT {
                    return Err(failed(format!(
                        "You already hold {live} published files, the per-agent limit is \
                         {MAX_PUBLISHED_PER_AGENT}. Ask the user to delete some."
                    )));
                }

                // The destination name is a fresh server-side UUID, so nothing
                // can be planted at it ahead of time.
                let dest = dir.join(format!("{id_task}.{ext}"));
                // The cap is enforced on the bytes written, not on the earlier
                // stat: the file can still grow while it is being copied.
                let copied = std::fs::File::create_new(&dest).and_then(|mut out| {
                    std::io::copy(&mut (&mut src).take(MAX_PUBLISH_BYTES + 1), &mut out)
                });
                let size = match copied {
                    Ok(n) if n <= MAX_PUBLISH_BYTES => n,
                    other => {
                        let _ = std::fs::remove_file(&dest);
                        return Err(failed(match other {
                            Ok(_) => format!("File grew past the {MAX_PUBLISH_BYTES} byte publish limit"),
                            Err(e) => format!("Cannot copy file: {e}"),
                        }));
                    }
                };

                // ponytail: a crash between the copy above and this INSERT leaves
                // an unreferenced blob nothing sweeps. Add a reaper with the TTL one.
                // ponytail: empty owner = visible to any authed operator, same
                // as artifacts. Per-user ownership when multi-tenant lands.
                let registered = conn.execute(
                    "INSERT INTO uploaded_files
                         (id, name, original_name, mime, size, path, tags, uploaded_at, owner_principal, scope)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), '', 'global')",
                    params![
                        &id_task,
                        sanitize_display_name(&title_task),
                        &title_task,
                        mime,
                        size as i64,
                        dest.to_string_lossy(),
                        format!("published,{agent_tag}")
                    ],
                );
                if let Err(e) = registered {
                    let _ = std::fs::remove_file(&dest);
                    return Err(failed(format!("Cannot register file: {e}")));
                }
                Ok((mime, size))
            },
        )
        .await
        .map_err(|e| failed(format!("spawn_blocking panicked: {e}")))??;

        let url = format!("/artifacts/{id}");
        let markdown = if mime.starts_with("image/") {
            format!("![{title}]({url})")
        } else {
            format!("[{title}]({url})")
        };
        let mut out = serde_json::json!({
            "file_id": id,
            "url": url,
            "title": title,
            "mime": mime,
            "bytes": size,
            "markdown": markdown,
            "note": "Put the 'markdown' value in your reply; the chat shows it as an inline preview.",
        });
        if extension_disagrees(&file_name, mime) {
            out["warning"] = serde_json::json!(format!(
                "'{file_name}' is named like media but its bytes are {mime} — the download \
                 likely saved an error page. Fetch it again before telling the user it worked."
            ));
        }
        Ok(out)
    }
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

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];

    /// Write `bytes` at `rel` inside the agent's home and publish it.
    async fn publish(
        tmp: &TempDir,
        agent: AgentID,
        rel: &str,
        bytes: &[u8],
    ) -> Result<serde_json::Value, AgentOSError> {
        let c = ctx(tmp.path(), agent);
        std::fs::write(c.agent_files_dir().expect("home").join(rel), bytes).expect("seed file");
        FilePublishTool::new()
            .execute(serde_json::json!({ "path": rel }), c)
            .await
    }

    #[tokio::test]
    async fn publishes_a_copy_tagged_published_never_artifact() {
        let tmp = TempDir::new().expect("tempdir");
        let agent = AgentID::new();
        let out = publish(&tmp, agent, "cat.png", PNG).await.expect("publish");

        let id = out["file_id"].as_str().expect("file_id");
        assert_eq!(out["mime"], "image/png");
        assert_eq!(out["markdown"], format!("![cat.png](/artifacts/{id})"));
        assert!(out.get("warning").is_none(), "got {out}");

        let (tags, path, mime): (String, String, String) =
            Connection::open(tmp.path().join("uploads").join("file_registry.db"))
                .expect("open registry")
                .query_row(
                    "SELECT tags, path, mime FROM uploaded_files WHERE id = ?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .expect("row");
        assert_eq!(tags, format!("published,agent:{agent}"));
        assert_eq!(mime, "image/png");
        // A copy under uploads/, not a pointer into the agent's workspace.
        assert!(Path::new(&path).starts_with(tmp.path().join("uploads").join("published")));
        assert_eq!(std::fs::read(&path).expect("read copy"), PNG);
    }

    /// The live bug: a download saved an HTML error page as `.jpg`. The type
    /// must come from the bytes, and the agent must be told.
    #[tokio::test]
    async fn html_named_jpg_is_inert_text_with_a_warning() {
        let tmp = TempDir::new().expect("tempdir");
        let out = publish(
            &tmp,
            AgentID::new(),
            "photo.jpg",
            b"<!DOCTYPE html><script>alert(1)</script>",
        )
        .await
        .expect("publish");
        assert_eq!(out["mime"], "text/plain");
        assert!(out["warning"]
            .as_str()
            .expect("warning")
            .contains("error page"));
        assert!(out["markdown"].as_str().expect("md").starts_with('['));
    }

    #[tokio::test]
    async fn rejects_traversal_and_paths_outside_the_home() {
        let tmp = TempDir::new().expect("tempdir");
        // Kernel state lives directly under data_dir — outside every agent home.
        std::fs::write(tmp.path().join("audit.db"), b"secret").expect("seed");
        let outside = tmp.path().join("audit.db").to_string_lossy().to_string();
        for path in ["../../audit.db", outside.as_str()] {
            let err = FilePublishTool::new()
                .execute(
                    serde_json::json!({ "path": path }),
                    ctx(tmp.path(), AgentID::new()),
                )
                .await
                .expect_err("must be denied");
            assert!(
                matches!(err, AgentOSError::PermissionDenied { .. }),
                "{path}: unexpected error: {err}"
            );
        }
        assert!(!tmp.path().join("uploads").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_symlink_that_leaves_the_home() {
        let tmp = TempDir::new().expect("tempdir");
        let c = ctx(tmp.path(), AgentID::new());
        std::fs::write(tmp.path().join("api_keys.db"), b"secret").expect("seed");
        std::os::unix::fs::symlink(
            tmp.path().join("api_keys.db"),
            c.agent_files_dir().expect("home").join("innocent.png"),
        )
        .expect("symlink");
        let err = FilePublishTool::new()
            .execute(serde_json::json!({ "path": "innocent.png" }), c)
            .await
            .expect_err("symlink escape must be denied");
        assert!(
            matches!(err, AgentOSError::PermissionDenied { .. }),
            "unexpected error: {err}"
        );
    }

    /// The workspace gate lives in the tool, not the runner: a granted folder
    /// is still unreadable without `fs.workspace:r`.
    #[tokio::test]
    async fn workspace_file_needs_workspace_read_permission() {
        let tmp = TempDir::new().expect("tempdir");
        let ws = TempDir::new().expect("workspace");
        let ws_root = ws.path().canonicalize().expect("canonical workspace");
        std::fs::write(ws_root.join("chart.png"), PNG).expect("seed");
        let path = ws_root.join("chart.png").to_string_lossy().to_string();

        let mut c = ctx(tmp.path(), AgentID::new());
        c.workspace_paths = vec![ws_root.clone()];
        let err = FilePublishTool::new()
            .execute(serde_json::json!({ "path": path }), c)
            .await
            .expect_err("no fs.workspace:r");
        assert!(
            matches!(err, AgentOSError::PermissionDenied { ref resource, .. } if resource == "fs.workspace"),
            "unexpected error: {err}"
        );

        let mut c = ctx(tmp.path(), AgentID::new());
        c.workspace_paths = vec![ws_root];
        c.permissions
            .grant("fs.workspace".into(), true, false, false, None);
        let out = FilePublishTool::new()
            .execute(serde_json::json!({ "path": path }), c)
            .await
            .expect("granted");
        assert_eq!(out["mime"], "image/png");
    }

    #[tokio::test]
    async fn declares_the_read_permission_file_reader_needs() {
        let perms = FilePublishTool::new().required_permissions();
        assert!(perms.contains(&("fs.artifacts".to_string(), PermissionOp::Write)));
        assert!(perms.contains(&("fs.user_data".to_string(), PermissionOp::Read)));
    }

    #[tokio::test]
    async fn rejects_a_file_over_the_size_cap() {
        let tmp = TempDir::new().expect("tempdir");
        let c = ctx(tmp.path(), AgentID::new());
        // Sparse: a 50 MiB + 1 length without 50 MiB of disk.
        std::fs::File::create(c.agent_files_dir().expect("home").join("big.bin"))
            .and_then(|f| f.set_len(MAX_PUBLISH_BYTES + 1))
            .expect("sparse file");
        let err = FilePublishTool::new()
            .execute(serde_json::json!({ "path": "big.bin" }), c)
            .await
            .expect_err("over the cap");
        assert!(err.to_string().contains("publish limit"), "{err}");
        assert!(!tmp.path().join("uploads").join("published").exists());
    }

    #[tokio::test]
    async fn rejects_empty_files_and_directories() {
        let tmp = TempDir::new().expect("tempdir");
        let agent = AgentID::new();
        let err = publish(&tmp, agent, "dummy.jpg", b"")
            .await
            .expect_err("empty");
        assert!(err.to_string().contains("empty"), "{err}");

        let c = ctx(tmp.path(), agent);
        std::fs::create_dir_all(c.agent_files_dir().expect("home").join("speech")).expect("dir");
        let err = FilePublishTool::new()
            .execute(serde_json::json!({ "path": "speech" }), c)
            .await
            .expect_err("directory");
        assert!(err.to_string().contains("regular file"), "{err}");
    }
}
