use agentos_types::{AgentID, AgentOSError, TaskID};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

const LOCK_TIMEOUT_SECS: i64 = 60;

/// Metadata about the agent currently holding a write lock.
#[derive(Debug, Clone)]
pub struct FileLockEntry {
    pub holder_agent_id: AgentID,
    pub holder_task_id: TaskID,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Process-level registry of exclusive file write locks.
///
/// A write lock is fully exclusive: no other agent may read or write the
/// locked file until the lock is released. Locks auto-expire after
/// `LOCK_TIMEOUT_SECS` seconds to recover from agent crashes mid-write.
///
/// Held by `ToolRunner` and injected into every `ToolExecutionContext`.
#[derive(Debug, Default)]
pub struct FileLockRegistry {
    locks: Mutex<HashMap<PathBuf, FileLockEntry>>,
}

impl FileLockRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn sweep(locks: &mut HashMap<PathBuf, FileLockEntry>) {
        let now = Utc::now();
        locks.retain(|_, e| e.expires_at > now);
    }

    /// Try to acquire an exclusive write lock on `path`.
    ///
    /// Returns `Ok(())` on success. Returns `Err(FileLocked)` if the path is
    /// already held by another agent (or by a non-expired lock from any agent).
    pub fn try_acquire(
        &self,
        path: &PathBuf,
        agent_id: AgentID,
        task_id: TaskID,
    ) -> Result<(), AgentOSError> {
        let mut locks = self.locks.lock().unwrap();
        Self::sweep(&mut locks);
        if let Some(e) = locks.get(path) {
            return Err(AgentOSError::FileLocked {
                path: path.display().to_string(),
                holder_agent_id: e.holder_agent_id,
                holder_task_id: e.holder_task_id,
                acquired_at: e.acquired_at,
            });
        }
        let now = Utc::now();
        locks.insert(
            path.clone(),
            FileLockEntry {
                holder_agent_id: agent_id,
                holder_task_id: task_id,
                acquired_at: now,
                expires_at: now + Duration::seconds(LOCK_TIMEOUT_SECS),
            },
        );
        Ok(())
    }

    /// Release the lock for `path` if it is held by `agent_id`.
    /// No-op if the lock is not held or held by a different agent.
    pub fn release(&self, path: &PathBuf, agent_id: AgentID) {
        let mut locks = self.locks.lock().unwrap();
        if locks.get(path).map(|e| e.holder_agent_id) == Some(agent_id) {
            locks.remove(path);
        }
    }

    /// Check whether `path` is currently locked.
    ///
    /// Returns `Ok(())` if the path is free. Returns `Err(FileLocked)` if a
    /// valid (non-expired) lock is held — readers call this before proceeding.
    pub fn check(&self, path: &PathBuf) -> Result<(), AgentOSError> {
        let mut locks = self.locks.lock().unwrap();
        Self::sweep(&mut locks);
        if let Some(e) = locks.get(path) {
            return Err(AgentOSError::FileLocked {
                path: path.display().to_string(),
                holder_agent_id: e.holder_agent_id,
                holder_task_id: e.holder_task_id,
                acquired_at: e.acquired_at,
            });
        }
        Ok(())
    }
}

/// RAII write-lock guard.
///
/// Acquires the lock on construction and releases it on drop — even if the
/// write operation fails or panics.
pub struct WriteLockGuard<'a> {
    registry: &'a FileLockRegistry,
    path: PathBuf,
    agent_id: AgentID,
}

impl<'a> WriteLockGuard<'a> {
    /// Acquire the lock. Returns `Err(FileLocked)` if already held.
    pub fn acquire(
        registry: &'a FileLockRegistry,
        path: PathBuf,
        agent_id: AgentID,
        task_id: TaskID,
    ) -> Result<Self, AgentOSError> {
        registry.try_acquire(&path, agent_id, task_id)?;
        Ok(Self {
            registry,
            path,
            agent_id,
        })
    }
}

impl<'a> Drop for WriteLockGuard<'a> {
    fn drop(&mut self) {
        self.registry.release(&self.path, self.agent_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_and_release() {
        let reg = FileLockRegistry::new();
        let path = PathBuf::from("/data/file.txt");
        let a = AgentID::new();
        let t = TaskID::new();

        assert!(reg.try_acquire(&path, a, t).is_ok());
        assert!(reg.check(&path).is_err()); // locked
        reg.release(&path, a);
        assert!(reg.check(&path).is_ok()); // free again
    }

    #[test]
    fn test_second_acquire_fails() {
        let reg = FileLockRegistry::new();
        let path = PathBuf::from("/data/file.txt");
        let a1 = AgentID::new();
        let a2 = AgentID::new();

        reg.try_acquire(&path, a1, TaskID::new()).unwrap();
        let err = reg.try_acquire(&path, a2, TaskID::new()).unwrap_err();
        assert!(matches!(err, AgentOSError::FileLocked { .. }));
    }

    #[test]
    fn test_raii_guard_releases_on_drop() {
        let reg = FileLockRegistry::new();
        let path = PathBuf::from("/data/file.txt");
        let a = AgentID::new();

        {
            let _guard = WriteLockGuard::acquire(&reg, path.clone(), a, TaskID::new()).unwrap();
            assert!(reg.check(&path).is_err());
        } // guard drops here
        assert!(reg.check(&path).is_ok());
    }

    #[test]
    fn test_release_by_wrong_agent_is_noop() {
        let reg = FileLockRegistry::new();
        let path = PathBuf::from("/data/file.txt");
        let a1 = AgentID::new();
        let a2 = AgentID::new();

        reg.try_acquire(&path, a1, TaskID::new()).unwrap();
        reg.release(&path, a2); // wrong agent — no-op
        assert!(reg.check(&path).is_err()); // still locked
    }
}
