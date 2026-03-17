use agentos_types::AgentID;
use std::collections::{HashMap, HashSet, VecDeque};
use tokio::sync::{Mutex, RwLock};

/// Notification emitted when a lower-priority holder is preempted to resolve a deadlock.
#[derive(Debug, Clone)]
pub struct PreemptionNotification {
    pub preempted_agent: AgentID,
    pub preempting_agent: AgentID,
    pub resource_id: String,
}

/// Notification emitted when a deadlock cycle is detected and the request is rejected.
#[derive(Debug, Clone)]
pub struct DeadlockNotification {
    pub blocked_agent: AgentID,
    pub holder_agent: AgentID,
    pub resource_id: String,
}

/// Lock mode for a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Multiple agents can hold a shared read lock simultaneously.
    Shared,
    /// Only one agent can hold an exclusive write lock at a time.
    Exclusive,
}

/// A held lock on a resource.
#[derive(Debug, Clone)]
pub struct ResourceLock {
    /// Stable resource identifier (e.g. "fs:/home/user/report.md", "browser:0").
    pub resource_id: String,
    pub lock_mode: LockMode,
    pub held_by: AgentID,
    pub acquired_at: chrono::DateTime<chrono::Utc>,
    /// Auto-release TTL in seconds. 0 = no auto-release.
    pub ttl_seconds: u64,
}

/// Summary of a lock for CLI display.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LockSummary {
    pub resource_id: String,
    pub lock_mode: String,
    pub held_by: String,
    pub acquired_at: String,
    pub ttl_seconds: u64,
    pub waiters: usize,
}

/// A pending lock request queued behind the current holder.
#[derive(Debug)]
struct LockWaiter {
    agent_id: AgentID,
    mode: LockMode,
    /// Priority of the requesting agent (higher = more important). Used for
    /// priority-based preemption on deadlock detection.
    priority: u8,
    /// TTL requested by the caller — preserved so the grant uses the original value.
    ttl_seconds: u64,
    notify: tokio::sync::oneshot::Sender<Result<(), String>>,
}

/// Per-resource lock state.
struct ResourceState {
    /// Current shared lock holders (when mode is Shared, multiple agents can hold).
    shared_holders: Vec<AgentID>,
    /// Current exclusive lock holder.
    exclusive_holder: Option<AgentID>,
    /// Priority of the current holder(s) — used for preemption decisions.
    holder_priority: u8,
    /// Acquired timestamp and TTL for the current lock.
    acquired_at: Option<chrono::DateTime<chrono::Utc>>,
    ttl_seconds: u64,
    /// FIFO queue of waiters.
    waiters: VecDeque<LockWaiter>,
}

impl ResourceState {
    fn new() -> Self {
        Self {
            shared_holders: Vec::new(),
            exclusive_holder: None,
            holder_priority: 0,
            acquired_at: None,
            ttl_seconds: 0,
            waiters: VecDeque::new(),
        }
    }

    fn is_locked(&self) -> bool {
        self.exclusive_holder.is_some() || !self.shared_holders.is_empty()
    }

    fn is_expired(&self) -> bool {
        if self.ttl_seconds == 0 {
            return false;
        }
        if let Some(acquired) = self.acquired_at {
            // Use max(0) before the cast: a backwards NTP clock adjustment can
            // produce a negative duration, and casting a negative i64 to u64 wraps
            // to a huge number, which would falsely expire the lock.
            let elapsed = chrono::Utc::now()
                .signed_duration_since(acquired)
                .num_seconds()
                .max(0) as u64;
            return elapsed >= self.ttl_seconds;
        }
        false
    }

    /// Try to grant a lock immediately. Returns true if granted.
    fn try_grant(&mut self, agent_id: AgentID, mode: LockMode, ttl: u64) -> bool {
        self.try_grant_with_priority(agent_id, mode, ttl, 0)
    }

    fn try_grant_with_priority(
        &mut self,
        agent_id: AgentID,
        mode: LockMode,
        ttl: u64,
        priority: u8,
    ) -> bool {
        // Expired locks are released automatically before granting
        if self.is_expired() {
            self.release_all();
        }

        match mode {
            LockMode::Shared => {
                // Shared lock: granted if no exclusive holder
                if self.exclusive_holder.is_none() {
                    self.shared_holders.push(agent_id);
                    if self.acquired_at.is_none() {
                        self.acquired_at = Some(chrono::Utc::now());
                        self.ttl_seconds = ttl;
                    }
                    self.holder_priority = self.holder_priority.max(priority);
                    true
                } else {
                    false
                }
            }
            LockMode::Exclusive => {
                // Exclusive lock: granted only if completely unlocked
                if !self.is_locked() {
                    self.exclusive_holder = Some(agent_id);
                    self.acquired_at = Some(chrono::Utc::now());
                    self.ttl_seconds = ttl;
                    self.holder_priority = priority;
                    true
                } else {
                    false
                }
            }
        }
    }

    fn release_all(&mut self) {
        self.shared_holders.clear();
        self.exclusive_holder = None;
        self.holder_priority = 0;
        self.acquired_at = None;
        self.ttl_seconds = 0;
    }

    fn release_agent(&mut self, agent_id: AgentID) {
        self.shared_holders.retain(|&id| id != agent_id);
        if self.exclusive_holder == Some(agent_id) {
            self.exclusive_holder = None;
            self.acquired_at = None;
            self.ttl_seconds = 0;
        }
        if !self.is_locked() {
            self.acquired_at = None;
            self.ttl_seconds = 0;
        }
    }
}

/// Kernel-owned resource arbitration engine.
///
/// Enforces exclusive/shared locking on named resources with FIFO waiter queues.
/// Prevents concurrent file writes, browser conflicts, and API slot exhaustion
/// across agents running in parallel (Spec §8).
///
/// Deadlock detection: maintains a wait-for graph (`waiter → holder`) and runs
/// DFS cycle detection before queuing any new waiter. Returns `Err` immediately
/// if adding the edge would create a cycle.
pub struct ResourceArbiter {
    resources: RwLock<HashMap<String, Mutex<ResourceState>>>,
    /// Wait-for graph: agent_id → the agent_id it is currently blocked on.
    /// Protected by a std::sync::Mutex so it can be accessed from both async
    /// and sync (wake_waiters) call sites without holding an await point.
    wait_for: std::sync::Mutex<HashMap<AgentID, AgentID>>,
    /// Optional channel for notifying the kernel of preemption/deadlock events.
    arbiter_sender: Option<tokio::sync::mpsc::Sender<ArbiterNotification>>,
}

/// Notification types emitted by the resource arbiter.
#[derive(Debug, Clone)]
pub enum ArbiterNotification {
    Preemption(PreemptionNotification),
    Deadlock(DeadlockNotification),
}

impl ResourceArbiter {
    pub fn new() -> Self {
        Self {
            resources: RwLock::new(HashMap::new()),
            wait_for: std::sync::Mutex::new(HashMap::new()),
            arbiter_sender: None,
        }
    }

    /// Set the notification sender for preemption and deadlock events.
    pub fn set_arbiter_sender(&mut self, sender: tokio::sync::mpsc::Sender<ArbiterNotification>) {
        self.arbiter_sender = Some(sender);
    }

    /// Returns `true` if adding the edge `waiter → holder` would introduce a
    /// cycle in the wait-for graph.  Runs DFS from `holder` following existing
    /// edges; a cycle exists if we reach `waiter`.
    fn would_deadlock(
        wait_for: &HashMap<AgentID, AgentID>,
        waiter: AgentID,
        holder: AgentID,
    ) -> bool {
        let mut current = holder;
        let mut visited = HashSet::new();
        loop {
            if current == waiter {
                return true;
            }
            if !visited.insert(current) {
                // Already traversed this node — no path to waiter
                return false;
            }
            match wait_for.get(&current) {
                Some(&next) => current = next,
                None => return false,
            }
        }
    }

    /// Acquire a lock on a resource. Blocks until the lock is available.
    /// Returns `Ok(())` when the lock is held, `Err` if TTL expired or rejected.
    ///
    /// # Arguments
    /// * `resource_id` - The resource to lock (e.g. "fs:/path/to/file")
    /// * `agent_id` - The agent requesting the lock
    /// * `mode` - `Shared` (read) or `Exclusive` (write)
    /// * `ttl_seconds` - Auto-release after this many seconds (0 = no auto-release)
    pub async fn acquire(
        &self,
        resource_id: &str,
        agent_id: AgentID,
        mode: LockMode,
        ttl_seconds: u64,
    ) -> Result<(), String> {
        self.acquire_with_priority(resource_id, agent_id, mode, ttl_seconds, 0)
            .await
    }

    /// Acquire a lock with a priority hint. On deadlock, a higher-priority requester
    /// can preempt a lower-priority holder instead of returning an error.
    pub async fn acquire_with_priority(
        &self,
        resource_id: &str,
        agent_id: AgentID,
        mode: LockMode,
        ttl_seconds: u64,
        priority: u8,
    ) -> Result<(), String> {
        // Ensure the resource entry exists
        {
            let mut resources = self.resources.write().await;
            resources
                .entry(resource_id.to_string())
                .or_insert_with(|| Mutex::new(ResourceState::new()));
        }

        // Try immediate grant
        let (tx, rx) = tokio::sync::oneshot::channel();

        {
            let resources = self.resources.read().await;
            let mut state = resources[resource_id].lock().await;

            if state.try_grant(agent_id, mode, ttl_seconds) {
                // Immediately granted — no need for the waiter channel
                drop(tx); // discard
                return Ok(());
            }

            // Determine current holder(s) for deadlock detection.
            // Use the exclusive holder if present, otherwise first shared holder.
            let holder_opt = state
                .exclusive_holder
                .or_else(|| state.shared_holders.first().copied());

            if let Some(holder) = holder_opt {
                // Deadlock check: would adding waiter→holder create a cycle?
                let mut wf = self
                    .wait_for
                    .lock()
                    .map_err(|_| "wait_for lock poisoned".to_string())?;

                if Self::would_deadlock(&wf, agent_id, holder) {
                    // Priority-based preemption: if the requester has higher priority
                    // than the current holder, preempt the holder.
                    if priority > state.holder_priority {
                        tracing::warn!(
                            agent = %agent_id,
                            resource = %resource_id,
                            waiter_priority = priority,
                            holder_priority = state.holder_priority,
                            "Preempting lower-priority holder to resolve deadlock"
                        );
                        // Notify kernel of preemption
                        if let Some(ref sender) = self.arbiter_sender {
                            if let Err(e) = sender.try_send(ArbiterNotification::Preemption(
                                PreemptionNotification {
                                    preempted_agent: holder,
                                    preempting_agent: agent_id,
                                    resource_id: resource_id.to_string(),
                                },
                            )) {
                                tracing::warn!(error = %e, "Failed to send preemption notification (channel full or closed)");
                            }
                        }
                        state.release_all();
                        // Remove preempted holder from wait-for graph
                        wf.remove(&holder);
                        drop(wf);
                        // Wake any other waiters (they'll be notified of release)
                        self.wake_waiters(&mut state);
                        // Now try to grant immediately
                        if state.try_grant_with_priority(agent_id, mode, ttl_seconds, priority) {
                            drop(tx);
                            return Ok(());
                        }
                    }
                    // Notify kernel of deadlock detection
                    if let Some(ref sender) = self.arbiter_sender {
                        if let Err(e) =
                            sender.try_send(ArbiterNotification::Deadlock(DeadlockNotification {
                                blocked_agent: agent_id,
                                holder_agent: holder,
                                resource_id: resource_id.to_string(),
                            }))
                        {
                            tracing::warn!(error = %e, "Failed to send deadlock notification (channel full or closed)");
                        }
                    }
                    return Err(format!(
                        "Deadlock detected: agent {} waiting on '{}' held by {} would create a wait cycle",
                        agent_id, resource_id, holder
                    ));
                }

                // Record the wait edge
                wf.insert(agent_id, holder);
            }

            // Queue the waiter
            state.waiters.push_back(LockWaiter {
                agent_id,
                mode,
                priority,
                ttl_seconds,
                notify: tx,
            });
        }

        // Wait for the lock to be granted (or an error)
        let result = rx
            .await
            .map_err(|_| format!("Lock wait for '{}' was cancelled", resource_id))?;

        // Regardless of outcome, remove the wait edge (we are no longer waiting)
        match self.wait_for.lock() {
            Ok(mut wf) => {
                wf.remove(&agent_id);
            }
            Err(_) => {
                tracing::error!(
                    agent_id = %agent_id,
                    "wait_for graph lock poisoned; wait edge for agent may be stale"
                );
            }
        }

        result
    }

    /// Try to acquire a lock without blocking.
    /// Returns `Ok(true)` if granted, `Ok(false)` if not available, `Err` on internal error.
    pub async fn try_acquire(
        &self,
        resource_id: &str,
        agent_id: AgentID,
        mode: LockMode,
        ttl_seconds: u64,
    ) -> bool {
        let mut resources = self.resources.write().await;
        let state = resources
            .entry(resource_id.to_string())
            .or_insert_with(|| Mutex::new(ResourceState::new()));

        let mut locked_state = state.lock().await;
        locked_state.try_grant(agent_id, mode, ttl_seconds)
    }

    /// Release a lock held by `agent_id` on `resource_id`.
    /// After releasing, wakes the next waiter in FIFO order if the resource is now free.
    pub async fn release(&self, resource_id: &str, agent_id: AgentID) {
        let resources = self.resources.read().await;
        if let Some(mutex) = resources.get(resource_id) {
            let mut state = mutex.lock().await;
            state.release_agent(agent_id);

            // Wake next waiter(s) in FIFO order
            self.wake_waiters(&mut state);
        }
    }

    /// Release all locks held by an agent (called on agent disconnect or task failure).
    pub async fn release_all_for_agent(&self, agent_id: AgentID) {
        // Also remove any wait edge for this agent (in case it was queued but cancelled)
        match self.wait_for.lock() {
            Ok(mut wf) => {
                wf.remove(&agent_id);
            }
            Err(_) => {
                tracing::error!(
                    agent_id = %agent_id,
                    "wait_for graph lock poisoned; wait edge for agent may be stale"
                );
            }
        }
        let resources = self.resources.read().await;
        for mutex in resources.values() {
            let mut state = mutex.lock().await;
            if state.exclusive_holder == Some(agent_id) || state.shared_holders.contains(&agent_id)
            {
                state.release_agent(agent_id);
                self.wake_waiters(&mut state);
            }
        }
    }

    /// Wake the next eligible waiter(s) in FIFO order.
    /// Also removes granted agents from the wait-for graph.
    fn wake_waiters(&self, state: &mut ResourceState) {
        loop {
            // Check expiry before accessing the front waiter to avoid borrow conflicts
            if state.is_expired() {
                state.release_all();
            }
            let Some(waiter) = state.waiters.front() else {
                break;
            };
            let mode = waiter.mode;
            let agent_id = waiter.agent_id;
            let priority = waiter.priority;
            let ttl = waiter.ttl_seconds;

            if state.try_grant_with_priority(agent_id, mode, ttl, priority) {
                // Grant succeeded — pop and notify
                if let Some(w) = state.waiters.pop_front() {
                    // Remove from wait-for graph: agent is no longer waiting
                    match self.wait_for.lock() {
                        Ok(mut wf) => {
                            wf.remove(&w.agent_id);
                        }
                        Err(_) => {
                            tracing::error!(
                                agent_id = %w.agent_id,
                                "wait_for graph lock poisoned; wait edge for agent may be stale"
                            );
                        }
                    }
                    let _ = w.notify.send(Ok(()));
                }
                // For shared locks, continue trying to wake more shared waiters
                if mode == LockMode::Exclusive {
                    break;
                }
            } else {
                // Cannot grant — stop waking (FIFO: don't skip ahead)
                break;
            }
        }
    }

    /// Sweep expired locks and wake their waiters. Call periodically.
    pub async fn sweep_expired(&self) {
        let resources = self.resources.read().await;
        for mutex in resources.values() {
            let mut state = mutex.lock().await;
            if state.is_expired() {
                state.release_all();
                self.wake_waiters(&mut state);
            }
        }
    }

    /// List all currently held locks for CLI display.
    pub async fn list_locks(&self) -> Vec<LockSummary> {
        let resources = self.resources.read().await;
        let mut summaries = Vec::new();
        for (resource_id, mutex) in resources.iter() {
            let state = mutex.lock().await;
            if !state.is_locked() {
                continue;
            }
            let (mode_str, held_by_str) = if let Some(holder) = state.exclusive_holder {
                ("exclusive".to_string(), holder.to_string())
            } else {
                let holders: Vec<String> = state
                    .shared_holders
                    .iter()
                    .map(|id| id.to_string())
                    .collect();
                ("shared".to_string(), holders.join(", "))
            };

            summaries.push(LockSummary {
                resource_id: resource_id.clone(),
                lock_mode: mode_str,
                held_by: held_by_str,
                acquired_at: state
                    .acquired_at
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default(),
                ttl_seconds: state.ttl_seconds,
                waiters: state.waiters.len(),
            });
        }
        summaries
    }

    /// Return contention statistics for all resources with active locks or waiters.
    pub async fn contention_stats(&self) -> serde_json::Value {
        let resources = self.resources.read().await;
        let mut stats = Vec::new();
        for (resource_id, mutex) in resources.iter() {
            let state = mutex.lock().await;
            if !state.is_locked() && state.waiters.is_empty() {
                continue;
            }
            let holders = if let Some(h) = state.exclusive_holder {
                vec![h.to_string()]
            } else {
                state
                    .shared_holders
                    .iter()
                    .map(|id| id.to_string())
                    .collect()
            };
            let waiter_agents: Vec<String> = state
                .waiters
                .iter()
                .map(|w| w.agent_id.to_string())
                .collect();
            stats.push(serde_json::json!({
                "resource_id": resource_id,
                "lock_mode": if state.exclusive_holder.is_some() { "exclusive" } else { "shared" },
                "holders": holders,
                "holder_priority": state.holder_priority,
                "waiter_count": state.waiters.len(),
                "waiters": waiter_agents,
            }));
        }
        serde_json::json!({
            "contended_resources": stats.len(),
            "resources": stats,
        })
    }

    /// Count of currently active locks across all resources.
    pub async fn active_lock_count(&self) -> usize {
        let resources = self.resources.read().await;
        let mut count = 0;
        for mutex in resources.values() {
            let state = mutex.lock().await;
            if state.is_locked() {
                count += 1;
            }
        }
        count
    }
}

impl Default for ResourceArbiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_exclusive_lock_basic() {
        let arbiter = ResourceArbiter::new();
        let agent_a = AgentID::new();

        assert!(
            arbiter
                .try_acquire("fs:/tmp/test.txt", agent_a, LockMode::Exclusive, 30)
                .await
        );
        let locks = arbiter.list_locks().await;
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].lock_mode, "exclusive");

        arbiter.release("fs:/tmp/test.txt", agent_a).await;
        let locks = arbiter.list_locks().await;
        assert!(locks.is_empty());
    }

    #[tokio::test]
    async fn test_shared_locks_coexist() {
        let arbiter = ResourceArbiter::new();
        let a = AgentID::new();
        let b = AgentID::new();

        assert!(
            arbiter
                .try_acquire("fs:/tmp/data.csv", a, LockMode::Shared, 30)
                .await
        );
        assert!(
            arbiter
                .try_acquire("fs:/tmp/data.csv", b, LockMode::Shared, 30)
                .await
        );

        let locks = arbiter.list_locks().await;
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].lock_mode, "shared");
    }

    #[tokio::test]
    async fn test_exclusive_blocks_while_held() {
        let arbiter = ResourceArbiter::new();
        let a = AgentID::new();
        let b = AgentID::new();

        assert!(
            arbiter
                .try_acquire("browser:0", a, LockMode::Exclusive, 30)
                .await
        );
        // Agent B cannot get exclusive while A holds it
        assert!(
            !arbiter
                .try_acquire("browser:0", b, LockMode::Exclusive, 30)
                .await
        );

        arbiter.release("browser:0", a).await;
        // Now B can acquire
        assert!(
            arbiter
                .try_acquire("browser:0", b, LockMode::Exclusive, 30)
                .await
        );
    }

    #[tokio::test]
    async fn test_shared_blocks_exclusive() {
        let arbiter = ResourceArbiter::new();
        let a = AgentID::new();
        let b = AgentID::new();

        assert!(
            arbiter
                .try_acquire("fs:/report.md", a, LockMode::Shared, 30)
                .await
        );
        // Cannot get exclusive while any shared lock is held
        assert!(
            !arbiter
                .try_acquire("fs:/report.md", b, LockMode::Exclusive, 30)
                .await
        );
    }

    #[tokio::test]
    async fn test_release_all_for_agent() {
        let arbiter = ResourceArbiter::new();
        let agent = AgentID::new();

        arbiter
            .try_acquire("fs:/file1", agent, LockMode::Exclusive, 0)
            .await;
        arbiter
            .try_acquire("fs:/file2", agent, LockMode::Shared, 0)
            .await;
        assert_eq!(arbiter.active_lock_count().await, 2);

        arbiter.release_all_for_agent(agent).await;
        assert_eq!(arbiter.active_lock_count().await, 0);
    }

    #[test]
    fn test_deadlock_detection_static() {
        // A waits on B, B waits on C. Adding C→A creates a cycle A→B→C→A.
        let a = AgentID::new();
        let b = AgentID::new();
        let c = AgentID::new();
        let mut wf = HashMap::new();
        wf.insert(a, b); // A waits on B
        wf.insert(b, c); // B waits on C
                         // Adding C→A: traverse from A: A→B→C→A == waiter → cycle
        assert!(ResourceArbiter::would_deadlock(&wf, c, a));
        // Adding D→A is fine (no cycle)
        let d = AgentID::new();
        assert!(!ResourceArbiter::would_deadlock(&wf, d, a));
    }

    #[tokio::test]
    async fn test_deadlock_rejected_at_acquire() {
        let arbiter = std::sync::Arc::new(ResourceArbiter::new());
        let a = AgentID::new();
        let b = AgentID::new();

        // A holds res1, B holds res2
        assert!(arbiter.try_acquire("res1", a, LockMode::Exclusive, 0).await);
        assert!(arbiter.try_acquire("res2", b, LockMode::Exclusive, 0).await);

        // B tries to acquire res1 (held by A) — queued in background
        let arbiter2 = arbiter.clone();
        let _handle =
            tokio::spawn(async move { arbiter2.acquire("res1", b, LockMode::Exclusive, 0).await });
        // Give time for B to queue
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // A now tries to acquire res2 (held by B) — this would create A→B→A cycle
        let result = arbiter.acquire("res2", a, LockMode::Exclusive, 0).await;
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("Deadlock detected"), "got: {msg}");
    }

    #[tokio::test]
    async fn test_waiter_is_woken_on_release() {
        let arbiter = std::sync::Arc::new(ResourceArbiter::new());
        let a = AgentID::new();
        let b = AgentID::new();

        // A holds exclusive lock
        assert!(
            arbiter
                .try_acquire("fs:/shared.txt", a, LockMode::Exclusive, 0)
                .await
        );

        // B waits for it (in background)
        let arbiter2 = arbiter.clone();
        let handle = tokio::spawn(async move {
            arbiter2
                .acquire("fs:/shared.txt", b, LockMode::Exclusive, 0)
                .await
        });

        // Let B queue up
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // A releases
        arbiter.release("fs:/shared.txt", a).await;

        // B should now have the lock
        let result = tokio::time::timeout(tokio::time::Duration::from_millis(500), handle).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_ok_and(|r| r.is_ok()));
    }
}
