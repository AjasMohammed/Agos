use crate::state_store::{KernelStateStore, SnapshotRow};
use agentos_types::{reject_traversal, TaskID};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use uuid::Uuid;

/// A single file captured before modification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub existed_before: bool,
    pub original_content: Option<Vec<u8>>, // None if file didn't exist
    pub captured_at: chrono::DateTime<chrono::Utc>,
}

/// A snapshot of system state before a reversible action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub snap_id: String, // e.g. "snap_4821"
    pub task_id: TaskID,
    pub agent_id: String,
    pub action_type: String, // e.g. "fs.write"
    pub files: Vec<FileSnapshot>,
    pub context_entries: Vec<agentos_types::ContextEntry>,
    pub taken_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub restored: bool,
}

pub struct SnapshotManager {
    /// Durable snapshot index. Every `snap_id` handed out as an
    /// `AuditEntry.rollback_ref` resolves from here, so rollback survives a
    /// kernel restart — the index used to be an in-memory map that was never
    /// hydrated, which silently made `reversible = true` a lie after a reboot.
    store: Arc<KernelStateStore>,
    /// Serialises restores so two concurrent calls for the same snapshot
    /// cannot both observe `restored == false`.
    restore_lock: tokio::sync::Mutex<()>,
    /// Root directory for the on-disk snapshot blobs
    storage_dir: PathBuf,
    /// Canonical allowed root for snapshotted/restored files.
    /// Paths that do not start with this prefix are rejected.
    allowed_root: PathBuf,
    /// How long snapshots are retained (default: 72 hours)
    retention_hours: u64,
}

impl SnapshotManager {
    pub fn new(
        storage_dir: PathBuf,
        allowed_root: PathBuf,
        retention_hours: u64,
        store: Arc<KernelStateStore>,
    ) -> Self {
        // Canonicalize the root at construction time so all comparisons are stable.
        let allowed_root = allowed_root.canonicalize().unwrap_or(allowed_root);
        Self {
            store,
            restore_lock: tokio::sync::Mutex::new(()),
            storage_dir,
            allowed_root,
            retention_hours,
        }
    }

    fn blob_path_for(&self, snap_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{}.json", snap_id))
    }

    /// Read and deserialize the JSON payload behind an index row.
    async fn load_blob(&self, row: &SnapshotRow) -> anyhow::Result<Snapshot> {
        let bytes = tokio::fs::read(&row.blob_path).await.map_err(|e| {
            anyhow::anyhow!(
                "Snapshot {} blob unreadable at {}: {}",
                row.snap_id,
                row.blob_path.display(),
                e
            )
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("Snapshot {} blob is not valid JSON: {}", row.snap_id, e))
    }

    /// Returns `Ok(canonical)` if `path_str` resolves to a location within
    /// `self.allowed_root`, or `Err` if it escapes or contains a traversal.
    fn validate_path(&self, path_str: &str) -> anyhow::Result<PathBuf> {
        reject_traversal(path_str)
            .map_err(|_| anyhow::anyhow!("Path contains '..' traversal: {}", path_str))?;
        let path = PathBuf::from(path_str);
        // Resolve absolute paths directly; relative paths against allowed_root.
        let resolved = if path.is_absolute() {
            path.clone()
        } else {
            self.allowed_root.join(&path)
        };
        // Canonicalize if the path exists; otherwise canonicalize the parent.
        let canonical = if resolved.exists() {
            resolved.canonicalize()?
        } else if let Some(parent) = resolved.parent() {
            let canon_parent = parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf());
            canon_parent.join(resolved.file_name().unwrap_or_default())
        } else {
            resolved.clone()
        };
        if !canonical.starts_with(&self.allowed_root) {
            anyhow::bail!(
                "Path '{}' resolves outside allowed root '{}' — access denied",
                path_str,
                self.allowed_root.display()
            );
        }
        Ok(canonical)
    }

    fn new_snap_id() -> String {
        format!("snap_{}", Uuid::new_v4().simple())
    }

    /// Capture filesystem state before a reversible action.
    /// Returns the snap_id to store in AuditEntry.rollback_ref.
    pub async fn take_snapshot(
        &self,
        task_id: &TaskID,
        agent_id: &str,
        action_type: &str,
        paths: Vec<String>,
        context_entries: Vec<agentos_types::ContextEntry>,
    ) -> anyhow::Result<String> {
        let snap_id = Self::new_snap_id();
        let taken_at = chrono::Utc::now();
        let expires_at = taken_at + chrono::Duration::hours(self.retention_hours as i64);

        let mut file_snapshots = Vec::new();
        for path_str in paths {
            // Validate containment before reading; skip paths that escape the allowed root.
            let validated_path = match self.validate_path(&path_str) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        task_id = %task_id,
                        path = %path_str,
                        "Snapshot skipped path that failed validation: {}",
                        e
                    );
                    continue;
                }
            };
            let existed_before = validated_path.exists();
            let original_content = if existed_before && validated_path.is_file() {
                Some(tokio::fs::read(&validated_path).await?)
            } else {
                None
            };
            file_snapshots.push(FileSnapshot {
                path: path_str,
                existed_before,
                original_content,
                captured_at: chrono::Utc::now(),
            });
        }

        let snapshot = Snapshot {
            snap_id: snap_id.clone(),
            task_id: *task_id,
            agent_id: agent_id.to_string(),
            action_type: action_type.to_string(),
            files: file_snapshots,
            context_entries,
            taken_at,
            expires_at,
            restored: false,
        };

        // Blob first, then the index row. A blob with no row is recoverable by
        // `reconcile_on_boot`; a row pointing at a blob that was never written
        // is not. If indexing fails we unlink the blob and fail the call rather
        // than hand back a snap_id that no rollback could ever resolve.
        let snapshot_file = self.blob_path_for(&snap_id);
        if let Some(parent) = snapshot_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json_bytes = serde_json::to_vec_pretty(&snapshot)?;
        let size_bytes = json_bytes.len() as u64;
        tokio::fs::write(&snapshot_file, json_bytes).await?;

        if let Err(e) = self
            .store
            .insert_snapshot(SnapshotRow {
                snap_id: snap_id.clone(),
                task_id: *task_id,
                agent_id: agent_id.to_string(),
                action_type: action_type.to_string(),
                taken_at,
                expires_at,
                restored: false,
                blob_path: snapshot_file.clone(),
                size_bytes,
            })
            .await
        {
            let _ = tokio::fs::remove_file(&snapshot_file).await;
            return Err(e.context("Failed to index snapshot; blob discarded"));
        }

        Ok(snap_id)
    }

    /// Restore filesystem state from a snapshot.
    pub async fn restore(&self, snap_id: &str) -> anyhow::Result<Snapshot> {
        // Serialise restores: the `restored` check and the mark must not
        // interleave with a concurrent restore of the same snapshot.
        let _restore_guard = self.restore_lock.lock().await;

        let row = self
            .store
            .get_snapshot(snap_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Snapshot {} not found", snap_id))?;

        if row.restored {
            return Err(anyhow::anyhow!("Snapshot {} already restored", snap_id));
        }

        let mut snapshot = self.load_blob(&row).await?;

        for file_snap in &snapshot.files {
            // Validate containment before any I/O — a tampered on-disk snapshot
            // could contain crafted paths (absolute or with `..`) to escape the root.
            let validated_path = self.validate_path(&file_snap.path).map_err(|e| {
                anyhow::anyhow!(
                    "Snapshot {} path failed validation — restore aborted: {}",
                    snap_id,
                    e
                )
            })?;
            if file_snap.existed_before {
                if let Some(ref content) = file_snap.original_content {
                    if let Some(parent) = validated_path.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::write(&validated_path, content).await?;
                }
            } else {
                // File didn't exist before, so delete it if it exists now
                if validated_path.exists() {
                    if validated_path.is_file() {
                        tokio::fs::remove_file(&validated_path).await?;
                    } else if validated_path.is_dir() {
                        tokio::fs::remove_dir_all(&validated_path).await?;
                    }
                }
            }
        }

        // Durable mark first — it is the single-winner guard. Only then update
        // the blob, which is a convenience copy of the same fact.
        if !self.store.mark_snapshot_restored(snap_id).await? {
            return Err(anyhow::anyhow!("Snapshot {} already restored", snap_id));
        }
        snapshot.restored = true;

        if let Ok(json_bytes) = serde_json::to_vec_pretty(&snapshot) {
            if let Err(e) = tokio::fs::write(&row.blob_path, json_bytes).await {
                tracing::warn!(
                    snap_id = %snap_id,
                    error = %e,
                    "Restored snapshot but could not update its blob; index is authoritative"
                );
            }
        }

        Ok(snapshot)
    }

    /// Find all snapshots for a given task.
    pub async fn snapshots_for_task(&self, task_id: &TaskID) -> Vec<Snapshot> {
        let rows = match self.store.list_snapshots_for_task(task_id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(task_id = %task_id, error = %e, "Failed to list snapshots");
                return Vec::new();
            }
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            match self.load_blob(&row).await {
                Ok(snap) => out.push(snap),
                // A row whose blob is unreadable is dropped by the next
                // `reconcile_on_boot`; skipping it here keeps listing usable.
                Err(e) => tracing::warn!(snap_id = %row.snap_id, error = %e, "Skipping snapshot"),
            }
        }
        out
    }

    /// Index rows for a task without reading the blobs.
    pub async fn snapshot_rows_for_task(&self, task_id: &TaskID) -> Vec<SnapshotRow> {
        self.store
            .list_snapshots_for_task(task_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(task_id = %task_id, error = %e, "Failed to list snapshot rows");
                Vec::new()
            })
    }

    /// Delete expired snapshots: unlink the blob, then drop the index row.
    pub async fn sweep_expired(&self) -> usize {
        let rows = match self.store.list_expired_snapshots(chrono::Utc::now()).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Expired-snapshot query failed");
                return 0;
            }
        };

        let mut deleted = 0usize;
        for row in rows {
            match tokio::fs::remove_file(&row.blob_path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    // Leave the row so the next sweep retries the unlink rather
                    // than orphaning the file forever.
                    tracing::warn!(snap_id = %row.snap_id, error = %e, "Snapshot blob unlink failed");
                    continue;
                }
            }
            match self.store.delete_snapshot(&row.snap_id).await {
                Ok(true) => deleted += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(snap_id = %row.snap_id, error = %e, "Snapshot row delete failed")
                }
            }
        }
        deleted
    }

    /// Reconcile the durable index against the blobs on disk. Returns
    /// `(adopted, dropped)`.
    ///
    /// Adopts blobs written before the index existed (or by a build that
    /// crashed between blob and row) and drops rows whose blob is gone. Never
    /// deletes a blob it cannot parse — an unreadable file is a diagnostic,
    /// not garbage.
    pub async fn reconcile_on_boot(&self) -> anyhow::Result<(usize, usize)> {
        let rows = self.store.list_all_snapshots().await?;
        let mut known: HashSet<String> = HashSet::with_capacity(rows.len());
        let mut dropped = 0usize;

        for row in rows {
            if tokio::fs::metadata(&row.blob_path).await.is_err() {
                if self
                    .store
                    .delete_snapshot(&row.snap_id)
                    .await
                    .unwrap_or(false)
                {
                    dropped += 1;
                }
            } else {
                known.insert(row.snap_id);
            }
        }

        let mut adopted = 0usize;
        let mut dir = match tokio::fs::read_dir(&self.storage_dir).await {
            Ok(d) => d,
            // No storage dir yet simply means nothing to adopt.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, dropped)),
            Err(e) => return Err(e.into()),
        };

        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if known.contains(stem) {
                continue;
            }
            let bytes = match tokio::fs::read(&path).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Orphan snapshot unreadable");
                    continue;
                }
            };
            let snap: Snapshot = match serde_json::from_slice(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Orphan snapshot not parseable; leaving file in place");
                    continue;
                }
            };
            let row = SnapshotRow {
                snap_id: snap.snap_id.clone(),
                task_id: snap.task_id,
                agent_id: snap.agent_id.clone(),
                action_type: snap.action_type.clone(),
                taken_at: snap.taken_at,
                expires_at: snap.expires_at,
                restored: snap.restored,
                blob_path: path.clone(),
                size_bytes: bytes.len() as u64,
            };
            match self.store.insert_snapshot(row).await {
                Ok(()) => adopted += 1,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Failed to adopt orphan snapshot")
                }
            }
        }

        tracing::info!(adopted, dropped, "Snapshot index reconciled");
        Ok((adopted, dropped))
    }
}

impl crate::kernel::Kernel {
    pub async fn take_snapshot(
        &self,
        task_id: &TaskID,
        action_type: &str,
        payload: Option<&serde_json::Value>,
    ) -> Option<String> {
        let agent_id = {
            if let Some(task) = self.scheduler.get_task(task_id).await {
                task.agent_id.to_string()
            } else {
                "system".to_string()
            }
        };

        // Extract potential file paths from payload (containment validation is done inside
        // SnapshotManager::take_snapshot via validate_path before any I/O).
        let mut paths = Vec::new();
        if let Some(p) = payload {
            for field in &["path", "target", "file"] {
                if let Some(path_str) = p.get(*field).and_then(|v| v.as_str()) {
                    paths.push(path_str.to_string());
                }
            }
        }

        // Capture context entries
        let context_entries = if let Ok(window) = self.context_manager.get_context(task_id).await {
            window.entries.clone()
        } else {
            Vec::new()
        };

        match self
            .snapshot_manager
            .take_snapshot(task_id, &agent_id, action_type, paths, context_entries)
            .await
        {
            Ok(sid) => Some(sid),
            Err(e) => {
                tracing::warn!(task_id = %task_id, error = %e, "Failed to take snapshot");
                None
            }
        }
    }

    pub(crate) async fn cmd_list_snapshots(&self, task_id: TaskID) -> agentos_bus::KernelResponse {
        // Listing reads index rows only — no need to pull every blob off disk.
        let rows = self.snapshot_manager.snapshot_rows_for_task(&task_id).await;
        let entries: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "task_id": s.task_id.to_string(),
                    "snapshot_ref": s.snap_id,
                    "action_type": s.action_type,
                    "size_bytes": s.size_bytes,
                    "restored": s.restored,
                    "created_at_unix": s.taken_at.timestamp(),
                })
            })
            .collect();

        agentos_bus::KernelResponse::SnapshotList(entries)
    }

    pub(crate) async fn cmd_rollback_task(
        &self,
        task_id: TaskID,
        snapshot_ref: Option<String>,
    ) -> agentos_bus::KernelResponse {
        // Resolve snapshot_ref
        let snap_ref = if let Some(r) = snapshot_ref {
            r
        } else {
            // Find latest for task
            let rows = self.snapshot_manager.snapshot_rows_for_task(&task_id).await;
            if let Some(latest) = rows.into_iter().max_by_key(|s| s.taken_at) {
                latest.snap_id
            } else {
                return agentos_bus::KernelResponse::Error {
                    message: format!("No snapshots found for task {}", task_id),
                };
            }
        };

        match self.snapshot_manager.restore(&snap_ref).await {
            Ok(snap) => {
                // Restore context entries
                if let Ok(mut window) = self.context_manager.get_context(&task_id).await {
                    window.clear_unpinned();
                    for entry in snap.context_entries {
                        window.push(entry);
                    }
                    self.context_manager
                        .replace_context(&task_id, window)
                        .await
                        .ok();
                }

                // Log audit event
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::SnapshotRestored,
                    agent_id: Some(
                        snap.agent_id
                            .parse()
                            .unwrap_or(agentos_types::AgentID::new()),
                    ),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "snapshot_ref": snap_ref,
                        "task_id": task_id.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                agentos_bus::KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "status": "rolled_back",
                        "task_id": task_id.to_string(),
                        "snapshot_ref": snap_ref,
                    })),
                }
            }
            Err(e) => agentos_bus::KernelResponse::Error {
                message: format!("Rollback failed: {}", e),
            },
        }
    }

    /// Fire-and-forget expiry sweep. Retention comes from the manager's own
    /// `retention_hours`; the old `retention` parameter was ignored, so it is
    /// gone rather than left as a lie in the signature.
    pub fn sweep_expired_snapshots(self: &std::sync::Arc<Self>) {
        let kernel = self.clone();
        tokio::spawn(async move {
            let count = kernel.snapshot_manager.sweep_expired().await;
            if count > 0 {
                tracing::info!("Swept {} expired snapshots", count);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;
    use tempfile::tempdir;

    /// Build a manager over a real state DB in `dir`. Returning the store lets
    /// a test reopen a second manager on the same durable index — which is how
    /// the across-restart behaviour is exercised.
    async fn manager_in(dir: &std::path::Path, retention_hours: u64) -> SnapshotManager {
        let store = Arc::new(
            crate::state_store::KernelStateStore::open(dir.join("state.db"))
                .await
                .expect("open state db"),
        );
        SnapshotManager::new(dir.join("snaps"), dir.join("work"), retention_hours, store)
    }

    fn sample_entries() -> Vec<ContextEntry> {
        vec![ContextEntry {
            role: ContextRole::User,
            parts: vec![ContentPart::Text {
                text: "hello".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        }]
    }

    #[tokio::test]
    async fn test_snapshot_take_and_restore() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let storage_dir = dir.path().join("snaps");
        let work_dir = dir.path().join("work");
        tokio::fs::create_dir_all(&work_dir).await?;

        let _ = &storage_dir;
        let manager = manager_in(dir.path(), 72).await;
        let task_id = TaskID::new();

        // Create a test file
        let test_file = work_dir.join("test.txt");
        tokio::fs::write(&test_file, "original content").await?;

        // Create some context entries
        let entries = vec![ContextEntry {
            role: ContextRole::User,
            parts: vec![ContentPart::Text {
                text: "hello".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        }];

        // Take snapshot
        let snap_id = manager
            .take_snapshot(
                &task_id,
                "agent_1",
                "fs.write",
                vec![test_file.to_str().unwrap().to_string()],
                entries.clone(),
            )
            .await?;

        // Modify file
        tokio::fs::write(&test_file, "modified content").await?;

        // Restore
        let restored_snap = manager.restore(&snap_id).await?;
        assert_eq!(restored_snap.snap_id, snap_id);
        assert_eq!(restored_snap.context_entries.len(), 1);
        assert_eq!(restored_snap.context_entries[0].text(), "hello");

        // Verify file content restored
        let restored_content = tokio::fs::read_to_string(&test_file).await?;
        assert_eq!(restored_content, "original content");

        Ok(())
    }

    #[tokio::test]
    async fn test_snapshot_restore_non_existent_file() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let storage_dir = dir.path().join("snaps");
        let work_dir = dir.path().join("work");
        tokio::fs::create_dir_all(&work_dir).await?;

        let _ = &storage_dir;
        let manager = manager_in(dir.path(), 72).await;
        let task_id = TaskID::new();

        let new_file = work_dir.join("new.txt");

        // Take snapshot before file exists
        let snap_id = manager
            .take_snapshot(
                &task_id,
                "agent_1",
                "fs.create",
                vec![new_file.to_str().unwrap().to_string()],
                vec![],
            )
            .await?;

        // Create file
        tokio::fs::write(&new_file, "i exist now").await?;
        assert!(new_file.exists());

        // Restore (should delete the file)
        manager.restore(&snap_id).await?;
        assert!(!new_file.exists());

        Ok(())
    }

    /// THE enforcement test for this phase's rule: an id handed to the audit
    /// log as `rollback_ref` must still resolve after the process restarts.
    /// Manager A takes the snapshot and is dropped; manager B — a fresh
    /// instance over the same state.db and storage dir, i.e. a new boot —
    /// must be able to roll it back.
    #[tokio::test]
    async fn test_restore_survives_restart() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let work_dir = dir.path().join("work");
        tokio::fs::create_dir_all(&work_dir).await?;
        let test_file = work_dir.join("test.txt");
        tokio::fs::write(&test_file, "original content").await?;

        let snap_id = {
            let a = manager_in(dir.path(), 72).await;
            a.take_snapshot(
                &TaskID::new(),
                "agent_1",
                "fs.write",
                vec![test_file.to_str().unwrap().to_string()],
                sample_entries(),
            )
            .await?
        }; // manager A dropped — this is the "restart"

        tokio::fs::write(&test_file, "modified content").await?;

        let b = manager_in(dir.path(), 72).await;
        let restored = b.restore(&snap_id).await?;
        assert_eq!(restored.snap_id, snap_id);
        assert_eq!(
            tokio::fs::read_to_string(&test_file).await?,
            "original content"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_reconcile_adopts_orphan_blob() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let work_dir = dir.path().join("work");
        tokio::fs::create_dir_all(&work_dir).await?;
        let snaps_dir = dir.path().join("snaps");
        tokio::fs::create_dir_all(&snaps_dir).await?;

        let task_id = TaskID::new();
        let orphan = Snapshot {
            snap_id: "snap_orphan01".to_string(),
            task_id,
            agent_id: "agent_1".to_string(),
            action_type: "fs.write".to_string(),
            files: vec![],
            context_entries: vec![],
            taken_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(72),
            restored: false,
        };
        tokio::fs::write(
            snaps_dir.join("snap_orphan01.json"),
            serde_json::to_vec_pretty(&orphan)?,
        )
        .await?;

        let m = manager_in(dir.path(), 72).await;
        assert_eq!(m.reconcile_on_boot().await?, (1, 0));
        assert_eq!(m.snapshot_rows_for_task(&task_id).await.len(), 1);
        // Idempotent: a second boot adopts nothing.
        assert_eq!(m.reconcile_on_boot().await?, (0, 0));
        Ok(())
    }

    #[tokio::test]
    async fn test_reconcile_drops_row_without_blob() -> anyhow::Result<()> {
        let dir = tempdir()?;
        tokio::fs::create_dir_all(dir.path().join("work")).await?;
        tokio::fs::create_dir_all(dir.path().join("snaps")).await?;

        let store = Arc::new(
            crate::state_store::KernelStateStore::open(dir.path().join("state.db")).await?,
        );
        store
            .insert_snapshot(SnapshotRow {
                snap_id: "snap_ghost".to_string(),
                task_id: TaskID::new(),
                agent_id: "agent_1".to_string(),
                action_type: "fs.write".to_string(),
                taken_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(72),
                restored: false,
                blob_path: dir.path().join("snaps").join("snap_ghost.json"),
                size_bytes: 10,
            })
            .await?;

        let m = SnapshotManager::new(
            dir.path().join("snaps"),
            dir.path().join("work"),
            72,
            Arc::clone(&store),
        );
        assert_eq!(m.reconcile_on_boot().await?, (0, 1));
        assert!(store.get_snapshot("snap_ghost").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_sweep_deletes_row_and_unlinks_blob() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let work_dir = dir.path().join("work");
        tokio::fs::create_dir_all(&work_dir).await?;
        let f = work_dir.join("a.txt");
        tokio::fs::write(&f, "x").await?;

        // retention 0 => expires_at == taken_at, already in the past by the
        // time the sweep runs.
        let m = manager_in(dir.path(), 0).await;
        let snap_id = m
            .take_snapshot(
                &TaskID::new(),
                "agent_1",
                "fs.write",
                vec![f.to_str().unwrap().to_string()],
                vec![],
            )
            .await?;
        let blob = dir.path().join("snaps").join(format!("{}.json", snap_id));
        assert!(blob.exists());

        assert_eq!(m.sweep_expired().await, 1);
        assert!(!blob.exists());
        assert!(m.restore(&snap_id).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_restore_twice_rejected() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let work_dir = dir.path().join("work");
        tokio::fs::create_dir_all(&work_dir).await?;
        let f = work_dir.join("a.txt");
        tokio::fs::write(&f, "original").await?;

        let m = manager_in(dir.path(), 72).await;
        let snap_id = m
            .take_snapshot(
                &TaskID::new(),
                "agent_1",
                "fs.write",
                vec![f.to_str().unwrap().to_string()],
                vec![],
            )
            .await?;

        assert!(m.restore(&snap_id).await.is_ok());
        let second = m.restore(&snap_id).await;
        assert!(second.is_err(), "second restore must be rejected");
        assert!(second.unwrap_err().to_string().contains("already restored"));
        Ok(())
    }

    /// Two concurrent restores of the same snapshot: exactly one wins.
    #[tokio::test]
    async fn test_concurrent_restore_single_winner() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let work_dir = dir.path().join("work");
        tokio::fs::create_dir_all(&work_dir).await?;
        let f = work_dir.join("a.txt");
        tokio::fs::write(&f, "original").await?;

        let m = Arc::new(manager_in(dir.path(), 72).await);
        let snap_id = m
            .take_snapshot(
                &TaskID::new(),
                "agent_1",
                "fs.write",
                vec![f.to_str().unwrap().to_string()],
                vec![],
            )
            .await?;

        let (m1, m2) = (Arc::clone(&m), Arc::clone(&m));
        let (id1, id2) = (snap_id.clone(), snap_id.clone());
        let (r1, r2) = tokio::join!(
            tokio::spawn(async move { m1.restore(&id1).await.is_ok() }),
            tokio::spawn(async move { m2.restore(&id2).await.is_ok() }),
        );
        let wins = u8::from(r1?) + u8::from(r2?);
        assert_eq!(wins, 1, "exactly one restore should succeed");
        Ok(())
    }
}
