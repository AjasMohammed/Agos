//! FileStore-backed implementations of the kernel's two media slots:
//! [`agentos_llm::ImageResolver`] (chat `FileRef` → base64 for vision) and
//! [`crate::attachment_sink::AttachmentSink`] (inbound channel media → a stable
//! file id).
//!
//! These used to live in `agentos-web` and were installed only by
//! `WebServer::new`, so `agentos start` (kernel + REST API, no HTMX server) and
//! `agentos gateway run` left both slots on their no-op defaults: every Telegram
//! voice note, photo and document was downloaded and then dropped with
//! `attachment sink declined; inbound media NOT persisted`. The FileStore is
//! owned by the kernel, so the kernel wires them itself at boot.

use std::sync::Arc;

use agentos_llm::media::MAX_INLINE_IMAGE_BYTES;
use agentos_types::AgentOSError;
use base64::Engine;
use uuid::Uuid;

use crate::file_store::FileStore;

/// How long a derived (extracted-text) page survives before the opportunistic
/// sweep deletes it.
pub const DERIVED_PAGE_TTL_HOURS: u32 = 24;

/// How long inbound channel media (Telegram photos, Discord attachments, …)
/// survives before the opportunistic sweep deletes it.
pub const INBOUND_MEDIA_TTL_HOURS: u32 = 24 * 30;

/// Delete expired derived pages, rows and bytes both.
pub fn prune_derived_pages(store: &FileStore) {
    let stale = match store.prune_derived(DERIVED_PAGE_TTL_HOURS) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "store_derived_page: prune failed");
            return;
        }
    };
    unlink_pruned(store, stale, "prune_derived");
}

/// Delete expired inbound channel media, rows and bytes both. Opportunistic:
/// the sink write path is the only code that runs often enough to sweep, so it
/// sweeps.
pub fn prune_inbound_media(store: &FileStore) {
    let stale = match store.prune_inbound_media(INBOUND_MEDIA_TTL_HOURS) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "inbound media prune failed");
            return;
        }
    };
    unlink_pruned(store, stale, "prune_inbound_media");
}

/// Unlink the bytes behind rows a prune already deleted, plus any copies agents
/// materialized from them.
pub fn unlink_pruned(store: &FileStore, stale: Vec<(String, String)>, context: &str) {
    if stale.is_empty() {
        return;
    }
    remove_agent_handles(store, stale.iter().map(|(id, _)| id.as_str()), context);
    let uploads = match store.uploads_dir.canonicalize() {
        Ok(u) => u,
        Err(e) => {
            // The rows are already gone, so nothing will retry these paths.
            tracing::warn!(context, error = %e, stale = stale.len(),
                "cannot canonicalize uploads_dir; pruned bytes orphaned");
            return;
        }
    };
    for (_, path) in stale {
        let p = std::path::PathBuf::from(&path);
        match p.canonicalize() {
            Ok(c) if c.starts_with(&uploads) => {
                if let Err(e) = std::fs::remove_file(&c) {
                    // The row is already gone, so nothing will retry: say so
                    // rather than leaking bytes silently.
                    tracing::warn!(context, path = %c.display(), error = %e, "row deleted but bytes remain on disk");
                }
            }
            Ok(c) => {
                tracing::warn!(context, path = %c.display(), "path escapes uploads_dir, not deleting")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(context, path = %p.display(), error = %e,
                    "cannot resolve pruned path; bytes may be orphaned")
            }
        }
    }
}

/// Remove the per-agent copies `user-file-reader` materialized for these uploads.
///
/// `mode: "handle"` hard-links (or copies) an upload into
/// `<data_dir>/agents/<agent>/inbox/<file_id>/` so path-taking tools can reach it
/// without being pointed at the kernel state dir. Those copies outlive the
/// registry row unless the same sweep takes them, and a hard link keeps the
/// bytes alive on disk even after the original is unlinked.
pub fn remove_agent_handles<'a>(
    store: &FileStore,
    ids: impl Iterator<Item = &'a str>,
    context: &str,
) {
    let Some(agents_dir) = store.uploads_dir.parent().map(|d| d.join("agents")) else {
        return;
    };
    let ids: Vec<&str> = ids.collect();
    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(e) => e,
        // No agent has ever taken a handle: nothing to sweep.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(context, error = %e, "cannot scan agent homes; handles may be orphaned");
            return;
        }
    };
    for agent in entries.flatten() {
        let inbox = agent.path().join("inbox");
        for id in &ids {
            // The id is a registry primary key (a UUID), never a path from a
            // user, but this joins it as a path component and then recursively
            // deletes the result, so it is checked anyway. It must be one
            // *Normal* component: counting components is not enough, because
            // `..` is a single component and `inbox/..` is the agent's whole
            // home directory.
            let mut parts = std::path::Path::new(id).components();
            let one_normal = matches!(parts.next(), Some(std::path::Component::Normal(c)) if c == std::ffi::OsStr::new(id))
                && parts.next().is_none();
            if !one_normal {
                tracing::warn!(context, id, "refusing to sweep a malformed file id");
                continue;
            }
            let dir = inbox.join(id);
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => {
                    tracing::debug!(context, path = %dir.display(), "removed agent file handle")
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(context, path = %dir.display(), error = %e,
                    "row deleted but agent handle remains on disk"),
            }
        }
    }
}

/// Resolves uploaded file IDs to `(mime, base64)` for multimodal LLM adapters.
pub struct FileStoreImageResolver {
    store: Arc<FileStore>,
    uploads_canon: std::path::PathBuf,
}

impl FileStoreImageResolver {
    pub fn new(store: Arc<FileStore>) -> Result<Self, std::io::Error> {
        Ok(Self {
            uploads_canon: store.uploads_dir.canonicalize()?,
            store,
        })
    }
}

impl agentos_llm::ImageResolver for FileStoreImageResolver {
    fn resolve_filename(&self, file_id: &str) -> Option<String> {
        self.store
            .get_file_by_id_unscoped(file_id)
            .ok()
            .flatten()
            .map(|r| r.original_name)
    }

    fn resolve_base64(&self, file_id: &str) -> Result<(String, String), AgentOSError> {
        let record =
            self.store
                .get_file_by_id_unscoped(file_id)
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("file lookup: {e}"),
                })?;
        let Some(record) = record else {
            return Err(AgentOSError::LLMError {
                provider: "file-store".to_string(),
                reason: format!("unknown file_id {file_id}"),
            });
        };
        let disk_path = std::path::PathBuf::from(&record.path);
        let canonical = disk_path
            .canonicalize()
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("canonicalize: {e}"),
            })?;
        if !canonical.starts_with(&self.uploads_canon) {
            return Err(AgentOSError::KernelError {
                reason: "path escapes uploads directory".into(),
            });
        }
        let bytes = std::fs::read(&canonical).map_err(|e| AgentOSError::KernelError {
            reason: format!("read: {e}"),
        })?;
        if bytes.len() > MAX_INLINE_IMAGE_BYTES {
            return Err(AgentOSError::SchemaValidation(format!(
                "image exceeds max inline size ({} bytes)",
                MAX_INLINE_IMAGE_BYTES
            )));
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok((record.mime, b64))
    }
}

/// Persists inbound channel media (Telegram photos/docs/voice, Discord and
/// Slack attachments, WhatsApp documents) into the FileStore at global scope,
/// mirroring the HTTP upload path, so downloaded media gets a stable,
/// resolvable file id.
pub struct FileStoreAttachmentSink {
    store: Arc<FileStore>,
}

impl FileStoreAttachmentSink {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl crate::attachment_sink::AttachmentSink for FileStoreAttachmentSink {
    async fn store(
        &self,
        original_name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<String, String> {
        let store = Arc::clone(&self.store);
        let original_name = original_name.to_string();
        let mime = mime.to_string();
        tokio::task::spawn_blocking(move || -> Result<String, String> {
            // Opportunistic GC, same as the derived-page write path: inbound
            // media has no other sweeper and no operator watching it.
            prune_inbound_media(&store);

            let file_id = Uuid::new_v4().to_string();
            let safe_part = crate::file_store::sanitize_storage_name(&original_name);
            let stored_name = format!("{file_id}_{safe_part}");
            let disk_path = store.uploads_dir.join(&stored_name);
            let disk_path_str = disk_path.to_string_lossy().to_string();
            let size = bytes.len() as u64;
            std::fs::write(&disk_path, &bytes).map_err(|e| format!("write to disk: {e}"))?;
            if let Err(e) = store.register_file(
                &file_id,
                &original_name,
                &mime,
                size,
                &disk_path_str,
                // Load-bearing: `FileStore::prune_inbound_media` sweeps on this
                // tag and `user-file-reader` refuses to resolve these rows by
                // name because of it. Was "inbound,telegram" — which was a lie
                // on every Discord, Slack, Matrix and WhatsApp attachment.
                "inbound",
                "",
                "global",
            ) {
                let _ = std::fs::remove_file(&disk_path_str);
                return Err(format!("register in db: {e}"));
            }
            Ok(file_id)
        })
        .await
        .map_err(|e| format!("storage task join error: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle is a hard link, so unlinking the upload alone leaves the bytes
    /// alive under the agent's home: "delete this file" would hand the agent a
    /// permanent readable copy of the file the user just deleted.
    #[test]
    fn pruning_a_row_also_removes_the_handles_agents_took() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStore::open(dir.path()).expect("store");

        let id = "11111111-1111-1111-1111-111111111111";
        let upload = store.uploads_dir.join(format!("{id}_f.bin"));
        std::fs::write(&upload, b"bytes").expect("write");

        let handle_dir = dir
            .path()
            .join("agents")
            .join("Nemo3")
            .join("inbox")
            .join(id);
        std::fs::create_dir_all(&handle_dir).expect("mkdir");
        let handle = handle_dir.join("f.bin");
        std::fs::hard_link(&upload, &handle).expect("link");

        unlink_pruned(
            &store,
            vec![(id.to_string(), upload.to_string_lossy().to_string())],
            "test",
        );

        assert!(!upload.exists(), "upload bytes survived");
        assert!(!handle_dir.exists(), "agent handle survived the delete");
    }

    /// The id is a registry primary key, but the sweep joins it as a path
    /// component — a malformed row must not walk it out of the inbox.
    #[test]
    fn a_malformed_id_cannot_walk_out_of_the_inbox() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStore::open(dir.path()).expect("store");

        let agent = dir.path().join("agents").join("Nemo3");
        std::fs::create_dir_all(agent.join("inbox")).expect("mkdir");
        let bystander = agent.join("notes.md");
        std::fs::write(&bystander, b"agent's own file").expect("write");

        remove_agent_handles(&store, ["../notes.md", "..", "a/b"].into_iter(), "test");

        assert!(bystander.exists(), "sweep escaped the inbox");
    }
}
