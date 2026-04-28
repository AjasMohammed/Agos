use agentos_types::schedule::{OnceJob, OnceJobState, TimerAction, TimerEntry};
use agentos_types::*;
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

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
            notification_sender: RwLock::new(None),
        }
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
        // Normalize 5-field cron (min hr dom mon dow) to 6-field (sec min hr dom mon dow)
        // by prepending a "0" seconds field, matching standard crontab format.
        let cron_expression = if cron_expression.split_whitespace().count() == 5 {
            format!("0 {}", cron_expression)
        } else {
            cron_expression
        };
        Schedule::from_str(&cron_expression).map_err(|e| {
            AgentOSError::SchemaValidation(format!(
                "Invalid cron expression '{}': {}",
                cron_expression, e
            ))
        })?;

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
        };

        let id = job.id;
        self.jobs.write().await.insert(id, job);
        Ok(id)
    }

    pub async fn pause(&self, id: &ScheduleID) -> Result<(), AgentOSError> {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(id) {
            job.state = ScheduleState::Paused;
            Ok(())
        } else {
            Err(AgentOSError::KernelError {
                reason: format!("Schedule {} not found", id),
            })
        }
    }

    pub async fn resume(&self, id: &ScheduleID) -> Result<(), AgentOSError> {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(id) {
            job.state = ScheduleState::Active;
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
            Ok(())
        } else {
            Err(AgentOSError::KernelError {
                reason: format!("Schedule {} not found", id),
            })
        }
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
        Ok(id)
    }

    pub async fn list_timers(&self) -> Vec<TimerEntry> {
        self.timers.read().await.values().cloned().collect()
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
        timers.remove(&id).ok_or_else(|| AgentOSError::KernelError {
            reason: format!("timer '{}' vanished between find and remove", name),
        })
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
        due_ids
            .into_iter()
            .filter_map(|id| timers.remove(&id))
            .collect()
    }

    // ── Once jobs ─────────────────────────────────────────────────────────────

    /// Schedule a task to run once at `fire_at`. Survives until fired or cancelled.
    pub async fn create_once_job(
        &self,
        name: String,
        fire_at: chrono::DateTime<chrono::Utc>,
        agent_name: String,
        task_prompt: String,
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
        let job = OnceJob {
            id: ScheduleID::new(),
            name: name.clone(),
            agent_name,
            task_prompt,
            fire_at,
            created_at: chrono::Utc::now(),
            state: OnceJobState::Pending,
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
        Ok(id)
    }

    pub async fn list_once_jobs(&self) -> Vec<OnceJob> {
        self.once_jobs.read().await.values().cloned().collect()
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
        Ok(job)
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
        due_ids
            .into_iter()
            .filter_map(|id| once_jobs.remove(&id))
            .collect()
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
