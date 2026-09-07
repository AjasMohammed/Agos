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
    /// Depth for the TaskTimedOut/TaskFailed events emitted for this task, so
    /// event-triggered tasks that time out don't reset the trigger-loop counter.
    pub chain_depth: u32,
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
    /// Maps parent task IDs to their spawned child task IDs (for cascade-cancel).
    child_map: RwLock<HashMap<TaskID, Vec<TaskID>>>,
    /// Failure reason per failed task (first line of the error chain).
    /// Persisted alongside the task row so it survives restarts.
    failure_reasons: RwLock<HashMap<TaskID, String>>,
    /// Maximum queued (not running) tasks per agent; 0 disables the cap.
    /// `max_concurrent_tasks` bounds execution, not queue depth — see
    /// `enqueue` for why the rejection path emits no events.
    max_queued_per_agent: usize,
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

/// Fallback queue-depth cap when the scheduler is built without an explicit
/// one (tests, `TaskScheduler::new`). Mirrors `default_max_queued_per_agent`
/// in `config.rs`.
pub const DEFAULT_MAX_QUEUED_PER_AGENT: usize = 500;

impl TaskScheduler {
    pub fn new(_max_concurrent: usize) -> Self {
        Self::with_state_store(_max_concurrent, None)
    }

    pub fn with_state_store(
        _max_concurrent: usize,
        state_store: Option<Arc<KernelStateStore>>,
    ) -> Self {
        Self::with_limits(_max_concurrent, state_store, DEFAULT_MAX_QUEUED_PER_AGENT)
    }

    pub fn with_limits(
        _max_concurrent: usize,
        state_store: Option<Arc<KernelStateStore>>,
        max_queued_per_agent: usize,
    ) -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            tasks: RwLock::new(HashMap::new()),
            dependency_graph: RwLock::new(TaskDependencyGraph::new()),
            state_store,
            child_map: RwLock::new(HashMap::new()),
            failure_reasons: RwLock::new(HashMap::new()),
            max_queued_per_agent,
        }
    }

    /// Record why a task failed. Kept in memory for `list_tasks` and written
    /// to the state store so the reason is still available after a restart.
    pub async fn set_failure_reason(&self, task_id: &TaskID, reason: String) {
        self.failure_reasons
            .write()
            .await
            .insert(*task_id, reason.clone());
        if let Some(store) = &self.state_store {
            if let Err(e) = store.set_scheduler_task_error(task_id, &reason).await {
                tracing::warn!(task_id = %task_id, error = %e, "Failed to persist task failure reason");
            }
        }
    }

    /// Persist the final answer of a task that just completed (write-through;
    /// nothing in memory reads it — the detail view fetches via `outcome`).
    pub async fn set_result(&self, task_id: &TaskID, answer: &str) {
        if let Some(store) = &self.state_store {
            if let Err(e) = store.set_scheduler_task_result(task_id, answer).await {
                tracing::warn!(task_id = %task_id, error = %e, "Failed to persist task result");
            }
        }
    }

    /// `(completed_at, final answer)` for a terminal task; `None` while it is
    /// still queued/running or when no state store is configured.
    pub async fn outcome(
        &self,
        task_id: &TaskID,
    ) -> Option<(chrono::DateTime<chrono::Utc>, Option<String>)> {
        let terminal = self.tasks.read().await.get(task_id).is_some_and(|t| {
            matches!(
                t.state,
                TaskState::Complete | TaskState::Failed | TaskState::Cancelled
            )
        });
        if !terminal {
            return None;
        }
        self.state_store
            .as_ref()?
            .scheduler_task_outcome(task_id)
            .await
            .ok()
            .flatten()
    }

    /// Failure reason for a task, only while it is actually in `Failed`. A
    /// task that failed once and later completed (retry/requeue) must not keep
    /// reporting the stale reason.
    pub async fn failure_reason(&self, task_id: &TaskID) -> Option<String> {
        let failed = self
            .tasks
            .read()
            .await
            .get(task_id)
            .is_some_and(|t| t.state == TaskState::Failed);
        if !failed {
            return None;
        }
        self.failure_reasons.read().await.get(task_id).cloned()
    }

    /// Drop failure reasons for tasks the scheduler no longer tracks (called
    /// from the periodic prune sweep) so the map can't grow unbounded.
    pub async fn prune_failure_reasons(&self) -> usize {
        let live: HashSet<TaskID> = self.tasks.read().await.keys().copied().collect();
        let mut reasons = self.failure_reasons.write().await;
        let before = reasons.len();
        reasons.retain(|id, _| live.contains(id));
        before - reasons.len()
    }

    /// Load recently finished tasks (complete/failed/cancelled) from the state
    /// store into the in-memory map so task history survives a kernel restart.
    /// They are never re-queued. Returns the number of rows loaded.
    pub async fn restore_terminal_history(&self, limit: usize) -> anyhow::Result<usize> {
        let Some(store) = &self.state_store else {
            return Ok(0);
        };
        let rows = store.load_recent_terminal_scheduler_tasks(limit).await?;
        let mut tasks = self.tasks.write().await;
        let mut reasons = self.failure_reasons.write().await;
        let mut loaded = 0usize;
        for (task, error) in rows {
            if tasks.contains_key(&task.id) {
                continue;
            }
            if let Some(err) = error {
                reasons.insert(task.id, err);
            }
            tasks.insert(task.id, task);
            loaded += 1;
        }
        Ok(loaded)
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
    ///
    /// Two guards keep a runaway backlog from resurrecting itself on every
    /// restart (2026-07-26: 190k queued tasks replayed at each boot, growing
    /// 38k → 62k → 111k → 193k):
    /// - tasks enqueued more than `max_age_hours` ago are cancelled instead of
    ///   replayed (`0` disables the cutoff),
    /// - the same per-agent depth cap that `enqueue` applies is enforced here,
    ///   so a pre-existing backlog cannot reload past it.
    ///
    /// `resumable` holds task IDs that have a saved checkpoint; they bypass
    /// both guards. A checkpointed task did real work before the crash and is
    /// resumed from its saved context by `recover_checkpointed_tasks` (A1) —
    /// and that recovery looks the task up *in the scheduler map*, so
    /// cancelling one here would silently drop the resume. Storm tasks never
    /// reach a tool call, so they never have a checkpoint.
    ///
    /// Cancelled rows are persisted back as `Cancelled` so the next boot does
    /// not see them again, and are returned in the second tuple element for
    /// logging.
    pub async fn restore_from_store(
        &self,
        max_age_hours: u32,
        resumable: &HashSet<TaskID>,
    ) -> anyhow::Result<(usize, usize)> {
        let Some(store) = &self.state_store else {
            return Ok((0, 0));
        };

        let persisted = store.load_non_terminal_scheduler_tasks().await?;
        if persisted.is_empty() {
            return Ok((0, 0));
        }

        let cutoff = if max_age_hours == 0 {
            None
        } else {
            Some(chrono::Utc::now() - chrono::Duration::hours(i64::from(max_age_hours)))
        };

        let mut restored_count = 0usize;
        let mut normalized_to_queued = Vec::new();
        let mut dropped = Vec::new();
        let mut queued_per_agent: HashMap<AgentID, usize> = HashMap::new();

        // LOCK ORDER: `queue` before `tasks` whenever both are held, matching
        // `dequeue_runnable`. Restore only runs at boot today (before the
        // executor is spawned), so the reverse order was safe — but a future
        // runtime "reload state" caller would deadlock against the executor.
        let mut queue = self.queue.lock().await;
        let mut tasks = self.tasks.write().await;

        for mut task in persisted {
            if matches!(
                task.state,
                TaskState::Complete | TaskState::Failed | TaskState::Cancelled
            ) {
                continue;
            }

            let exempt = resumable.contains(&task.id);

            if !exempt && cutoff.is_some_and(|c| task.created_at < c) {
                task.state = TaskState::Cancelled;
                dropped.push(task);
                continue;
            }

            if task.state == TaskState::Running {
                task.state = TaskState::Queued;
                task.started_at = None;
                normalized_to_queued.push(task.clone());
            }

            if task.state == TaskState::Queued {
                let seen = queued_per_agent.entry(task.agent_id).or_insert(0);
                if !exempt && self.max_queued_per_agent > 0 && *seen >= self.max_queued_per_agent {
                    task.state = TaskState::Cancelled;
                    dropped.push(task);
                    continue;
                }
                *seen += 1;
                queue.push(PrioritizedTask {
                    priority: task.priority,
                    created_at: task.created_at,
                    task_id: task.id,
                });
            }

            tasks.insert(task.id, task);
            restored_count = restored_count.saturating_add(1);
        }

        drop(tasks);
        drop(queue);

        let dropped_count = dropped.len();

        // Persist normalized state transitions (running -> queued) after lock release.
        for task in normalized_to_queued {
            self.persist_task_snapshot(task).await;
        }
        // Persist the cancellations so they are not reconsidered next boot.
        for task in dropped {
            self.persist_task_snapshot(task).await;
        }

        Ok((restored_count, dropped_count))
    }

    /// Delete persisted terminal rows older than `max_age`.
    ///
    /// Thin delegate so callers don't need their own `KernelStateStore` handle
    /// — the scheduler owns these rows. No-op without persistence.
    pub async fn prune_terminal_persisted(
        &self,
        max_age: chrono::Duration,
    ) -> anyhow::Result<usize> {
        let pruned = match &self.state_store {
            Some(store) => store.prune_terminal_scheduler_tasks(max_age).await?,
            None => 0,
        };
        self.prune_failure_reasons().await;
        Ok(pruned)
    }

    /// Number of tasks currently in `Queued` state for an agent.
    pub async fn queued_count_for_agent(&self, agent_id: &AgentID) -> usize {
        self.tasks
            .read()
            .await
            .values()
            .filter(|t| t.agent_id == *agent_id && t.state == TaskState::Queued)
            .count()
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
    ///
    /// If the agent already holds `max_queued_per_agent` queued tasks the task
    /// is **not** queued: it is recorded as `Failed` so `task status` can
    /// explain what happened, and an ERROR is logged.
    ///
    /// The rejection deliberately emits no event. Routing it through
    /// `complete_task_failure` would emit `TaskFailed` — the very event that
    /// feeds an event-trigger loop — so a cap breach would fuel the storm the
    /// cap exists to stop. The scheduler has no event-bus access, which is what
    /// makes this the safe place for the check; keeping the `-> TaskID`
    /// signature keeps all 30+ callers unchanged.
    #[tracing::instrument(skip_all, fields(task_id = %task.id, priority = task.priority))]
    pub async fn enqueue(&self, mut task: AgentTask) -> TaskID {
        let task_id = task.id;

        // A task already in the map is a *re-enqueue* (checkpoint resume,
        // requeue of a parked task), not new work — it is already counted in
        // the agent's queued total. Charging it again lets a full-cap agent
        // reject the very task it is resuming, which would flip that task to
        // Failed after `recover_checkpointed_tasks` already bumped its
        // poison-pill counter — three boots of that and the checkpoint is
        // deleted for good.
        let is_reenqueue = self.tasks.read().await.contains_key(&task_id);

        if !is_reenqueue && self.max_queued_per_agent > 0 {
            let queued = self.queued_count_for_agent(&task.agent_id).await;
            if queued >= self.max_queued_per_agent {
                tracing::error!(
                    task_id = %task_id,
                    agent_id = %task.agent_id,
                    queued,
                    cap = self.max_queued_per_agent,
                    "Task rejected — agent queue cap exceeded. Drain with \
                     `agentos task purge --agent <name>`"
                );
                task.state = TaskState::Failed;
                let snapshot = task.clone();
                self.tasks.write().await.insert(task_id, task);
                self.persist_task_snapshot(snapshot).await;
                // Persist why, or the task detail view shows a bare "failed".
                self.set_failure_reason(
                    &task_id,
                    format!(
                        "Rejected: agent already has {} queued tasks (cap {})",
                        queued, self.max_queued_per_agent
                    ),
                )
                .await;
                return task_id;
            }
        }

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

    /// Bulk-drop an agent's tasks in the given states (default: `Queued`).
    ///
    /// Recovery valve for runaway trigger loops — `cmd_cancel_task` is per-ID
    /// and cannot drain a six-figure backlog. `Running` tasks are never
    /// touched; cancel those individually so their cleanup path runs.
    ///
    /// Returns the number of tasks dropped from memory and persistence.
    pub async fn purge_agent_tasks(&self, agent_id: &AgentID, states: &[TaskState]) -> usize {
        let states: Vec<TaskState> = if states.is_empty() {
            vec![TaskState::Queued]
        } else {
            states
                .iter()
                .copied()
                .filter(|s| *s != TaskState::Running)
                .collect()
        };
        if states.is_empty() {
            return 0;
        }

        let mut tasks = self.tasks.write().await;
        let doomed: Vec<TaskID> = tasks
            .values()
            .filter(|t| t.agent_id == *agent_id && states.contains(&t.state))
            .map(|t| t.id)
            .collect();
        if doomed.is_empty() {
            return 0;
        }
        let doomed_set: HashSet<TaskID> = doomed.iter().copied().collect();
        for id in &doomed {
            tasks.remove(id);
        }
        drop(tasks);
        {
            let mut reasons = self.failure_reasons.write().await;
            for id in &doomed {
                reasons.remove(id);
            }
        }

        // Rebuild the heap without the purged entries.
        {
            let mut queue = self.queue.lock().await;
            let retained: Vec<PrioritizedTask> = std::mem::take(&mut *queue)
                .into_vec()
                .into_iter()
                .filter(|p| !doomed_set.contains(&p.task_id))
                .collect();
            *queue = BinaryHeap::from(retained);
        }

        {
            let mut children = self.child_map.write().await;
            children.retain(|parent, _| !doomed_set.contains(parent));
            for kids in children.values_mut() {
                kids.retain(|k| !doomed_set.contains(k));
            }
        }

        // Release anyone blocked on a purged task. `complete_dependency` only
        // ever fires from the task-completion paths, which a purged task never
        // reaches — without this a delegating parent sits in `Waiting` forever
        // (the parked-task reaper in `check_timeouts` would eventually fail it,
        // but only after its whole timeout budget elapsed)
        // and its graph edges leak.
        let waiters: Vec<TaskID> = {
            let mut graph = self.dependency_graph.write().await;
            let mut waiters = Vec::new();
            for id in &doomed {
                waiters.extend(graph.dependents_of(*id));
                graph.remove_edges_for(*id);
            }
            waiters.retain(|w| !doomed_set.contains(w));
            waiters.sort_unstable();
            waiters.dedup();
            waiters
        };
        for waiter in waiters {
            if let Err(e) = self.requeue(&waiter).await {
                tracing::warn!(
                    task_id = %waiter,
                    error = %e,
                    "Failed to wake a task that was waiting on a purged dependency"
                );
            }
        }

        if let Some(store) = &self.state_store {
            if let Err(e) = store.delete_scheduler_tasks(&doomed).await {
                tracing::warn!(
                    agent_id = %agent_id,
                    error = %e,
                    "Purged tasks from memory but failed to delete persisted rows"
                );
            }
        }

        doomed.len()
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
        self.dequeue_runnable(|_| true).await
    }

    /// Dequeue the highest-priority `Queued` task whose agent passes
    /// `runnable`.
    ///
    /// Tasks belonging to agents that fail the predicate are pushed back onto
    /// the heap, so pausing an agent holds its backlog instead of losing it.
    /// Without this the pause primitive is only half-implemented: marking an
    /// agent `manually_offline` correctly stops it being reactivated at boot,
    /// but its already-queued tasks kept executing.
    pub async fn dequeue_runnable(&self, runnable: impl Fn(&AgentID) -> bool) -> Option<AgentTask> {
        let mut queue = self.queue.lock().await;
        let mut skipped: Vec<PrioritizedTask> = Vec::new();
        let mut found = None;

        while let Some(prioritized) = queue.pop() {
            let tasks = self.tasks.read().await;
            let Some(task) = tasks.get(&prioritized.task_id) else {
                continue; // task was purged — drop the stale heap entry
            };
            if task.state != TaskState::Queued {
                continue;
            }
            if !runnable(&task.agent_id) {
                drop(tasks);
                skipped.push(prioritized);
                continue;
            }
            found = Some(task.clone());
            break;
        }

        for entry in skipped {
            queue.push(entry);
        }
        found
    }

    /// Wake a parked task by ID and re-enqueue it for execution.
    ///
    /// Only tasks that are actually parked — `Waiting` (blocked on a dependency,
    /// tool, or sub-agent) or `Suspended` (budget-paused) — are woken. Any other
    /// state is a silent no-op:
    ///
    /// - `Running`: the task already has a live execution loop. Re-enqueuing it
    ///   would let the executor spawn a **second** concurrent loop over the same
    ///   task/context — duplicated inferences and tool side-effects. This is the
    ///   crux of the delegating-parent double-execution bug: a non-blocking
    ///   `task-delegate` leaves the parent `Running`, and the child's completion
    ///   fires `requeue` on it. Guarding here makes the wake idempotent and safe
    ///   regardless of caller.
    /// - `Queued`: already in the queue; re-pushing would double-enqueue it.
    /// - `Complete`/`Failed`/`Cancelled`: terminal, nothing to wake.
    #[tracing::instrument(skip_all, fields(task_id = %task_id))]
    pub async fn requeue(&self, task_id: &TaskID) -> Result<(), AgentOSError> {
        let mut tasks = self.tasks.write().await;
        let task = match tasks.get_mut(task_id) {
            Some(task) => task,
            None => return Err(AgentOSError::TaskNotFound(*task_id)),
        };
        if !matches!(task.state, TaskState::Waiting | TaskState::Suspended) {
            tracing::debug!(
                state = ?task.state,
                "requeue skipped: task is not parked (Waiting/Suspended)"
            );
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
        // Snapshot + release before taking `tasks`: every other path locks
        // `tasks` first, so holding `failure_reasons` across that acquire
        // would be a lock-order inversion.
        let reasons = self.failure_reasons.read().await.clone();
        self.tasks
            .read()
            .await
            .values()
            .map(|t| TaskSummary {
                error: if t.state == TaskState::Failed {
                    reasons.get(&t.id).cloned()
                } else {
                    None
                },
                id: t.id,
                state: t.state,
                agent_id: t.agent_id,
                // Long enough that the panel can skip the bracketed context
                // headers event-trigger prompts start with and still find a title.
                prompt_preview: t.original_prompt.chars().take(400).collect(),
                created_at: t.created_at,
                tool_calls: 0,
                tokens_used: 0,
                priority: t.priority,
                is_team_coordinator: t.is_team_coordinator,
                parent_task_id: t.parent_task_id,
                spawn_depth: t.spawn_depth,
            })
            .collect()
    }

    /// IDs of every task that is not in a terminal state. Used by the checkpoint
    /// prune sweep to keep the recovery point of tasks that are still alive
    /// (a long-parked task must stay resumable).
    pub async fn non_terminal_task_ids(&self) -> HashSet<TaskID> {
        self.tasks
            .read()
            .await
            .values()
            .filter(|t| {
                !matches!(
                    t.state,
                    TaskState::Complete | TaskState::Failed | TaskState::Cancelled
                )
            })
            .map(|t| t.id)
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
    ///
    /// Covers parked tasks (`Waiting` / `Suspended`) as well as `Running` ones:
    /// `requeue` only fires on an explicit answer/resume, so an unanswered
    /// `ask-user` or a never-resumed budget pause would otherwise sit forever
    /// with its context, checkout and work item held. The bound is the task's own
    /// `timeout` (the same effective value used for `Running`, so it already
    /// honours `autonomous_mode.task_timeout_secs` and the preemption
    /// multiplier): a task parked past its entire time budget is dead by its own
    /// configured definition, and the escalation manager already auto-denies
    /// unanswered prompts after 5 minutes, so anything still parked at `timeout`
    /// is genuinely orphaned rather than merely waiting on a slow human.
    pub async fn check_timeouts(&self) -> Vec<TimedOutTask> {
        let mut timed_out = Vec::new();
        let mut changed_tasks = Vec::new();
        let mut tasks = self.tasks.write().await;
        let now = chrono::Utc::now();
        for task in tasks.values_mut() {
            if matches!(
                task.state,
                TaskState::Running | TaskState::Waiting | TaskState::Suspended
            ) {
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
                        chain_depth: task.event_chain_depth(),
                    });
                }
            }
        }
        drop(tasks);

        for task in changed_tasks {
            self.persist_task_snapshot(task).await;
        }
        // The run loop also records this, but only for tasks it observes; a
        // reason written here survives even if that path is skipped.
        for t in &timed_out {
            self.set_failure_reason(
                &t.task_id,
                format!(
                    "Task timed out after {}s (limit {}s)",
                    t.elapsed_seconds, t.timeout_seconds
                ),
            )
            .await;
        }

        timed_out
    }

    // --- Child-Map Methods ---

    /// Register a child task under its parent for cascade-cancel.
    pub async fn register_child(&self, parent_id: TaskID, child_id: TaskID) {
        self.child_map
            .write()
            .await
            .entry(parent_id)
            .or_default()
            .push(child_id);
    }

    /// Return all child task IDs registered under a parent.
    pub async fn get_children(&self, task_id: &TaskID) -> Vec<TaskID> {
        self.child_map
            .read()
            .await
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Count children of `parent_id` that are not yet in a terminal state
    /// (Queued/Running/Waiting/Suspended). Used to bound concurrent fan-out so
    /// a single parent cannot enqueue an unbounded number of live children
    /// (fork-bomb / queue-exhaustion protection). Sequential delegation is
    /// unaffected — the count drops as children complete.
    ///
    /// Lock order: child_map guard is dropped before tasks is locked, so this
    /// never nests the two locks.
    pub async fn count_active_children(&self, parent_id: &TaskID) -> usize {
        let children = {
            let map = self.child_map.read().await;
            match map.get(parent_id) {
                Some(v) if !v.is_empty() => v.clone(),
                _ => return 0,
            }
        };
        let tasks = self.tasks.read().await;
        children
            .iter()
            .filter(|id| {
                tasks
                    .get(id)
                    .map(|t| {
                        !matches!(
                            t.state,
                            TaskState::Complete | TaskState::Failed | TaskState::Cancelled
                        )
                    })
                    .unwrap_or(false)
            })
            .count()
    }

    /// Return a brief text summary of a completed task's original prompt (first 200 chars).
    /// Used by `cmd_await_sub_agents` to surface meaningful result context.
    /// Returns `None` if the task is not found (e.g. already evicted from memory).
    pub async fn get_task_result_summary(&self, task_id: TaskID) -> Option<String> {
        self.tasks
            .read()
            .await
            .get(&task_id)
            .map(|t| t.original_prompt.chars().take(200).collect())
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
            autonomous: false,
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: Default::default(),
            spawner_agent_id: None,
            tool_categories: None,
            disable_tool_scoping: false,
            chain_depth: 0,
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
    async fn test_requeue_noops_for_running_task() {
        // Regression: a still-Running task (e.g. a non-blocking delegating parent
        // whose child just completed) must NOT be re-enqueued — doing so would
        // spawn a second concurrent execution loop over the same task.
        let scheduler = TaskScheduler::new(10);
        let mut task = make_task(5, "running parent");
        task.state = TaskState::Running;
        let task_id = task.id;
        scheduler.enqueue(task).await;
        // Drain the enqueue so the queue is empty before requeue.
        let _ = scheduler.dequeue().await;

        scheduler.requeue(&task_id).await.unwrap();

        // Requeue must not have pushed anything back onto the queue.
        assert!(
            scheduler.dequeue().await.is_none(),
            "Running task must not be re-enqueued"
        );
        let current = scheduler.get_task(&task_id).await.unwrap();
        assert_eq!(current.state, TaskState::Running);
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
    async fn test_check_timeouts_reaps_parked_tasks() {
        let scheduler = TaskScheduler::new(10);
        // Parked on an unanswered ask-user for far longer than its budget.
        let mut waiting = make_task(5, "waiting on an answer that never came");
        waiting.state = TaskState::Waiting;
        waiting.timeout = Duration::from_secs(1);
        waiting.started_at = Some(chrono::Utc::now() - chrono::Duration::seconds(120));
        let waiting_id = waiting.id;

        // Budget-paused and never resumed.
        let mut suspended = make_task(5, "suspended and never resumed");
        suspended.state = TaskState::Suspended;
        suspended.timeout = Duration::from_secs(1);
        suspended.started_at = Some(chrono::Utc::now() - chrono::Duration::seconds(120));
        let suspended_id = suspended.id;

        // Parked, but still inside its budget — must survive.
        let mut fresh = make_task(5, "just parked");
        fresh.state = TaskState::Waiting;
        fresh.timeout = Duration::from_secs(600);
        fresh.started_at = Some(chrono::Utc::now() - chrono::Duration::seconds(5));
        let fresh_id = fresh.id;

        scheduler.enqueue(waiting).await;
        scheduler.enqueue(suspended).await;
        scheduler.enqueue(fresh).await;

        let timed_out = scheduler.check_timeouts().await;
        let reaped: Vec<TaskID> = timed_out.iter().map(|t| t.task_id).collect();
        assert_eq!(
            reaped.len(),
            2,
            "both over-budget parked tasks must be reaped"
        );
        assert!(reaped.contains(&waiting_id));
        assert!(reaped.contains(&suspended_id));

        // Reaped tasks go terminal via the normal timeout path.
        assert_eq!(
            scheduler.get_task(&waiting_id).await.unwrap().state,
            TaskState::Failed
        );
        assert_eq!(
            scheduler.get_task(&suspended_id).await.unwrap().state,
            TaskState::Failed
        );
        assert_eq!(
            scheduler.get_task(&fresh_id).await.unwrap().state,
            TaskState::Waiting,
            "a task parked inside its budget must not be reaped"
        );
    }

    #[tokio::test]
    async fn test_non_terminal_task_ids_excludes_terminal() {
        let scheduler = TaskScheduler::new(10);
        let mut live = make_task(5, "parked");
        live.state = TaskState::Waiting;
        let live_id = live.id;
        let mut done = make_task(5, "finished");
        done.state = TaskState::Complete;
        let done_id = done.id;

        scheduler.register_external(live).await;
        scheduler.register_external(done).await;

        let ids = scheduler.non_terminal_task_ids().await;
        assert!(ids.contains(&live_id));
        assert!(!ids.contains(&done_id));
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

    /// The failure-reason round-trip: recorded while Failed, reported by both
    /// `failure_reason` and `list_tasks`, NOT reported once the task reaches a
    /// non-failed state (a retried-then-succeeded task must not keep showing a
    /// stale reason), and still there after a restart.
    #[tokio::test]
    async fn test_failure_reason_is_scoped_to_failed_state_and_survives_restart() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("kernel_state.db");
        let store = Arc::new(
            KernelStateStore::open(db_path)
                .await
                .expect("state store should open"),
        );

        let scheduler = TaskScheduler::with_state_store(10, Some(store.clone()));
        let task = make_task(5, "will fail");
        let task_id = task.id;
        scheduler.enqueue(task).await;
        scheduler
            .update_state(&task_id, TaskState::Failed)
            .await
            .unwrap();
        scheduler
            .set_failure_reason(&task_id, "LLM error: connection refused".to_string())
            .await;

        assert_eq!(
            scheduler.failure_reason(&task_id).await.as_deref(),
            Some("LLM error: connection refused")
        );
        let listed = scheduler.list_tasks().await;
        let row = listed.iter().find(|t| t.id == task_id).unwrap();
        assert_eq!(row.error.as_deref(), Some("LLM error: connection refused"));

        // Retried and succeeded: the reason must stop being reported.
        scheduler
            .update_state(&task_id, TaskState::Complete)
            .await
            .unwrap();
        assert_eq!(
            scheduler.failure_reason(&task_id).await,
            None,
            "a task that later completed must not report its old failure reason"
        );
        let listed = scheduler.list_tasks().await;
        let row = listed.iter().find(|t| t.id == task_id).unwrap();
        assert_eq!(row.error, None);

        // A task that stays failed keeps its reason across a restart.
        let failed = make_task(5, "stays failed");
        let failed_id = failed.id;
        scheduler.enqueue(failed).await;
        scheduler
            .update_state(&failed_id, TaskState::Failed)
            .await
            .unwrap();
        scheduler
            .set_failure_reason(&failed_id, "boom".to_string())
            .await;

        let restored = TaskScheduler::with_state_store(10, Some(store));
        let loaded = restored
            .restore_terminal_history(100)
            .await
            .expect("history restore should succeed");
        assert!(loaded >= 2, "both terminal tasks should be restored");
        assert_eq!(
            restored.failure_reason(&failed_id).await.as_deref(),
            Some("boom")
        );
        assert_eq!(
            restored.failure_reason(&task_id).await,
            None,
            "the completed task's stale reason must not come back from the DB"
        );
    }

    /// `prune_failure_reasons` drops reasons for tasks the scheduler no longer
    /// tracks, and keeps the ones it does.
    #[tokio::test]
    async fn test_prune_failure_reasons_drops_only_untracked() {
        let scheduler = TaskScheduler::new(10);
        let task = make_task(5, "tracked");
        let tracked_id = task.id;
        scheduler.enqueue(task).await;
        scheduler
            .update_state(&tracked_id, TaskState::Failed)
            .await
            .unwrap();
        scheduler
            .set_failure_reason(&tracked_id, "kept".to_string())
            .await;
        // A reason for a task the scheduler never had (e.g. already purged).
        scheduler
            .set_failure_reason(&TaskID::new(), "orphan".to_string())
            .await;

        assert_eq!(scheduler.prune_failure_reasons().await, 1);
        assert_eq!(
            scheduler.failure_reason(&tracked_id).await.as_deref(),
            Some("kept")
        );
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
        let (restored_count, stale) = restored
            .restore_from_store(24, &HashSet::new())
            .await
            .expect("restore should succeed");
        assert_eq!(restored_count, 3);
        assert_eq!(stale, 0, "fresh tasks are not stale");

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

    /// Same agent for every task — the queue cap is per-agent, and `make_task`
    /// mints a fresh `AgentID` each call.
    fn make_task_for(agent_id: AgentID, priority: u8, prompt: &str) -> AgentTask {
        let mut task = make_task(priority, prompt);
        task.agent_id = agent_id;
        task
    }

    #[tokio::test]
    async fn test_enqueue_rejects_past_agent_cap() {
        let scheduler = TaskScheduler::with_limits(10, None, 3);
        let agent = AgentID::new();

        let ids: Vec<TaskID> = futures::future::join_all(
            (0..5).map(|i| scheduler.enqueue(make_task_for(agent, 5, &format!("t{i}")))),
        )
        .await;

        assert_eq!(scheduler.queued_count_for_agent(&agent).await, 3);
        let failed = futures::future::join_all(ids.iter().map(|id| scheduler.get_task(id)))
            .await
            .into_iter()
            .flatten()
            .filter(|t| t.state == TaskState::Failed)
            .count();
        assert_eq!(failed, 2, "over-cap tasks are recorded as Failed");

        // Only the 3 accepted tasks are actually runnable.
        assert!(scheduler.dequeue().await.is_some());
        assert!(scheduler.dequeue().await.is_some());
        assert!(scheduler.dequeue().await.is_some());
        assert!(scheduler.dequeue().await.is_none());
    }

    #[tokio::test]
    async fn test_enqueue_cap_is_per_agent() {
        let scheduler = TaskScheduler::with_limits(10, None, 2);
        let a = AgentID::new();
        let b = AgentID::new();

        for i in 0..3 {
            scheduler
                .enqueue(make_task_for(a, 5, &format!("a{i}")))
                .await;
            scheduler
                .enqueue(make_task_for(b, 5, &format!("b{i}")))
                .await;
        }

        assert_eq!(scheduler.queued_count_for_agent(&a).await, 2);
        assert_eq!(scheduler.queued_count_for_agent(&b).await, 2);
    }

    #[tokio::test]
    async fn test_enqueue_cap_zero_disables() {
        let scheduler = TaskScheduler::with_limits(10, None, 0);
        let agent = AgentID::new();
        for i in 0..50 {
            scheduler
                .enqueue(make_task_for(agent, 5, &format!("t{i}")))
                .await;
        }
        assert_eq!(scheduler.queued_count_for_agent(&agent).await, 50);
    }

    #[tokio::test]
    async fn test_purge_agent_tasks_drops_queued_only() {
        let scheduler = TaskScheduler::new(10);
        let a = AgentID::new();
        let b = AgentID::new();

        for i in 0..3 {
            scheduler
                .enqueue(make_task_for(a, 5, &format!("a{i}")))
                .await;
        }
        let running = make_task_for(a, 5, "a-running");
        let running_id = running.id;
        scheduler.enqueue(running).await;
        scheduler
            .update_state(&running_id, TaskState::Running)
            .await
            .unwrap();

        for i in 0..2 {
            scheduler
                .enqueue(make_task_for(b, 5, &format!("b{i}")))
                .await;
        }

        let purged = scheduler.purge_agent_tasks(&a, &[]).await;
        assert_eq!(purged, 3, "only agent a's queued tasks are purged");
        assert_eq!(scheduler.queued_count_for_agent(&a).await, 0);
        assert_eq!(scheduler.queued_count_for_agent(&b).await, 2);
        assert!(
            scheduler.get_task(&running_id).await.is_some(),
            "running tasks survive a purge — cancel those individually"
        );

        // The purged entries must not resurface through the heap.
        let remaining = [
            scheduler.dequeue().await,
            scheduler.dequeue().await,
            scheduler.dequeue().await,
        ];
        let popped: Vec<AgentID> = remaining.iter().flatten().map(|t| t.agent_id).collect();
        assert_eq!(popped, vec![b, b], "only agent b's work remains queued");
    }

    #[tokio::test]
    async fn test_purge_wakes_tasks_waiting_on_purged_dependencies() {
        let scheduler = TaskScheduler::new(10);
        let worker = AgentID::new();
        let orchestrator = AgentID::new();

        // Parent delegates to a child, then parks waiting on it.
        let parent = make_task_for(orchestrator, 5, "parent");
        let parent_id = parent.id;
        scheduler.enqueue(parent).await;

        let child = make_task_for(worker, 5, "child");
        let child_id = child.id;
        scheduler.enqueue(child).await;

        scheduler.add_dependency(parent_id, child_id).await;
        scheduler
            .update_state(&parent_id, TaskState::Waiting)
            .await
            .unwrap();

        // Operator drains the worker's backlog — the documented recovery.
        assert_eq!(scheduler.purge_agent_tasks(&worker, &[]).await, 1);

        // The child is gone, so `complete_dependency` will never fire for it.
        // Without an explicit wake the parent stays parked until the
        // `check_timeouts` reaper fails it a whole timeout budget later.
        let parent_now = scheduler.get_task(&parent_id).await.unwrap();
        assert_eq!(
            parent_now.state,
            TaskState::Queued,
            "a task waiting on a purged dependency must be woken, not stranded"
        );
    }

    #[tokio::test]
    async fn test_reenqueue_bypasses_the_cap() {
        let scheduler = TaskScheduler::with_limits(10, None, 2);
        let agent = AgentID::new();

        let resumed = make_task_for(agent, 5, "resumed from checkpoint");
        let resumed_id = resumed.id;
        scheduler.enqueue(resumed.clone()).await;
        scheduler.enqueue(make_task_for(agent, 5, "other")).await;
        assert_eq!(scheduler.queued_count_for_agent(&agent).await, 2);

        // Agent is now at cap. Re-enqueueing the *same* task (what
        // `cmd_resume_task` does) must not flip it to Failed — it is already
        // counted, and failing it here would burn a poison-pill boot-resume.
        scheduler.enqueue(resumed).await;
        let after = scheduler.get_task(&resumed_id).await.unwrap();
        assert_eq!(
            after.state,
            TaskState::Queued,
            "a re-enqueue must bypass the cap it is already counted against"
        );
    }

    #[tokio::test]
    async fn test_restore_exempts_checkpointed_tasks_from_cutoff() {
        let dir = tempdir().expect("temp dir");
        let store = Arc::new(
            KernelStateStore::open(dir.path().join("kernel_state.db"))
                .await
                .expect("state store should open"),
        );
        let scheduler = TaskScheduler::with_state_store(10, Some(store.clone()));

        let mut old_with_checkpoint = make_task(5, "crashed mid-flight");
        old_with_checkpoint.created_at = chrono::Utc::now() - chrono::Duration::hours(48);
        let resumable_id = old_with_checkpoint.id;
        scheduler.enqueue(old_with_checkpoint).await;

        let mut old_storm_task = make_task(5, "storm backlog");
        old_storm_task.created_at = chrono::Utc::now() - chrono::Duration::hours(48);
        let storm_id = old_storm_task.id;
        scheduler.enqueue(old_storm_task).await;

        let resumable: HashSet<TaskID> = [resumable_id].into_iter().collect();
        let restored = TaskScheduler::with_state_store(10, Some(store));
        let (count, dropped) = restored.restore_from_store(24, &resumable).await.unwrap();

        assert_eq!((count, dropped), (1, 1));
        assert!(
            restored.get_task(&resumable_id).await.is_some(),
            "a checkpointed task must survive the cutoff — recover_checkpointed_tasks \
             looks it up in the scheduler map, so cancelling it drops the resume"
        );
        assert!(
            restored.get_task(&storm_id).await.is_none(),
            "an equally old task with no checkpoint is still culled"
        );
    }

    #[tokio::test]
    async fn test_purge_clears_persisted_rows() {
        let dir = tempdir().expect("temp dir");
        let store = Arc::new(
            KernelStateStore::open(dir.path().join("kernel_state.db"))
                .await
                .expect("state store should open"),
        );
        let scheduler = TaskScheduler::with_state_store(10, Some(store.clone()));
        let agent = AgentID::new();
        for i in 0..4 {
            scheduler
                .enqueue(make_task_for(agent, 5, &format!("t{i}")))
                .await;
        }

        assert_eq!(scheduler.purge_agent_tasks(&agent, &[]).await, 4);

        // A fresh kernel must not replay them.
        let restored = TaskScheduler::with_state_store(10, Some(store));
        let (count, _) = restored
            .restore_from_store(24, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(count, 0, "purged rows are gone from persistence too");
    }

    #[tokio::test]
    async fn test_restore_cancels_stale_queued_tasks() {
        let dir = tempdir().expect("temp dir");
        let store = Arc::new(
            KernelStateStore::open(dir.path().join("kernel_state.db"))
                .await
                .expect("state store should open"),
        );
        let scheduler = TaskScheduler::with_state_store(10, Some(store.clone()));

        let mut stale = make_task(5, "stale");
        stale.created_at = chrono::Utc::now() - chrono::Duration::hours(48);
        let stale_id = stale.id;
        scheduler.enqueue(stale).await;

        let fresh = make_task(5, "fresh");
        let fresh_id = fresh.id;
        scheduler.enqueue(fresh).await;

        let restored = TaskScheduler::with_state_store(10, Some(store.clone()));
        let (count, dropped) = restored
            .restore_from_store(24, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(count, 1, "only the fresh task is replayed");
        assert_eq!(dropped, 1);
        assert!(restored.get_task(&fresh_id).await.is_some());
        assert!(restored.get_task(&stale_id).await.is_none());

        // The cancellation is persisted, so the next boot sees nothing at all.
        let again = TaskScheduler::with_state_store(10, Some(store));
        let (count2, dropped2) = again.restore_from_store(24, &HashSet::new()).await.unwrap();
        assert_eq!((count2, dropped2), (1, 0), "stale row is not reconsidered");
    }

    #[tokio::test]
    async fn test_restore_cutoff_zero_replays_everything() {
        let dir = tempdir().expect("temp dir");
        let store = Arc::new(
            KernelStateStore::open(dir.path().join("kernel_state.db"))
                .await
                .expect("state store should open"),
        );
        let scheduler = TaskScheduler::with_state_store(10, Some(store.clone()));
        let mut ancient = make_task(5, "ancient");
        ancient.created_at = chrono::Utc::now() - chrono::Duration::days(30);
        scheduler.enqueue(ancient).await;

        let restored = TaskScheduler::with_state_store(10, Some(store));
        let (count, dropped) = restored
            .restore_from_store(0, &HashSet::new())
            .await
            .unwrap();
        assert_eq!((count, dropped), (1, 0), "cutoff 0 disables the guard");
    }

    #[tokio::test]
    async fn test_dequeue_skips_paused_agent() {
        let scheduler = TaskScheduler::new(10);
        let paused = AgentID::new();
        let active = AgentID::new();

        // Higher priority for the paused agent, so it would win without the filter.
        let paused_task = make_task_for(paused, 9, "paused work");
        let paused_id = paused_task.id;
        scheduler.enqueue(paused_task).await;
        scheduler
            .enqueue(make_task_for(active, 1, "active work"))
            .await;

        let got = scheduler
            .dequeue_runnable(|id| *id != paused)
            .await
            .expect("active agent's task should dequeue");
        assert_eq!(got.agent_id, active);

        // The paused agent's task is held, not lost.
        assert_eq!(scheduler.queued_count_for_agent(&paused).await, 1);
        let after_resume = scheduler.dequeue().await.expect("task survives the skip");
        assert_eq!(after_resume.id, paused_id);
    }
}
