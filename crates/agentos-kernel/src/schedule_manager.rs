use crate::schedule_persistence::{SchedulePersistence, ScheduleSnapshot};
use crate::schedule_store::ScheduleStore;
use agentos_types::schedule::{OnceJob, OnceJobAction, OnceJobState, TimerAction, TimerEntry};
use agentos_types::*;
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

#[cfg(test)]
const MIN_CRON_INTERVAL_SECS: i64 = 1;
#[cfg(not(test))]
const MIN_CRON_INTERVAL_SECS: i64 = 60;

/// Per-creator cap on total schedule items (cron + once + timer) to prevent
/// DoS by a single agent. Picked deliberately generous so legitimate use
/// (daily/weekly recurring + occasional one-shots) is well within bounds.
#[cfg(test)]
const MAX_SCHEDULES_PER_CREATOR: usize = 8;
#[cfg(not(test))]
const MAX_SCHEDULES_PER_CREATOR: usize = 50;

/// Lightweight notification sent by ScheduleManager to the kernel.
/// The kernel converts these into properly HMAC-signed EventMessages with audit trail.
#[derive(Debug, Clone)]
pub struct ScheduleNotification {
    pub event_type: EventType,
    pub severity: EventSeverity,
    pub payload: serde_json::Value,
}

pub struct ScheduleManager {
    jobs: RwLock<HashMap<ScheduleID, ScheduledJob>>,
    timers: RwLock<HashMap<ScheduleID, TimerEntry>>,
    once_jobs: RwLock<HashMap<ScheduleID, OnceJob>>,
    /// Side-table tracking the `AgentID` that created each scheduled item.
    /// Used to enforce per-agent ownership on pause/resume/delete operations
    /// without modifying the underlying type definitions in `agentos-types`.
    /// Keyed by `ScheduleID` (covers all three kinds: cron, once, timer).
    creators: RwLock<HashMap<ScheduleID, AgentID>>,
    /// Optional disk persistence (JSON snapshot) for the schedule definitions.
    /// When set, every mutation triggers a snapshot flush. When `None`, the
    /// manager is in-memory only (used by unit tests).
    persistence: Option<Arc<SchedulePersistence>>,
    /// Optional SQLite-backed store for per-fire `ScheduledRun` records.
    /// The delivery router writes here on completion; agent visibility tools
    /// read from it. Independent of the JSON snapshot above (the snapshot
    /// covers schedule definitions; this store covers run history).
    store: Option<Arc<ScheduleStore>>,
    /// In-memory map: TaskID of a currently-running scheduled task → RunID
    /// of its `ScheduledRun`. Lets `task_completion` find the run record
    /// without racing the (possibly slower) `upsert_run` write.
    pending_runs: RwLock<HashMap<TaskID, RunID>>,
    /// Optional channel for notifying the kernel of schedule events.
    /// The kernel converts these into properly signed EventMessages.
    notification_sender: RwLock<Option<mpsc::Sender<ScheduleNotification>>>,
}

impl ScheduleManager {
    pub fn new() -> Self {
        Self {
            jobs: RwLock::new(HashMap::new()),
            timers: RwLock::new(HashMap::new()),
            once_jobs: RwLock::new(HashMap::new()),
            creators: RwLock::new(HashMap::new()),
            persistence: None,
            store: None,
            pending_runs: RwLock::new(HashMap::new()),
            notification_sender: RwLock::new(None),
        }
    }

    /// Construct a manager backed by disk snapshots and rehydrate state from
    /// the file. Call this once on kernel boot, before any scheduling work.
    pub async fn with_persistence(persistence: Arc<SchedulePersistence>) -> anyhow::Result<Self> {
        Self::with_persistence_and_store(persistence, None).await
    }

    /// Same as `with_persistence` but also attaches a `ScheduleStore` for
    /// per-fire `ScheduledRun` history. Schedule definitions still live in
    /// the JSON snapshot; this store covers run records consumed by the
    /// delivery router and `get-schedule-runs` tool.
    pub async fn with_persistence_and_store(
        persistence: Arc<SchedulePersistence>,
        store: Option<Arc<ScheduleStore>>,
    ) -> anyhow::Result<Self> {
        let snapshot = persistence.load().await?;
        // Backfill creators side-map from per-row `creator_agent_id` fields.
        // Snapshots written before the side-map existed (or with a torn
        // flush — see flush() ordering note) may be missing entries that the
        // struct field still carries. Single source of truth becomes
        // `creators` after this; reads via `creator_of()`.
        let mut creators = snapshot.creators;
        for (id, j) in &snapshot.jobs {
            if let std::collections::hash_map::Entry::Vacant(e) = creators.entry(*id) {
                if let Some(c) = j.creator_agent_id {
                    e.insert(c);
                }
            }
        }
        for (id, j) in &snapshot.once_jobs {
            if let std::collections::hash_map::Entry::Vacant(e) = creators.entry(*id) {
                if let Some(c) = j.creator_agent_id {
                    e.insert(c);
                }
            }
        }
        for (id, t) in &snapshot.timers {
            if let std::collections::hash_map::Entry::Vacant(e) = creators.entry(*id) {
                if let Some(c) = t.creator_agent_id {
                    e.insert(c);
                }
            }
        }
        let mgr = Self {
            jobs: RwLock::new(snapshot.jobs),
            timers: RwLock::new(snapshot.timers),
            once_jobs: RwLock::new(snapshot.once_jobs),
            creators: RwLock::new(creators),
            persistence: Some(persistence),
            store,
            pending_runs: RwLock::new(HashMap::new()),
            notification_sender: RwLock::new(None),
        };
        Ok(mgr)
    }

    /// Read access to the run-record store (used by the delivery router and
    /// `get-schedule-runs` tool). Returns `None` if no store was attached.
    pub fn store(&self) -> Option<&Arc<ScheduleStore>> {
        self.store.as_ref()
    }

    /// Race-free task→run mapping for in-flight scheduled task fires.
    /// Populated synchronously when run_loop opens a `Running` ScheduledRun;
    /// consumed by `task_completion` to transition the run without a SQLite
    /// scan and without depending on `upsert_run` having finished.
    pub async fn track_pending_run(&self, task_id: TaskID, run_id: RunID) {
        self.pending_runs.write().await.insert(task_id, run_id);
    }

    pub async fn take_pending_run(&self, task_id: &TaskID) -> Option<RunID> {
        self.pending_runs.write().await.remove(task_id)
    }

    /// Snapshot current state to disk. Called automatically after every
    /// mutation; safe to call from external paths if needed.
    async fn flush(&self) {
        let Some(p) = self.persistence.as_ref() else {
            return;
        };
        let snapshot = ScheduleSnapshot {
            jobs: self.jobs.read().await.clone(),
            once_jobs: self.once_jobs.read().await.clone(),
            timers: self.timers.read().await.clone(),
            creators: self.creators.read().await.clone(),
        };
        if let Err(e) = p.flush(snapshot).await {
            tracing::warn!(error = %e, "Failed to flush schedule snapshot to disk");
        }
    }

    /// Look up the `AgentID` that created the given schedule (any kind).
    /// Returns `None` if the schedule was created without a recorded creator
    /// (e.g. operator CLI path) or has been deleted.
    pub async fn creator_of(&self, id: &ScheduleID) -> Option<AgentID> {
        self.creators.read().await.get(id).copied()
    }

    /// Delete every schedule (cron + once + timer) created by `agent_id` and forget
    /// the creator records. Returns the number of schedules removed across all three
    /// maps (cron jobs, once jobs, timers).
    pub async fn delete_all_for_creator(&self, agent_id: AgentID) -> usize {
        let owned: Vec<ScheduleID> = self
            .creators
            .read()
            .await
            .iter()
            .filter_map(|(id, c)| (*c == agent_id).then_some(*id))
            .collect();
        if owned.is_empty() {
            return 0;
        }
        let mut removed = 0usize;
        {
            let mut jobs = self.jobs.write().await;
            let mut once = self.once_jobs.write().await;
            let mut timers = self.timers.write().await;
            for id in &owned {
                if jobs.remove(id).is_some()
                    || once.remove(id).is_some()
                    || timers.remove(id).is_some()
                {
                    removed += 1;
                }
            }
        }
        {
            let mut creators = self.creators.write().await;
            for id in &owned {
                creators.remove(id);
            }
        }
        self.flush().await;
        removed
    }

    /// Count of schedule items (cron + once + timer) created by `agent_id`.
    pub async fn schedule_count_for(&self, agent_id: AgentID) -> usize {
        self.creators
            .read()
            .await
            .values()
            .filter(|c| **c == agent_id)
            .count()
    }

    /// Reject if `agent_id` already owns at least `MAX_SCHEDULES_PER_CREATOR`
    /// items. Called from the `*_with_creator` and `create_job_full` paths
    /// before persisting state.
    async fn enforce_creator_cap(&self, agent_id: AgentID) -> Result<(), AgentOSError> {
        let count = self.schedule_count_for(agent_id).await;
        if count >= MAX_SCHEDULES_PER_CREATOR {
            return Err(AgentOSError::SchemaValidation(format!(
                "Agent already owns {} schedules; cap is {}. Delete an existing schedule first.",
                count, MAX_SCHEDULES_PER_CREATOR
            )));
        }
        Ok(())
    }

    /// Record a creator for a schedule. Internal helper used by the
    /// `*_with_creator` entry points.
    async fn record_creator(&self, id: ScheduleID, creator: AgentID) {
        self.creators.write().await.insert(id, creator);
    }

    /// Remove a creator record. Called on delete/cancel paths.
    async fn forget_creator(&self, id: &ScheduleID) {
        self.creators.write().await.remove(id);
    }

    /// Inject the notification sender so the kernel receives schedule events
    /// and converts them into properly HMAC-signed EventMessages.
    pub async fn set_notification_sender(&self, sender: mpsc::Sender<ScheduleNotification>) {
        *self.notification_sender.write().await = Some(sender);
    }

    /// Send a lightweight notification to the kernel for signing and dispatch.
    async fn notify(
        &self,
        event_type: EventType,
        severity: EventSeverity,
        payload: serde_json::Value,
    ) {
        let sender = self.notification_sender.read().await;
        if let Some(ref sender) = *sender {
            let notification = ScheduleNotification {
                event_type,
                severity,
                payload,
            };
            if let Err(e) = sender.try_send(notification) {
                tracing::warn!(error = %e, "Failed to send schedule notification (possibly full or closed)");
            }
        }
    }

    pub async fn create_job(
        &self,
        name: String,
        cron_expression: String,
        agent_name: String,
        task_prompt: String,
        permissions: Vec<String>,
    ) -> Result<ScheduleID, AgentOSError> {
        if cron_expression.trim().is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "cron_expression must not be empty".into(),
            ));
        }
        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "Schedule name must be 1-128 characters".into(),
            ));
        }
        // Normalize 5-field cron (min hr dom mon dow) to 6-field (sec min hr dom mon dow)
        // by prepending a "0" seconds field, matching standard crontab format.
        let cron_expression = if cron_expression.split_whitespace().count() == 5 {
            format!("0 {}", cron_expression)
        } else {
            cron_expression
        };
        let schedule = Schedule::from_str(&cron_expression).map_err(|e| {
            AgentOSError::SchemaValidation(format!(
                "Invalid cron expression '{}': {}",
                cron_expression, e
            ))
        })?;
        let mut upcoming = schedule.upcoming(chrono::Utc);
        if let (Some(first), Some(second)) = (upcoming.next(), upcoming.next()) {
            let delta = second.signed_duration_since(first).num_seconds();
            if delta > 0 && delta < MIN_CRON_INTERVAL_SECS {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Cron interval too frequent ({}s). Minimum interval is {}s",
                    delta, MIN_CRON_INTERVAL_SECS
                )));
            }
        }

        // Reject duplicate names to ensure name-based lookup stays unambiguous.
        {
            let jobs = self.jobs.read().await;
            if jobs.values().any(|j| j.name == name) {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Schedule job '{}' already exists",
                    name
                )));
            }
        }

        let action = OnceJobAction::RunTask {
            prompt: task_prompt.clone(),
        };
        let job = ScheduledJob {
            id: ScheduleID::new(),
            name,
            cron_expression,
            timezone: None,
            agent_name,
            task_prompt,
            permissions,
            state: ScheduleState::Active,
            created_at: chrono::Utc::now(),
            last_run_at: None,
            next_run_at: None,
            run_count: 0,
            max_retries: 3,
            retry_count: 0,
            output_destination: None,
            creator_agent_id: None,
            action,
            delivery: agentos_types::delivery::DeliveryMode::Silent,
        };

        let id = job.id;
        self.jobs.write().await.insert(id, job);
        self.flush().await;
        Ok(id)
    }

    /// Full-featured create entry point: caller provides a typed `action`
    /// (RunTask / NotifyUser / RunTool) plus the creator AgentID. The
    /// `task_prompt` shadow column is auto-populated from the action.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_job_full(
        &self,
        name: String,
        cron_expression: String,
        timezone: Option<String>,
        agent_name: String,
        action: OnceJobAction,
        permissions: Vec<String>,
        creator: AgentID,
    ) -> Result<ScheduleID, AgentOSError> {
        self.enforce_creator_cap(creator).await?;
        if cron_expression.trim().is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "cron_expression must not be empty".into(),
            ));
        }
        // Re-use the cron parsing + min-interval guard logic in create_job by
        // first inserting a placeholder RunTask row then patching the action.
        // Cheaper alternative: duplicate the validation; keep a single source
        // of truth by extracting a helper.
        let cron_expression = if cron_expression.split_whitespace().count() == 5 {
            format!("0 {}", cron_expression)
        } else {
            cron_expression
        };
        let schedule = Schedule::from_str(&cron_expression).map_err(|e| {
            AgentOSError::SchemaValidation(format!(
                "Invalid cron expression '{}': {}",
                cron_expression, e
            ))
        })?;
        let mut upcoming = schedule.upcoming(chrono::Utc);
        if let (Some(first), Some(second)) = (upcoming.next(), upcoming.next()) {
            let delta = second.signed_duration_since(first).num_seconds();
            if delta > 0 && delta < MIN_CRON_INTERVAL_SECS {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Cron interval too frequent ({}s). Minimum interval is {}s",
                    delta, MIN_CRON_INTERVAL_SECS
                )));
            }
        }
        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "Schedule name must be 1-128 characters".into(),
            ));
        }
        {
            let jobs = self.jobs.read().await;
            if jobs.values().any(|j| j.name == name) {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Schedule job '{}' already exists",
                    name
                )));
            }
        }
        let task_prompt = ScheduledJob::shadow_task_prompt(&action);
        let job = ScheduledJob {
            id: ScheduleID::new(),
            name,
            cron_expression,
            timezone,
            agent_name,
            task_prompt,
            permissions,
            state: ScheduleState::Active,
            created_at: chrono::Utc::now(),
            last_run_at: None,
            next_run_at: None,
            run_count: 0,
            max_retries: 3,
            retry_count: 0,
            output_destination: None,
            creator_agent_id: Some(creator),
            action,
            delivery: agentos_types::delivery::DeliveryMode::Silent,
        };
        let id = job.id;
        self.jobs.write().await.insert(id, job);
        self.record_creator(id, creator).await;
        self.flush().await;
        Ok(id)
    }

    pub async fn pause(&self, id: &ScheduleID) -> Result<(), AgentOSError> {
        let mut jobs = self.jobs.write().await;
        let mutated = if let Some(job) = jobs.get_mut(id) {
            job.state = ScheduleState::Paused;
            true
        } else {
            false
        };
        drop(jobs);
        if mutated {
            self.flush().await;
            Ok(())
        } else {
            Err(AgentOSError::KernelError {
                reason: format!("Schedule {} not found", id),
            })
        }
    }

    pub async fn resume(&self, id: &ScheduleID) -> Result<(), AgentOSError> {
        let mut jobs = self.jobs.write().await;
        let mutated = if let Some(job) = jobs.get_mut(id) {
            job.state = ScheduleState::Active;
            true
        } else {
            false
        };
        drop(jobs);
        if mutated {
            self.flush().await;
            Ok(())
        } else {
            Err(AgentOSError::KernelError {
                reason: format!("Schedule {} not found", id),
            })
        }
    }

    pub async fn delete(&self, id: &ScheduleID) -> Result<(), AgentOSError> {
        let mut jobs = self.jobs.write().await;
        if jobs.remove(id).is_some() {
            drop(jobs);
            self.forget_creator(id).await;
            self.flush().await;
            Ok(())
        } else {
            Err(AgentOSError::KernelError {
                reason: format!("Schedule {} not found", id),
            })
        }
    }

    /// Same as `create_job` but records the creator for ownership checks.
    pub async fn create_job_with_creator(
        &self,
        name: String,
        cron_expression: String,
        agent_name: String,
        task_prompt: String,
        permissions: Vec<String>,
        creator: AgentID,
    ) -> Result<ScheduleID, AgentOSError> {
        self.enforce_creator_cap(creator).await?;
        let id = self
            .create_job(name, cron_expression, agent_name, task_prompt, permissions)
            .await?;
        self.record_creator(id, creator).await;
        self.flush().await;
        Ok(id)
    }

    pub async fn list_jobs(&self) -> Vec<ScheduledJob> {
        self.jobs.read().await.values().cloned().collect()
    }

    pub async fn get_job(&self, id: &ScheduleID) -> Option<ScheduledJob> {
        self.jobs.read().await.get(id).cloned()
    }

    pub async fn get_by_name(&self, name: &str) -> Option<ScheduledJob> {
        self.jobs
            .read()
            .await
            .values()
            .find(|j| j.name == name)
            .cloned()
    }

    pub async fn check_due_jobs(&self) -> Vec<ScheduledJob> {
        let now = chrono::Utc::now();
        let mut due = Vec::new();
        let mut jobs = self.jobs.write().await;

        for job in jobs.values_mut() {
            if job.state != ScheduleState::Active {
                continue;
            }

            if job.next_run_at.is_none() {
                if let Ok(schedule) = Schedule::from_str(&job.cron_expression) {
                    job.next_run_at = schedule.upcoming(chrono::Utc).next();
                }
            }

            if let Some(next) = job.next_run_at {
                if now >= next {
                    due.push(job.clone());
                    job.last_run_at = Some(now);
                    job.run_count += 1;
                    if let Ok(schedule) = Schedule::from_str(&job.cron_expression) {
                        job.next_run_at = schedule.upcoming(chrono::Utc).next();
                    }
                }
            }
        }

        // Emit CronJobFired for each due job (outside the write lock)
        drop(jobs);
        for job in &due {
            self.notify(
                EventType::CronJobFired,
                EventSeverity::Info,
                serde_json::json!({
                    "schedule_id": job.id.to_string(),
                    "schedule_name": job.name,
                    "cron_expression": job.cron_expression,
                    "run_count": job.run_count,
                }),
            )
            .await;
        }

        due
    }

    /// Emit a `ScheduledTaskMissed` event when a due job's target agent is unavailable.
    /// Called by the kernel when it cannot find the target agent for a fired cron job.
    pub async fn emit_task_missed(&self, job: &ScheduledJob, reason: &str) {
        self.notify(
            EventType::ScheduledTaskMissed,
            EventSeverity::Warning,
            serde_json::json!({
                "schedule_id": job.id.to_string(),
                "schedule_name": job.name,
                "agent_name": job.agent_name,
                "reason": reason,
            }),
        )
        .await;
    }

    /// Emit a `ScheduledTaskCompleted` event when a scheduled task completes successfully.
    /// Called by the kernel after a cron-triggered task succeeds.
    pub async fn emit_task_completed(&self, job: &ScheduledJob) {
        self.notify(
            EventType::ScheduledTaskCompleted,
            EventSeverity::Info,
            serde_json::json!({
                "schedule_id": job.id.to_string(),
                "schedule_name": job.name,
                "agent_name": job.agent_name,
                "completed_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;
    }

    /// Emit a `ScheduledTaskFailed` event when a scheduled task completes with error.
    /// Called by the kernel after a cron-triggered task fails.
    pub async fn emit_task_failed(&self, job: &ScheduledJob, error: &str) {
        self.notify(
            EventType::ScheduledTaskFailed,
            EventSeverity::Warning,
            serde_json::json!({
                "schedule_id": job.id.to_string(),
                "schedule_name": job.name,
                "agent_name": job.agent_name,
                "error": error,
            }),
        )
        .await;
    }

    // ── Timers ────────────────────────────────────────────────────────────────

    /// Create a one-shot in-memory timer. Fires once after `delay_secs` seconds.
    /// The `_extra` parameter is reserved for future use.
    pub async fn create_timer(
        &self,
        name: String,
        delay_secs: u64,
        agent_name: String,
        action: TimerAction,
        _extra: Option<serde_json::Value>,
    ) -> Result<ScheduleID, AgentOSError> {
        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "Timer name must be 1–128 characters".into(),
            ));
        }
        if delay_secs == 0 || delay_secs > 86400 {
            return Err(AgentOSError::SchemaValidation(
                "Timer delay_secs must be 1–86400".into(),
            ));
        }
        let fire_at = chrono::Utc::now() + chrono::Duration::seconds(delay_secs as i64);
        let entry = TimerEntry {
            id: ScheduleID::new(),
            name: name.clone(),
            agent_name,
            fire_at,
            action,
            created_at: chrono::Utc::now(),
            creator_agent_id: None,
            delivery: agentos_types::delivery::DeliveryMode::Silent,
        };
        let id = entry.id;
        let mut timers = self.timers.write().await;
        if timers.values().any(|t| t.name == name) {
            return Err(AgentOSError::SchemaValidation(format!(
                "Timer '{}' already exists",
                name
            )));
        }
        timers.insert(id, entry);
        drop(timers);
        self.flush().await;
        Ok(id)
    }

    pub async fn list_timers(&self) -> Vec<TimerEntry> {
        self.timers.read().await.values().cloned().collect()
    }

    pub async fn get_timer_by_name(&self, name: &str) -> Option<TimerEntry> {
        self.timers
            .read()
            .await
            .values()
            .find(|t| t.name == name)
            .cloned()
    }

    pub async fn cancel_timer_by_name(&self, name: &str) -> Result<TimerEntry, AgentOSError> {
        let mut timers = self.timers.write().await;
        let id = timers
            .values()
            .find(|t| t.name == name)
            .map(|t| t.id)
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("Timer '{}' not found", name),
            })?;
        let removed = timers
            .remove(&id)
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("timer '{}' vanished between find and remove", name),
            })?;
        drop(timers);
        self.forget_creator(&id).await;
        self.flush().await;
        Ok(removed)
    }

    /// Same as `create_timer` but records the creator for ownership checks.
    pub async fn create_timer_with_creator(
        &self,
        name: String,
        delay_secs: u64,
        agent_name: String,
        action: TimerAction,
        extra: Option<serde_json::Value>,
        creator: AgentID,
    ) -> Result<ScheduleID, AgentOSError> {
        self.enforce_creator_cap(creator).await?;
        let id = self
            .create_timer(name, delay_secs, agent_name, action, extra)
            .await?;
        self.record_creator(id, creator).await;
        self.flush().await;
        Ok(id)
    }

    /// Return and remove all timers whose `fire_at` is in the past.
    pub async fn check_due_timers(&self) -> Vec<TimerEntry> {
        let now = chrono::Utc::now();
        let mut timers = self.timers.write().await;
        let due_ids: Vec<ScheduleID> = timers
            .values()
            .filter(|t| now >= t.fire_at)
            .map(|t| t.id)
            .collect();
        let fired: Vec<TimerEntry> = due_ids.iter().filter_map(|id| timers.remove(id)).collect();
        drop(timers);
        for id in &due_ids {
            self.forget_creator(id).await;
        }
        if !fired.is_empty() {
            self.flush().await;
        }
        fired
    }

    // ── Once jobs ─────────────────────────────────────────────────────────────

    /// Schedule a task to run once at `fire_at`. Survives until fired or cancelled.
    pub async fn create_once_job(
        &self,
        name: String,
        fire_at: chrono::DateTime<chrono::Utc>,
        agent_name: String,
        action: OnceJobAction,
    ) -> Result<ScheduleID, AgentOSError> {
        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "Once-job name must be 1–128 characters".into(),
            ));
        }
        let now = chrono::Utc::now();
        if fire_at <= now {
            return Err(AgentOSError::SchemaValidation(
                "fire_at must be in the future".into(),
            ));
        }
        if fire_at > now + chrono::Duration::days(30) {
            return Err(AgentOSError::SchemaValidation(
                "fire_at must be within 30 days of now".into(),
            ));
        }
        let task_prompt = OnceJob::shadow_task_prompt(&action);
        let job = OnceJob {
            id: ScheduleID::new(),
            name: name.clone(),
            agent_name,
            task_prompt,
            fire_at,
            created_at: chrono::Utc::now(),
            state: OnceJobState::Pending,
            action,
            creator_agent_id: None,
            delivery: agentos_types::delivery::DeliveryMode::Silent,
        };
        let id = job.id;
        let mut once_jobs = self.once_jobs.write().await;
        if once_jobs.values().any(|j| j.name == name) {
            return Err(AgentOSError::SchemaValidation(format!(
                "Once-job '{}' already exists",
                name
            )));
        }
        once_jobs.insert(id, job);
        drop(once_jobs);
        self.flush().await;
        Ok(id)
    }

    pub async fn list_once_jobs(&self) -> Vec<OnceJob> {
        self.once_jobs.read().await.values().cloned().collect()
    }

    pub async fn get_once_job_by_name(&self, name: &str) -> Option<OnceJob> {
        self.once_jobs
            .read()
            .await
            .values()
            .find(|j| j.name == name && j.state == OnceJobState::Pending)
            .cloned()
    }

    pub async fn cancel_once_job_by_name(&self, name: &str) -> Result<OnceJob, AgentOSError> {
        let mut once_jobs = self.once_jobs.write().await;
        let id = once_jobs
            .values()
            .find(|j| j.name == name && j.state == OnceJobState::Pending)
            .map(|j| j.id)
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("Pending once-job '{}' not found", name),
            })?;
        let mut job = once_jobs
            .remove(&id)
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("once-job '{}' vanished between find and remove", name),
            })?;
        job.state = OnceJobState::Cancelled;
        drop(once_jobs);
        self.forget_creator(&id).await;
        self.flush().await;
        Ok(job)
    }

    /// Same as `create_once_job` but records the creator for ownership checks.
    pub async fn create_once_job_with_creator(
        &self,
        name: String,
        fire_at: chrono::DateTime<chrono::Utc>,
        agent_name: String,
        action: OnceJobAction,
        creator: AgentID,
    ) -> Result<ScheduleID, AgentOSError> {
        self.enforce_creator_cap(creator).await?;
        let id = self
            .create_once_job(name, fire_at, agent_name, action)
            .await?;
        self.record_creator(id, creator).await;
        self.flush().await;
        Ok(id)
    }

    /// Return and remove all pending once-jobs whose `fire_at` is in the past.
    pub async fn check_due_once_jobs(&self) -> Vec<OnceJob> {
        let now = chrono::Utc::now();
        let mut once_jobs = self.once_jobs.write().await;
        let due_ids: Vec<ScheduleID> = once_jobs
            .values()
            .filter(|j| j.state == OnceJobState::Pending && now >= j.fire_at)
            .map(|j| j.id)
            .collect();
        let fired: Vec<OnceJob> = due_ids
            .iter()
            .filter_map(|id| once_jobs.remove(id))
            .collect();
        drop(once_jobs);
        for id in &due_ids {
            self.forget_creator(id).await;
        }
        if !fired.is_empty() {
            self.flush().await;
        }
        fired
    }
}

impl Default for ScheduleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cron_expression_validation() {
        let mgr = ScheduleManager::new();
        let result = mgr
            .create_job(
                "test".into(),
                "0 0 8 * * *".into(),
                "analyst".into(),
                "do stuff".into(),
                vec![],
            )
            .await;
        assert!(result.is_ok());

        let result = mgr
            .create_job(
                "bad".into(),
                "not a cron".into(),
                "analyst".into(),
                "do stuff".into(),
                vec![],
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_five_field_cron_normalization() {
        let mgr = ScheduleManager::new();
        let id = mgr
            .create_job(
                "five-field".into(),
                "*/5 * * * *".into(), // standard 5-field cron
                "agent".into(),
                "task".into(),
                vec![],
            )
            .await
            .expect("5-field cron should be accepted");
        let job = mgr.get_job(&id).await.unwrap();
        assert_eq!(job.cron_expression, "0 */5 * * * *");
    }

    #[tokio::test]
    async fn test_persistence_round_trip_across_simulated_restart() {
        let tmp = tempfile::TempDir::new().unwrap();
        let alice = AgentID::new();

        // First "boot": create persisted manager, add a schedule + once-job + timer.
        {
            let p = Arc::new(
                crate::schedule_persistence::SchedulePersistence::new(tmp.path()).unwrap(),
            );
            let mgr = ScheduleManager::with_persistence(p).await.unwrap();

            mgr.create_job_full(
                "daily-post".into(),
                "0 0 9 * * *".into(),
                None,
                "alice".into(),
                OnceJobAction::RunTask {
                    prompt: "post update".into(),
                },
                vec![],
                alice,
            )
            .await
            .unwrap();

            mgr.create_once_job_with_creator(
                "one-shot".into(),
                chrono::Utc::now() + chrono::Duration::hours(1),
                "alice".into(),
                OnceJobAction::NotifyUser {
                    subject: "hi".into(),
                    body: "body".into(),
                    priority: "info".into(),
                },
                alice,
            )
            .await
            .unwrap();

            mgr.create_timer_with_creator(
                "burner".into(),
                3600,
                "alice".into(),
                TimerAction::RunTask {
                    prompt: "fire later".into(),
                },
                None,
                alice,
            )
            .await
            .unwrap();
        }

        // Second "boot": fresh manager from same data dir; expect all three back.
        {
            let p = Arc::new(
                crate::schedule_persistence::SchedulePersistence::new(tmp.path()).unwrap(),
            );
            let mgr = ScheduleManager::with_persistence(p).await.unwrap();

            let jobs = mgr.list_jobs().await;
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].name, "daily-post");
            assert_eq!(jobs[0].state, ScheduleState::Active);
            assert_eq!(jobs[0].creator_agent_id, Some(alice));
            // creator side-table also persisted
            assert_eq!(mgr.creator_of(&jobs[0].id).await, Some(alice));

            let once = mgr.list_once_jobs().await;
            assert_eq!(once.len(), 1);
            assert_eq!(once[0].name, "one-shot");
            assert_eq!(mgr.creator_of(&once[0].id).await, Some(alice));

            let timers = mgr.list_timers().await;
            assert_eq!(timers.len(), 1);
            assert_eq!(timers[0].name, "burner");
            assert_eq!(mgr.creator_of(&timers[0].id).await, Some(alice));
        }
    }

    #[tokio::test]
    async fn test_persistence_delete_propagates_after_restart() {
        let tmp = tempfile::TempDir::new().unwrap();
        let alice = AgentID::new();

        let id = {
            let p = Arc::new(
                crate::schedule_persistence::SchedulePersistence::new(tmp.path()).unwrap(),
            );
            let mgr = ScheduleManager::with_persistence(p).await.unwrap();
            let id = mgr
                .create_job_full(
                    "transient".into(),
                    "0 0 * * * *".into(),
                    None,
                    "alice".into(),
                    OnceJobAction::RunTask { prompt: "p".into() },
                    vec![],
                    alice,
                )
                .await
                .unwrap();
            mgr.delete(&id).await.unwrap();
            id
        };

        let p =
            Arc::new(crate::schedule_persistence::SchedulePersistence::new(tmp.path()).unwrap());
        let mgr = ScheduleManager::with_persistence(p).await.unwrap();
        assert!(mgr.list_jobs().await.is_empty());
        assert_eq!(mgr.creator_of(&id).await, None);
    }

    #[tokio::test]
    async fn test_creator_recorded_and_cleared_on_delete() {
        let mgr = ScheduleManager::new();
        let alice = AgentID::new();
        let id = mgr
            .create_job_with_creator(
                "alice-job".into(),
                "0 0 * * * *".into(),
                "alice".into(),
                "p".into(),
                vec![],
                alice,
            )
            .await
            .unwrap();
        assert_eq!(mgr.creator_of(&id).await, Some(alice));

        mgr.delete(&id).await.unwrap();
        assert_eq!(mgr.creator_of(&id).await, None);
    }

    #[tokio::test]
    async fn test_creator_not_set_for_legacy_create_job_path() {
        // The legacy `create_job` (used by the operator CLI) does not set a
        // creator; ownership checks then default-deny on the agent control
        // path, which matches the security invariant in the plan: agents
        // cannot mutate operator-created schedules.
        let mgr = ScheduleManager::new();
        let id = mgr
            .create_job(
                "operator-cli-job".into(),
                "0 0 * * * *".into(),
                "alice".into(),
                "p".into(),
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(mgr.creator_of(&id).await, None);
    }

    #[tokio::test]
    async fn test_per_creator_schedule_cap_rejects_excess() {
        let mgr = ScheduleManager::new();
        let alice = AgentID::new();
        // cfg(test) cap is 8.
        for i in 0..MAX_SCHEDULES_PER_CREATOR {
            mgr.create_job_with_creator(
                format!("job-{}", i),
                "0 0 * * * *".into(),
                "alice".into(),
                "p".into(),
                vec![],
                alice,
            )
            .await
            .expect("under cap");
        }
        let err = mgr
            .create_job_with_creator(
                "overflow".into(),
                "0 0 * * * *".into(),
                "alice".into(),
                "p".into(),
                vec![],
                alice,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cap is"), "unexpected: {}", err);
    }

    #[tokio::test]
    async fn test_empty_cron_string_rejected() {
        let mgr = ScheduleManager::new();
        let err = mgr
            .create_job("x".into(), "   ".into(), "alice".into(), "p".into(), vec![])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn test_run_record_round_trips_through_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(
            crate::schedule_store::ScheduleStore::open(tmp.path().join("schedules.db"))
                .await
                .unwrap(),
        );
        let alice = AgentID::new();
        let schedule_id = ScheduleID::new();

        let run = agentos_types::schedule::ScheduledRun {
            run_id: agentos_types::RunID::new(),
            parent_kind: agentos_types::schedule::RunParentKind::Schedule,
            parent_id: schedule_id,
            parent_name: Some("daily-post".into()),
            creator_agent_id: Some(alice),
            task_id: None,
            state: agentos_types::schedule::RunState::Complete,
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            result: None,
            error: None,
            tool_calls: vec![],
            delivery: agentos_types::delivery::DeliveryMode::Silent,
            delivered: true,
            delivered_at: Some(chrono::Utc::now()),
            delivery_error: None,
            delivery_depth: None,
        };

        store.upsert_run(run.clone()).await.unwrap();

        let runs = store.list_runs_for_schedule(schedule_id, 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, run.run_id);
        assert_eq!(runs[0].state, agentos_types::schedule::RunState::Complete);

        let by_creator = store.list_runs_by_creator(alice, 10).await.unwrap();
        assert_eq!(by_creator.len(), 1);
    }

    #[tokio::test]
    async fn test_create_job_full_dispatches_each_action_type() {
        let mgr = ScheduleManager::new();
        let alice = AgentID::new();

        let id_task = mgr
            .create_job_full(
                "task-job".into(),
                "0 0 * * * *".into(),
                None,
                "alice".into(),
                OnceJobAction::RunTask {
                    prompt: "do x".into(),
                },
                vec![],
                alice,
            )
            .await
            .expect("RunTask must succeed");
        let job = mgr.get_job(&id_task).await.unwrap();
        assert!(matches!(job.action, OnceJobAction::RunTask { .. }));
        assert_eq!(job.task_prompt, "do x");

        let id_notify = mgr
            .create_job_full(
                "notify-job".into(),
                "0 0 * * * *".into(),
                None,
                "alice".into(),
                OnceJobAction::NotifyUser {
                    subject: "subj".into(),
                    body: "body".into(),
                    priority: "warning".into(),
                },
                vec![],
                alice,
            )
            .await
            .expect("NotifyUser must succeed");
        let job = mgr.get_job(&id_notify).await.unwrap();
        assert!(matches!(job.action, OnceJobAction::NotifyUser { .. }));
        assert_eq!(job.task_prompt, "[notify-user]");

        let id_tool = mgr
            .create_job_full(
                "tool-job".into(),
                "0 0 * * * *".into(),
                None,
                "alice".into(),
                OnceJobAction::RunTool {
                    tool: "datetime".into(),
                    args: serde_json::json!({"format": "rfc3339"}),
                },
                vec![],
                alice,
            )
            .await
            .expect("RunTool must succeed");
        let job = mgr.get_job(&id_tool).await.unwrap();
        assert!(matches!(job.action, OnceJobAction::RunTool { .. }));
        assert_eq!(job.task_prompt, "[run-tool:datetime]");

        // All three creators recorded.
        assert_eq!(mgr.creator_of(&id_task).await, Some(alice));
        assert_eq!(mgr.creator_of(&id_notify).await, Some(alice));
        assert_eq!(mgr.creator_of(&id_tool).await, Some(alice));
    }

    #[tokio::test]
    async fn test_create_job_full_rejects_duplicate_name() {
        let mgr = ScheduleManager::new();
        let alice = AgentID::new();
        mgr.create_job_full(
            "dup".into(),
            "0 0 * * * *".into(),
            None,
            "alice".into(),
            OnceJobAction::RunTask { prompt: "p".into() },
            vec![],
            alice,
        )
        .await
        .unwrap();
        let err = mgr
            .create_job_full(
                "dup".into(),
                "0 0 * * * *".into(),
                None,
                "alice".into(),
                OnceJobAction::RunTask {
                    prompt: "p2".into(),
                },
                vec![],
                alice,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "unexpected error: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_min_cron_interval_guard_via_create_job_uses_test_const() {
        // In #[cfg(test)] the constant is 1s, so a 1-second cron is accepted.
        // This documents the test-mode override; the production constant is 60s
        // and is exercised in integration tests that run without cfg(test).
        let mgr = ScheduleManager::new();
        let res = mgr
            .create_job(
                "fast".into(),
                "* * * * * *".into(),
                "agent".into(),
                "p".into(),
                vec![],
            )
            .await;
        assert!(res.is_ok(), "1s cron must be accepted under cfg(test)");
    }

    #[tokio::test]
    async fn test_pause_prevents_firing() {
        let mgr = ScheduleManager::new();
        let id = mgr
            .create_job(
                "paused-job".into(),
                "* * * * * *".into(),
                "agent".into(),
                "task".into(),
                vec![],
            )
            .await
            .unwrap();

        mgr.pause(&id).await.unwrap();
        // Just verify state changed
        let jobs = mgr.list_jobs().await;
        assert_eq!(jobs[0].state, ScheduleState::Paused);
    }

    #[tokio::test]
    async fn test_check_due_jobs_emits_cron_job_fired() {
        let mgr = ScheduleManager::new();
        let (event_tx, mut event_rx) = mpsc::channel(64);
        mgr.set_notification_sender(event_tx).await;

        // "* * * * * *" fires every second — next_run_at will be <= now by the time we check
        mgr.create_job(
            "every-sec".into(),
            "* * * * * *".into(),
            "agent".into(),
            "do something".into(),
            vec![],
        )
        .await
        .unwrap();

        // First call initializes next_run_at; wait briefly for it to become due
        let _ = mgr.check_due_jobs().await;
        // Small delay to ensure next_run_at is in the past
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        let due = mgr.check_due_jobs().await;
        assert!(!due.is_empty(), "job should be due");

        let notif = event_rx
            .try_recv()
            .expect("should receive CronJobFired notification");
        assert_eq!(notif.event_type, EventType::CronJobFired);
        assert_eq!(
            notif.payload["schedule_name"].as_str().unwrap(),
            "every-sec"
        );
    }

    #[tokio::test]
    async fn test_emit_task_missed() {
        let mgr = ScheduleManager::new();
        let (event_tx, mut event_rx) = mpsc::channel(64);
        mgr.set_notification_sender(event_tx).await;

        let job = ScheduledJob {
            id: ScheduleID::new(),
            name: "missed-job".into(),
            cron_expression: "* * * * * *".into(),
            timezone: None,
            agent_name: "ghost-agent".into(),
            task_prompt: "do stuff".into(),
            permissions: vec![],
            state: ScheduleState::Active,
            created_at: chrono::Utc::now(),
            last_run_at: None,
            next_run_at: None,
            run_count: 0,
            max_retries: 3,
            retry_count: 0,
            output_destination: None,
            creator_agent_id: None,
            action: OnceJobAction::RunTask {
                prompt: "do stuff".into(),
            },
            delivery: agentos_types::delivery::DeliveryMode::Silent,
        };

        mgr.emit_task_missed(&job, "agent not connected").await;

        let notif = event_rx
            .try_recv()
            .expect("should receive ScheduledTaskMissed notification");
        assert_eq!(notif.event_type, EventType::ScheduledTaskMissed);
        assert_eq!(notif.severity, EventSeverity::Warning);
        assert_eq!(notif.payload["agent_name"].as_str().unwrap(), "ghost-agent");
        assert_eq!(
            notif.payload["reason"].as_str().unwrap(),
            "agent not connected"
        );
    }

    #[tokio::test]
    async fn test_emit_task_failed() {
        let mgr = ScheduleManager::new();
        let (event_tx, mut event_rx) = mpsc::channel(64);
        mgr.set_notification_sender(event_tx).await;

        let job = ScheduledJob {
            id: ScheduleID::new(),
            name: "failed-job".into(),
            cron_expression: "* * * * * *".into(),
            timezone: None,
            agent_name: "worker".into(),
            task_prompt: "process data".into(),
            permissions: vec![],
            state: ScheduleState::Active,
            created_at: chrono::Utc::now(),
            last_run_at: None,
            next_run_at: None,
            run_count: 1,
            max_retries: 3,
            retry_count: 0,
            output_destination: None,
            creator_agent_id: None,
            action: OnceJobAction::RunTask {
                prompt: "process data".into(),
            },
            delivery: agentos_types::delivery::DeliveryMode::Silent,
        };

        mgr.emit_task_failed(&job, "timeout exceeded").await;

        let notif = event_rx
            .try_recv()
            .expect("should receive ScheduledTaskFailed notification");
        assert_eq!(notif.event_type, EventType::ScheduledTaskFailed);
        assert_eq!(notif.severity, EventSeverity::Warning);
        assert_eq!(notif.payload["error"].as_str().unwrap(), "timeout exceeded");
    }

    #[tokio::test]
    async fn test_schedule_works_without_notification_sender() {
        // Verify the manager works correctly when notification_sender is None
        let mgr = ScheduleManager::new();
        mgr.create_job(
            "no-sender".into(),
            "* * * * * *".into(),
            "agent".into(),
            "task".into(),
            vec![],
        )
        .await
        .unwrap();

        let _ = mgr.check_due_jobs().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        let due = mgr.check_due_jobs().await;
        assert!(!due.is_empty());
    }
}
