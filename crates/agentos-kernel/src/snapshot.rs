use agentos_types::TaskID;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};
use tokio::sync::RwLock;
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
    /// In-memory snapshot store (keyed by snap_id)
    snapshots: RwLock<HashMap<String, Snapshot>>,
    /// Root directory for any on-disk snapshot blobs
    storage_dir: PathBuf,
    /// Canonical allowed root for snapshotted/restored files.
    /// Paths that do not start with this prefix are rejected.
    allowed_root: PathBuf,
    /// How long snapshots are retained (default: 72 hours)
    retention_hours: u64,
}

impl SnapshotManager {
    pub fn new(storage_dir: PathBuf, allowed_root: PathBuf, retention_hours: u64) -> Self {
        // Canonicalize the root at construction time so all comparisons are stable.
        let allowed_root = allowed_root.canonicalize().unwrap_or(allowed_root);
        Self {
            snapshots: RwLock::new(HashMap::new()),
            storage_dir,
            allowed_root,
            retention_hours,
        }
    }

    /// Returns `Ok(canonical)` if `path_str` resolves to a location within
    /// `self.allowed_root`, or `Err` if it escapes or contains a traversal.
    fn validate_path(&self, path_str: &str) -> anyhow::Result<PathBuf> {
        if path_str.contains("..") {
            anyhow::bail!("Path contains '..' traversal: {}", path_str);
        }
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

        self.snapshots
            .write()
            .await
            .insert(snap_id.clone(), snapshot.clone());

        // Save to disk summary
        let snapshot_file = self.storage_dir.join(format!("{}.json", snap_id));
        if let Some(parent) = snapshot_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json_bytes = serde_json::to_vec_pretty(&snapshot)?;
        tokio::fs::write(&snapshot_file, json_bytes).await?;

        Ok(snap_id)
    }

    /// Restore filesystem state from a snapshot.
    pub async fn restore(&self, snap_id: &str) -> anyhow::Result<Snapshot> {
        let mut snaps_guard = self.snapshots.write().await;
        let snapshot = snaps_guard
            .get_mut(snap_id)
            .ok_or_else(|| anyhow::anyhow!("Snapshot {} not found", snap_id))?;

        if snapshot.restored {
            return Err(anyhow::anyhow!("Snapshot {} already restored", snap_id));
        }

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

        snapshot.restored = true;

        // Update on-disk snapshot record
        let snapshot_file = self.storage_dir.join(format!("{}.json", snap_id));
        if snapshot_file.exists() {
            let json_bytes = serde_json::to_vec_pretty(&snapshot)?;
            tokio::fs::write(&snapshot_file, json_bytes).await?;
        }

        Ok(snapshot.clone())
    }

    /// Find all snapshots for a given task.
    pub async fn snapshots_for_task(&self, task_id: &TaskID) -> Vec<Snapshot> {
        let snaps_guard = self.snapshots.read().await;
        snaps_guard
            .values()
            .filter(|s| s.task_id == *task_id)
            .cloned()
            .collect()
    }

    /// Delete expired snapshots.
    pub async fn sweep_expired(&self) -> usize {
        let mut snaps_guard = self.snapshots.write().await;
        let now = chrono::Utc::now();
        let mut keys_to_remove = Vec::new();

        for (id, snap) in snaps_guard.iter() {
            if snap.expires_at < now {
                keys_to_remove.push(id.clone());
            }
        }

        let count = keys_to_remove.len();
        for key in keys_to_remove {
            snaps_guard.remove(&key);
            let snapshot_file = self.storage_dir.join(format!("{}.json", key));
            let _ = tokio::fs::remove_file(snapshot_file).await;
        }

        count
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
        let snaps = self.snapshot_manager.snapshots_for_task(&task_id).await;
        let entries: Vec<serde_json::Value> = snaps
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "task_id": s.task_id.to_string(),
                    "snapshot_ref": s.snap_id,
                    "action_type": s.action_type,
                    "size_bytes": 0, // In-memory size not tracked yet
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
            let snaps = self.snapshot_manager.snapshots_for_task(&task_id).await;
            if let Some(latest) = snaps.into_iter().max_by_key(|s| s.taken_at) {
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

    pub fn sweep_expired_snapshots(self: &std::sync::Arc<Self>, _retention: std::time::Duration) {
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

    #[tokio::test]
    async fn test_snapshot_take_and_restore() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let storage_dir = dir.path().join("snaps");
        let work_dir = dir.path().join("work");
        tokio::fs::create_dir_all(&work_dir).await?;

        let manager = SnapshotManager::new(storage_dir, work_dir.clone(), 72);
        let task_id = TaskID::new();

        // Create a test file
        let test_file = work_dir.join("test.txt");
        tokio::fs::write(&test_file, "original content").await?;

        // Create some context entries
        let entries = vec![ContextEntry {
            role: ContextRole::User,
            content: "hello".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
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
        assert_eq!(restored_snap.context_entries[0].content, "hello");

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

        let manager = SnapshotManager::new(storage_dir, work_dir.clone(), 72);
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
}
