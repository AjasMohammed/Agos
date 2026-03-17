use agentos_types::*;
use std::collections::HashMap;
use tokio::sync::RwLock;

pub struct BackgroundPool {
    tasks: RwLock<HashMap<TaskID, BackgroundTask>>,
}

impl BackgroundPool {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(&self, task: BackgroundTask) {
        self.tasks.write().await.insert(task.id, task);
    }

    pub async fn complete(&self, task_id: &TaskID, result: serde_json::Value) {
        if let Some(task) = self.tasks.write().await.get_mut(task_id) {
            task.state = TaskState::Complete;
            task.completed_at = Some(chrono::Utc::now());
            task.result = Some(result);
        }
    }

    pub async fn fail(&self, task_id: &TaskID, error: String) {
        if let Some(task) = self.tasks.write().await.get_mut(task_id) {
            task.state = TaskState::Failed;
            task.completed_at = Some(chrono::Utc::now());
            task.result = Some(serde_json::json!({ "error": error }));
        }
    }

    pub async fn set_waiting(&self, task_id: &TaskID, reason: String) {
        if let Some(task) = self.tasks.write().await.get_mut(task_id) {
            task.state = TaskState::Waiting;
            task.result = Some(serde_json::json!({ "status": "paused", "reason": reason }));
        }
    }

    pub async fn list_all(&self) -> Vec<BackgroundTask> {
        self.tasks.read().await.values().cloned().collect()
    }

    pub async fn list_running(&self) -> Vec<BackgroundTask> {
        self.tasks
            .read()
            .await
            .values()
            .filter(|t| t.state == TaskState::Running)
            .cloned()
            .collect()
    }

    pub async fn get_task(&self, task_id: &TaskID) -> Option<BackgroundTask> {
        self.tasks.read().await.get(task_id).cloned()
    }

    pub async fn set_scheduled_job(&self, task_id: &TaskID, job_id: ScheduleID) {
        if let Some(task) = self.tasks.write().await.get_mut(task_id) {
            task.scheduled_job_id = Some(job_id);
        }
    }

    pub async fn get_by_name(&self, name: &str) -> Option<BackgroundTask> {
        self.tasks
            .read()
            .await
            .values()
            .find(|t| t.name == name)
            .cloned()
    }

    /// Remove terminal (Complete / Failed) tasks that completed more than
    /// `max_age_secs` seconds ago. Called periodically by the timeout checker
    /// to prevent unbounded memory growth in long-running kernels.
    pub async fn evict_terminal(&self, max_age_secs: i64) {
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(max_age_secs);
        self.tasks.write().await.retain(|_, task| {
            let is_terminal = matches!(task.state, TaskState::Complete | TaskState::Failed);
            if is_terminal {
                if let Some(completed_at) = task.completed_at {
                    return completed_at > cutoff;
                }
            }
            true
        });
    }
}

impl Default for BackgroundPool {
    fn default() -> Self {
        Self::new()
    }
}
