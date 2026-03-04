use agentos_types::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use tokio::sync::{Mutex, RwLock};

pub struct TaskScheduler {
    /// Priority queue — higher priority tasks are dequeued first.
    queue: Mutex<BinaryHeap<PrioritizedTask>>,
    /// All tasks by ID (active + completed).
    tasks: RwLock<HashMap<TaskID, AgentTask>>,
    #[allow(dead_code)]
    max_concurrent: usize,
}

#[derive(Eq, PartialEq)]
struct PrioritizedTask {
    priority: u8,
    created_at: chrono::DateTime<chrono::Utc>,
    task_id: TaskID,
}

// Higher priority first; if equal, older tasks first (FIFO within same priority)
impl Ord for PrioritizedTask {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.created_at.cmp(&self.created_at)) // older first
    }
}

impl PartialOrd for PrioritizedTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TaskScheduler {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            tasks: RwLock::new(HashMap::new()),
            max_concurrent,
        }
    }

    /// Enqueue a new task. Returns the TaskID.
    pub async fn enqueue(&self, task: AgentTask) -> TaskID {
        let task_id = task.id;
        let prioritized = PrioritizedTask {
            priority: task.priority,
            created_at: task.created_at,
            task_id,
        };
        self.tasks.write().await.insert(task_id, task);
        self.queue.lock().await.push(prioritized);
        task_id
    }

    /// Dequeue the highest-priority task that is in Queued state.
    pub async fn dequeue(&self) -> Option<AgentTask> {
        let mut queue = self.queue.lock().await;
        while let Some(prioritized) = queue.pop() {
            let tasks = self.tasks.read().await;
            if let Some(task) = tasks.get(&prioritized.task_id) {
                if task.state == TaskState::Queued {
                    return Some(task.clone());
                }
            }
        }
        None
    }

    /// Update a task's state.
    pub async fn update_state(
        &self,
        task_id: &TaskID,
        state: TaskState,
    ) -> Result<(), AgentOSError> {
        let mut tasks = self.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(task) => {
                task.state = state;
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }

    /// Get a task by ID.
    pub async fn get_task(&self, task_id: &TaskID) -> Option<AgentTask> {
        self.tasks.read().await.get(task_id).cloned()
    }

    /// List all tasks (for the CLI `task list` command).
    pub async fn list_tasks(&self) -> Vec<TaskSummary> {
        self.tasks
            .read()
            .await
            .values()
            .map(|t| TaskSummary {
                id: t.id,
                state: t.state,
                agent_id: t.agent_id,
                prompt_preview: t.original_prompt.chars().take(100).collect(),
                created_at: t.created_at,
                tool_calls: 0,
                tokens_used: 0,
            })
            .collect()
    }

    /// Get currently running task count.
    pub async fn running_count(&self) -> usize {
        self.tasks
            .read()
            .await
            .values()
            .filter(|t| t.state == TaskState::Running)
            .count()
    }

    /// Check for timed-out tasks and mark them as Failed.
    pub async fn check_timeouts(&self) -> Vec<TaskID> {
        let mut timed_out = Vec::new();
        let mut tasks = self.tasks.write().await;
        let now = chrono::Utc::now();
        for task in tasks.values_mut() {
            if task.state == TaskState::Running {
                let elapsed = now
                    .signed_duration_since(task.created_at)
                    .to_std()
                    .unwrap_or_default();
                if elapsed > task.timeout {
                    task.state = TaskState::Failed;
                    timed_out.push(task.id);
                }
            }
        }
        timed_out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn make_task(priority: u8, prompt: &str) -> AgentTask {
        AgentTask {
            id: TaskID::new(),
            state: TaskState::Queued,
            agent_id: AgentID::new(),
            capability_token: CapabilityToken {
                task_id: TaskID::new(),
                agent_id: AgentID::new(),
                allowed_tools: BTreeSet::new(),
                allowed_intents: BTreeSet::new(),
                permissions: PermissionSet::new(),
                issued_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now(),
                signature: Vec::new(),
            },
            assigned_llm: None,
            priority,
            created_at: chrono::Utc::now(),
            timeout: Duration::from_secs(300),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: None,
        }
    }

    #[tokio::test]
    async fn test_task_scheduler_priority_ordering() {
        let scheduler = TaskScheduler::new(10);

        let low_task = make_task(1, "low priority task");
        let high_task = make_task(10, "high priority task");

        scheduler.enqueue(low_task).await;
        scheduler.enqueue(high_task).await;

        // High priority should dequeue first
        let first = scheduler.dequeue().await.unwrap();
        assert_eq!(first.priority, 10);

        let second = scheduler.dequeue().await.unwrap();
        assert_eq!(second.priority, 1);
    }
}
