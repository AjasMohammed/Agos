use agentos_types::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use tokio::sync::{Mutex, RwLock};

pub struct TaskScheduler {
    /// Priority queue — higher priority tasks are dequeued first.
    queue: Mutex<BinaryHeap<PrioritizedTask>>,
    /// All tasks by ID (active + completed).
    tasks: RwLock<HashMap<TaskID, AgentTask>>,
    /// Dependency graph for deadlock prevention.
    dependency_graph: RwLock<TaskDependencyGraph>,
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
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            tasks: RwLock::new(HashMap::new()),
            dependency_graph: RwLock::new(TaskDependencyGraph::new()),
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
                    timed_out.push(task.id);
                }
            }
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
            reasoning_hints: None,
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
}
