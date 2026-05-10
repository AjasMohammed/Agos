//! Minimal disk persistence for the in-memory schedule manager.
//!
//! Snapshots `ScheduledJob` / `OnceJob` / `TimerEntry` plus the creator
//! side-table to a single JSON file under the kernel data dir. Write-through
//! on every mutation; load on boot.
//!
//! Scope: just enough to survive a kernel restart. Per-fire run history,
//! delivery routing, and the elaborate SQL schema in `schedule_store.rs` are
//! tracked separately under
//! `obsidian-vault/plans/scheduled-task-delivery/`.

use agentos_types::schedule::{OnceJob, ScheduledJob, TimerEntry};
use agentos_types::{AgentID, ScheduleID};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

const SNAPSHOT_FILE: &str = "schedules.json";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ScheduleSnapshot {
    #[serde(default)]
    pub jobs: HashMap<ScheduleID, ScheduledJob>,
    #[serde(default)]
    pub once_jobs: HashMap<ScheduleID, OnceJob>,
    #[serde(default)]
    pub timers: HashMap<ScheduleID, TimerEntry>,
    #[serde(default)]
    pub creators: HashMap<ScheduleID, AgentID>,
}

/// Disk-backed snapshot. Serializes the entire state on every flush.
/// At realistic schedule volumes (10s–100s of entries) this is fine and
/// avoids the complexity of incremental SQLite migrations.
pub struct SchedulePersistence {
    path: PathBuf,
    /// Single mutex serialises writes so concurrent flushes can't tear the
    /// file. `tokio::sync::Mutex` because flush is async (spawn_blocking).
    write_lock: Mutex<()>,
}

impl SchedulePersistence {
    /// Open the snapshot at `<data_dir>/schedules.json`. Creates the parent
    /// dir if missing. Does NOT load — call `load()` separately on boot.
    pub fn new(data_dir: &Path) -> anyhow::Result<Self> {
        if !data_dir.exists() {
            std::fs::create_dir_all(data_dir)?;
        }
        let path = data_dir.join(SNAPSHOT_FILE);
        Ok(Self {
            path,
            write_lock: Mutex::new(()),
        })
    }

    /// Read the snapshot from disk. Returns an empty snapshot if the file
    /// doesn't exist (first boot).
    pub async fn load(&self) -> anyhow::Result<ScheduleSnapshot> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<ScheduleSnapshot> {
            if !path.exists() {
                return Ok(ScheduleSnapshot::default());
            }
            let bytes = std::fs::read(&path)?;
            if bytes.is_empty() {
                return Ok(ScheduleSnapshot::default());
            }
            let snap: ScheduleSnapshot = serde_json::from_slice(&bytes).map_err(|e| {
                anyhow::anyhow!("Failed to parse schedule snapshot at {:?}: {}", path, e)
            })?;
            Ok(snap)
        })
        .await
        .map_err(|e| anyhow::anyhow!("schedule snapshot load join error: {}", e))?
    }

    /// Atomically write the snapshot to disk. Uses write-rename so a
    /// concurrent reader never observes a partial file.
    pub async fn flush(&self, snapshot: ScheduleSnapshot) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let tmp = path.with_extension("json.tmp");
            let json = serde_json::to_vec_pretty(&snapshot)?;
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("schedule snapshot flush join error: {}", e))??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::schedule::{OnceJobAction, ScheduleState};
    use tempfile::TempDir;

    fn sample_job() -> ScheduledJob {
        ScheduledJob {
            id: ScheduleID::new(),
            name: "daily".into(),
            cron_expression: "0 0 9 * * *".into(),
            timezone: None,
            agent_name: "alice".into(),
            task_prompt: "post update".into(),
            permissions: vec![],
            state: ScheduleState::Active,
            created_at: chrono::Utc::now(),
            last_run_at: None,
            next_run_at: None,
            run_count: 0,
            max_retries: 3,
            retry_count: 0,
            output_destination: None,
            creator_agent_id: Some(AgentID::new()),
            action: OnceJobAction::RunTask {
                prompt: "post update".into(),
            },
            delivery: agentos_types::delivery::DeliveryMode::Silent,
        }
    }

    #[tokio::test]
    async fn empty_load_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let p = SchedulePersistence::new(tmp.path()).unwrap();
        let snap = p.load().await.unwrap();
        assert!(snap.jobs.is_empty());
        assert!(snap.creators.is_empty());
    }

    #[tokio::test]
    async fn round_trip_through_disk() {
        let tmp = TempDir::new().unwrap();
        let p = SchedulePersistence::new(tmp.path()).unwrap();
        let mut snap = ScheduleSnapshot::default();
        let job = sample_job();
        let creator = job.creator_agent_id.unwrap();
        snap.jobs.insert(job.id, job.clone());
        snap.creators.insert(job.id, creator);
        p.flush(snap.clone()).await.unwrap();

        let loaded = p.load().await.unwrap();
        assert_eq!(loaded.jobs.len(), 1);
        assert_eq!(loaded.jobs.get(&job.id).unwrap().name, "daily");
        assert_eq!(loaded.creators.get(&job.id), Some(&creator));
    }

    #[tokio::test]
    async fn atomic_write_does_not_leave_tmp() {
        let tmp = TempDir::new().unwrap();
        let p = SchedulePersistence::new(tmp.path()).unwrap();
        p.flush(ScheduleSnapshot::default()).await.unwrap();
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(entries.contains(&"schedules.json".to_string()));
        assert!(!entries.iter().any(|n| n.ends_with(".tmp")));
    }
}
