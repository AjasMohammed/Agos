use crate::state_store::KernelStateStore;
use agentos_types::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone)]
pub struct TimedOutTask {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub timeout_seconds: u64,
    pub elapsed_seconds: u64,
}

pub struct TaskScheduler {
    /// Priority queue — higher priority tasks are dequeued first.
    queue: Mutex<BinaryHeap<PrioritizedTask>>,
    /// All tasks by ID (active + completed).
    tasks: RwLock<HashMap<TaskID, AgentTask>>,
    /// Dependency graph for deadlock prevention.
    dependency_graph: RwLock<TaskDependencyGraph>,
    /// Optional persistence backend for crash-safe task state restoration.
    state_store: Option<Arc<KernelStateStore>>,
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

/// Directed graph tracking task delegation dependencies.
/// Edge (A, B) means "task A is waiting on task B to complete".
struct TaskDependencyGraph {
    /// edges: (waiting_task, depended_on_task)
    edges: Vec<(TaskID, TaskID)>,
}

impl TaskDependencyGraph {
    fn new() -> Self {
        Self { edges: Vec::new() }
    }

    /// Returns true if adding an edge from `from` → `to` would create a cycle.
    /// Uses DFS from `to` — if we can reach `from`, adding the edge creates a cycle.
    fn would_create_cycle(&self, from: TaskID, to: TaskID) -> bool {
        if from == to {
            return true;
        }
        let mut visited = HashSet::new();
        let mut stack = vec![to];
        while let Some(node) = stack.pop() {
            if node == from {
                return true;
            }
            if visited.insert(node) {
                for &(waiter, dep) in &self.edges {
                    if waiter == node {
                        stack.push(dep);
                    }
                }
            }
        }
        false
    }

    fn add_edge(&mut self, from: TaskID, to: TaskID) {
        self.edges.push((from, to));
    }

    fn remove_edges_for(&mut self, task_id: TaskID) {
        self.edges
            .retain(|&(from, to)| from != task_id && to != task_id);
    }

    fn dependents_of(&self, task_id: TaskID) -> Vec<TaskID> {
        self.edges
            .iter()
            .filter(|&&(_, dep)| dep == task_id)
            .map(|&(waiter, _)| waiter)
            .collect()
    }
}

impl TaskScheduler {
    pub fn new(_max_concurrent: usize) -> Self {
        Self::with_state_store(_max_concurrent, None)
    }

    pub fn with_state_store(
        _max_concurrent: usize,
        state_store: Option<Arc<KernelStateStore>>,
    ) -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            tasks: RwLock::new(HashMap::new()),
            dependency_graph: RwLock::new(TaskDependencyGraph::new()),
            state_store,
        }
    }

    async fn persist_task_snapshot(&self, task: AgentTask) {
        let task_id = task.id;
        if let Some(store) = &self.state_store {
            if let Err(e) = store.upsert_scheduler_task(task).await {
                tracing::error!(
                    task_id = %task_id,
                    error = %e,
                    "Failed to persist scheduler task state"
                );
            }
        }
    }

    /// Restore non-terminal task state from SQLite at boot.
    ///
    /// Behavior:
    /// - `Queued` tasks are re-queued.
    /// - `Running` tasks are normalized to `Queued` and re-queued.
    /// - `Waiting` tasks are restored in the task map but remain paused.
    pub async fn restore_from_store(&self) -> anyhow::Result<usize> {
        let Some(store) = &self.state_store else {
            return Ok(0);
        };

        let persisted = store.load_non_terminal_scheduler_tasks().await?;
        if persisted.is_empty() {
            return Ok(0);
        }

        let mut restored_count = 0usize;
        let mut normalized_to_queued = Vec::new();

        let mut tasks = self.tasks.write().await;
        let mut queue = self.queue.lock().await;

        for mut task in persisted {
            if matches!(
                task.state,
                TaskState::Complete | TaskState::Failed | TaskState::Cancelled
            ) {
                continue;
            }

            if task.state == TaskState::Running {
                task.state = TaskState::Queued;
                task.started_at = None;
                normalized_to_queued.push(task.clone());
            }

            if task.state == TaskState::Queued {
                queue.push(PrioritizedTask {
                    priority: task.priority,
                    created_at: task.created_at,
                    task_id: task.id,
                });
            }

            tasks.insert(task.id, task);
            restored_count = restored_count.saturating_add(1);
        }

        drop(queue);
        drop(tasks);

        // Persist normalized state transitions (running -> queued) after lock release.
        for task in normalized_to_queued {
            self.persist_task_snapshot(task).await;
        }

        Ok(restored_count)
    }

    /// Return a snapshot of all tasks for use in `task-status` / `task-list` tools.
    pub async fn snapshot_tasks(&self) -> TaskSnapshot {
        let tasks = self.tasks.read().await;
        let summaries: Vec<TaskIntrospectionSummary> = tasks
            .values()
            .map(|t| TaskIntrospectionSummary {
                id: t.id,
                agent_id: t.agent_id,
                description: {
                    let boundary = t.original_prompt.char_indices().nth(100).map(|(i, _)| i);
                    match boundary {
                        Some(b) => format!("{}...", &t.original_prompt[..b]),
                        None => t.original_prompt.clone(),
                    }
                },
                status: format!("{:?}", t.state).to_lowercase(),
                created_at: t.created_at,
                started_at: t.started_at,
            })
            .collect();
        TaskSnapshot::new(summaries)
    }

    /// Enqueue a new task. Returns the TaskID.
    pub async fn enqueue(&self, task: AgentTask) -> TaskID {
        let task_id = task.id;
        let task_snapshot = task.clone();
        let prioritized = PrioritizedTask {
            priority: task.priority,
            created_at: task.created_at,
            task_id,
        };
        self.tasks.write().await.insert(task_id, task);
        self.queue.lock().await.push(prioritized);
        self.persist_task_snapshot(task_snapshot).await;
        task_id
    }

    /// Register a task in scheduler state without placing it on the run queue.
    /// Used by synchronous execution paths that run outside the background loop.
    pub async fn register_external(&self, task: AgentTask) -> TaskID {
        let task_id = task.id;
        let task_snapshot = task.clone();
        self.tasks.write().await.insert(task_id, task);
        self.persist_task_snapshot(task_snapshot).await;
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

    /// Requeue an existing task by ID and mark it as Queued.
    /// No-ops silently if the task is already in a terminal state (Complete, Failed, Cancelled).
    pub async fn requeue(&self, task_id: &TaskID) -> Result<(), AgentOSError> {
        let mut tasks = self.tasks.write().await;
        let task = match tasks.get_mut(task_id) {
            Some(task) => task,
            None => return Err(AgentOSError::TaskNotFound(*task_id)),
        };
        if matches!(
            task.state,
            TaskState::Complete | TaskState::Failed | TaskState::Cancelled
        ) {
            return Ok(());
        }
        task.state = TaskState::Queued;
        let prioritized = PrioritizedTask {
            priority: task.priority,
            created_at: task.created_at,
            task_id: *task_id,
        };
        let snapshot = task.clone();
        drop(tasks);
        self.queue.lock().await.push(prioritized);
        self.persist_task_snapshot(snapshot).await;
        Ok(())
    }

    /// Update a task's state.
    pub async fn update_state(
        &self,
        task_id: &TaskID,
        state: TaskState,
    ) -> Result<(), AgentOSError> {
        let snapshot = {
            let mut tasks = self.tasks.write().await;
            match tasks.get_mut(task_id) {
                Some(task) => {
                    task.state = state;
                    task.clone()
                }
                None => return Err(AgentOSError::TaskNotFound(*task_id)),
            }
        };
        self.persist_task_snapshot(snapshot).await;
        Ok(())
    }

    /// Update a task state only if the current state is not terminal.
    /// Returns Ok(true) when updated, Ok(false) when no-op due to terminal state.
    pub async fn update_state_if_not_terminal(
        &self,
        task_id: &TaskID,
        state: TaskState,
    ) -> Result<bool, AgentOSError> {
        let snapshot = {
            let mut tasks = self.tasks.write().await;
            match tasks.get_mut(task_id) {
                Some(task) => {
                    if matches!(
                        task.state,
                        TaskState::Complete | TaskState::Failed | TaskState::Cancelled
                    ) {
                        return Ok(false);
                    }
                    task.state = state;
                    task.clone()
                }
                None => return Err(AgentOSError::TaskNotFound(*task_id)),
            }
        };
        self.persist_task_snapshot(snapshot).await;
        Ok(true)
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
                priority: t.priority,
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

    /// Set `started_at` timestamp on a task (when it transitions to Running).
    pub async fn mark_started(&self, task_id: &TaskID) -> Result<(), AgentOSError> {
        let snapshot = {
            let mut tasks = self.tasks.write().await;
            match tasks.get_mut(task_id) {
                Some(task) => {
                    task.started_at = Some(chrono::Utc::now());
                    task.clone()
                }
                None => return Err(AgentOSError::TaskNotFound(*task_id)),
            }
        };
        self.persist_task_snapshot(snapshot).await;
        Ok(())
    }

    /// Check for timed-out tasks and mark them as Failed.
    pub async fn check_timeouts(&self) -> Vec<TimedOutTask> {
        let mut timed_out = Vec::new();
        let mut changed_tasks = Vec::new();
        let mut tasks = self.tasks.write().await;
        let now = chrono::Utc::now();
        for task in tasks.values_mut() {
            if task.state == TaskState::Running {
                let baseline = task.started_at.unwrap_or(task.created_at);
                let elapsed = now
                    .signed_duration_since(baseline)
                    .to_std()
                    .unwrap_or_default();
                // Apply timeout multiplier based on preemption sensitivity
                let effective_timeout = match task
                    .reasoning_hints
                    .as_ref()
                    .map(|h| h.preemption_sensitivity)
                {
                    Some(PreemptionLevel::High) => task.timeout * 3,
                    Some(PreemptionLevel::Normal) => task.timeout * 2,
                    _ => task.timeout,
                };

                if elapsed > effective_timeout {
                    task.state = TaskState::Failed;
                    changed_tasks.push(task.clone());
                    timed_out.push(TimedOutTask {
                        task_id: task.id,
                        agent_id: task.agent_id,
                        timeout_seconds: effective_timeout.as_secs(),
                        elapsed_seconds: elapsed.as_secs(),
                    });
                }
            }
        }
        drop(tasks);

        for task in changed_tasks {
            self.persist_task_snapshot(task).await;
        }

        timed_out
    }

    // --- Dependency Graph Methods ---

    /// Check if adding a dependency (parent waits on child) would create a cycle.
    /// Returns Ok(()) if safe, Err with reason if it would deadlock.
    pub async fn check_delegation_safe(
        &self,
        parent_task_id: TaskID,
        child_task_id: TaskID,
    ) -> Result<(), String> {
        let graph = self.dependency_graph.read().await;
        if graph.would_create_cycle(parent_task_id, child_task_id) {
            Err(format!(
                "DeadlockPrevented: circular dependency between task {} and task {}",
                parent_task_id, child_task_id
            ))
        } else {
            Ok(())
        }
    }

    /// Register a delegation dependency: parent waits on child.
    pub async fn add_dependency(&self, parent_task_id: TaskID, child_task_id: TaskID) {
        self.dependency_graph
            .write()
            .await
            .add_edge(parent_task_id, child_task_id);
    }

    /// Called when a task completes — removes all edges and wakes waiting parents.
    /// Returns the list of parent tasks that were waiting on this task.
    pub async fn complete_dependency(&self, completed_task_id: TaskID) -> Vec<TaskID> {
        let mut graph = self.dependency_graph.write().await;
        let waiters = graph.dependents_of(completed_task_id);
        graph.remove_edges_for(completed_task_id);
        waiters
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;

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
            started_at: None,
            timeout: Duration::from_secs(300),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            max_iterations: None,
            trigger_source: None,
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

    #[tokio::test]
    async fn test_cycle_detection_simple() {
        let graph = TaskDependencyGraph::new();
        let a = TaskID::new();
        // Self-loop
        assert!(graph.would_create_cycle(a, a));
    }

    #[tokio::test]
    async fn test_cycle_detection_chain() {
        let mut graph = TaskDependencyGraph::new();
        let a = TaskID::new();
        let b = TaskID::new();
        let c = TaskID::new();

        // A waits on B, B waits on C
        graph.add_edge(a, b);
        graph.add_edge(b, c);

        // Adding C waits on A would create cycle
        assert!(graph.would_create_cycle(c, a));
        // Adding C waits on D would not create cycle
        let d = TaskID::new();
        assert!(!graph.would_create_cycle(c, d));
    }

    #[tokio::test]
    async fn test_delegation_safe_check() {
        let scheduler = TaskScheduler::new(10);
        let parent = make_task(5, "parent");
        let child = make_task(5, "child");
        let parent_id = parent.id;
        let child_id = child.id;

        scheduler.enqueue(parent).await;
        scheduler.enqueue(child).await;

        // First delegation is safe
        assert!(scheduler
            .check_delegation_safe(parent_id, child_id)
            .await
            .is_ok());
        scheduler.add_dependency(parent_id, child_id).await;

        // Reverse delegation would deadlock
        assert!(scheduler
            .check_delegation_safe(child_id, parent_id)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_complete_dependency_wakes_parents() {
        let scheduler = TaskScheduler::new(10);
        let parent = make_task(5, "parent");
        let child = make_task(5, "child");
        let parent_id = parent.id;
        let child_id = child.id;

        scheduler.enqueue(parent).await;
        scheduler.enqueue(child).await;
        scheduler.add_dependency(parent_id, child_id).await;

        let waiters = scheduler.complete_dependency(child_id).await;
        assert_eq!(waiters.len(), 1);
        assert_eq!(waiters[0], parent_id);
    }

    #[tokio::test]
    async fn test_check_timeouts_returns_task_metadata() {
        let scheduler = TaskScheduler::new(10);
        let mut task = make_task(5, "times out");
        task.state = TaskState::Running;
        task.timeout = Duration::from_secs(1);
        task.created_at = chrono::Utc::now() - chrono::Duration::seconds(5);
        let task_id = task.id;
        let agent_id = task.agent_id;

        scheduler.enqueue(task).await;
        let timed_out = scheduler.check_timeouts().await;

        assert_eq!(timed_out.len(), 1);
        let record = &timed_out[0];
        assert_eq!(record.task_id, task_id);
        assert_eq!(record.agent_id, agent_id);
        assert_eq!(record.timeout_seconds, 1);
        assert!(record.elapsed_seconds >= 5);
    }

    #[tokio::test]
    async fn test_requeue_marks_task_queued_and_enqueues() {
        let scheduler = TaskScheduler::new(10);
        let mut task = make_task(5, "requeue me");
        task.state = TaskState::Waiting;
        let task_id = task.id;

        scheduler.enqueue(task).await;
        scheduler.requeue(&task_id).await.unwrap();

        let popped = scheduler.dequeue().await.expect("task should be queued");
        assert_eq!(popped.id, task_id);
        assert_eq!(popped.state, TaskState::Queued);
    }

    #[tokio::test]
    async fn test_update_state_if_not_terminal_noops_for_complete() {
        let scheduler = TaskScheduler::new(10);
        let mut task = make_task(5, "done");
        task.state = TaskState::Complete;
        let task_id = task.id;

        scheduler.enqueue(task).await;
        let updated = scheduler
            .update_state_if_not_terminal(&task_id, TaskState::Failed)
            .await
            .unwrap();
        assert!(!updated);

        let current = scheduler.get_task(&task_id).await.unwrap();
        assert_eq!(current.state, TaskState::Complete);
    }

    #[tokio::test]
    async fn test_requeue_noops_for_terminal_states() {
        let scheduler = TaskScheduler::new(10);

        // Complete task should not be requeued
        let mut task = make_task(5, "completed task");
        task.state = TaskState::Complete;
        let task_id = task.id;
        scheduler.enqueue(task).await;
        scheduler.requeue(&task_id).await.unwrap();
        let current = scheduler.get_task(&task_id).await.unwrap();
        assert_eq!(current.state, TaskState::Complete);

        // Failed task should not be requeued
        let mut task2 = make_task(5, "failed task");
        task2.state = TaskState::Failed;
        let task2_id = task2.id;
        scheduler.enqueue(task2).await;
        scheduler.requeue(&task2_id).await.unwrap();
        let current2 = scheduler.get_task(&task2_id).await.unwrap();
        assert_eq!(current2.state, TaskState::Failed);

        // Cancelled task should not be requeued
        let mut task3 = make_task(5, "cancelled task");
        task3.state = TaskState::Cancelled;
        let task3_id = task3.id;
        scheduler.enqueue(task3).await;
        scheduler.requeue(&task3_id).await.unwrap();
        let current3 = scheduler.get_task(&task3_id).await.unwrap();
        assert_eq!(current3.state, TaskState::Cancelled);
    }

    #[tokio::test]
    async fn test_check_timeouts_uses_started_at() {
        let scheduler = TaskScheduler::new(10);
        let mut task = make_task(5, "started recently");
        task.state = TaskState::Running;
        task.timeout = Duration::from_secs(10);
        // created_at is 60 seconds ago — would timeout if measured from created_at
        task.created_at = chrono::Utc::now() - chrono::Duration::seconds(60);
        // started_at is 2 seconds ago — should NOT timeout since 2 < 10
        task.started_at = Some(chrono::Utc::now() - chrono::Duration::seconds(2));
        let task_id = task.id;

        scheduler.enqueue(task).await;
        let timed_out = scheduler.check_timeouts().await;

        assert!(
            timed_out.is_empty(),
            "Task should NOT time out when started_at is recent"
        );

        // Verify task is still Running
        let current = scheduler.get_task(&task_id).await.unwrap();
        assert_eq!(current.state, TaskState::Running);
    }

    #[tokio::test]
    async fn test_check_timeouts_falls_back_to_created_at() {
        let scheduler = TaskScheduler::new(10);
        let mut task = make_task(5, "no started_at");
        task.state = TaskState::Running;
        task.timeout = Duration::from_secs(1);
        task.created_at = chrono::Utc::now() - chrono::Duration::seconds(5);
        task.started_at = None; // no started_at — should use created_at

        scheduler.enqueue(task).await;
        let timed_out = scheduler.check_timeouts().await;

        assert_eq!(
            timed_out.len(),
            1,
            "Task should time out using created_at fallback"
        );
    }

    #[tokio::test]
    async fn test_mark_started_sets_timestamp() {
        let scheduler = TaskScheduler::new(10);
        let task = make_task(5, "to be started");
        let task_id = task.id;
        scheduler.enqueue(task).await;

        let before = scheduler.get_task(&task_id).await.unwrap();
        assert!(before.started_at.is_none());

        scheduler.mark_started(&task_id).await.unwrap();

        let after = scheduler.get_task(&task_id).await.unwrap();
        assert!(after.started_at.is_some());
    }

    #[tokio::test]
    async fn test_restore_from_store_recovers_non_terminal_tasks() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("kernel_state.db");
        let store = Arc::new(
            KernelStateStore::open(db_path)
                .await
                .expect("state store should open"),
        );

        // Seed persisted state.
        let scheduler = TaskScheduler::with_state_store(10, Some(store.clone()));
        let queued_task = make_task(7, "queued");
        let queued_id = queued_task.id;
        scheduler.enqueue(queued_task).await;

        let running_task = make_task(6, "running");
        let running_id = running_task.id;
        scheduler.enqueue(running_task).await;
        scheduler
            .update_state(&running_id, TaskState::Running)
            .await
            .unwrap();
        scheduler.mark_started(&running_id).await.unwrap();

        let waiting_task = make_task(5, "waiting");
        let waiting_id = waiting_task.id;
        scheduler.enqueue(waiting_task).await;
        scheduler
            .update_state(&waiting_id, TaskState::Waiting)
            .await
            .unwrap();

        // Simulate restart by creating a fresh scheduler on the same DB.
        let restored = TaskScheduler::with_state_store(10, Some(store));
        let restored_count = restored
            .restore_from_store()
            .await
            .expect("restore should succeed");
        assert_eq!(restored_count, 3);

        let restored_running = restored.get_task(&running_id).await.unwrap();
        assert_eq!(
            restored_running.state,
            TaskState::Queued,
            "running task should be normalized to queued on restore"
        );

        let restored_waiting = restored.get_task(&waiting_id).await.unwrap();
        assert_eq!(
            restored_waiting.state,
            TaskState::Waiting,
            "waiting task should remain paused after restore"
        );

        // Waiting task should not be dequeued for execution.
        let first = restored.dequeue().await.expect("first task");
        let second = restored.dequeue().await.expect("second task");
        let dequeued = [first.id, second.id];
        assert!(dequeued.contains(&queued_id));
        assert!(dequeued.contains(&running_id));
        assert_ne!(first.id, waiting_id);
        assert_ne!(second.id, waiting_id);
    }
}
