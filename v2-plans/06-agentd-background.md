# Plan 06 — Background Tasks & `agentd` Supervisor

## Goal

Implement `agentd` — the AgentOS equivalent of `systemd` + `cron`. This subsystem manages long-running agent tasks, scheduled (cron) agent jobs, detached background execution, and task lifecycle management (pause, resume, retry, notify).

## Dependencies

- `agentos-types`, `agentos-kernel` (existing)
- `agentos-audit` (existing)
- `agentos-capability` (existing)
- `cron` — cron expression parsing
- `tokio` — timer and spawn infrastructure
- `serde`, `serde_json`
- `chrono`

## New Dependency

```toml
# Add to workspace Cargo.toml
cron = "0.12"
```

## Architecture

```
agentd (kernel subsystem)
    │
    ├── ScheduleManager     — cron expression evaluation, next-run tracking
    ├── BackgroundTaskPool   — detached tasks running independently
    ├── RetryEngine          — exponential backoff retry for failed tasks
    └── NotificationHook     — agent message or log on completion/failure

Main loop (runs every second):
    1. Check all scheduled jobs — is any job due to fire?
    2. For due jobs: create AgentTask, enqueue via TaskScheduler
    3. Check background tasks — any completed or timed out?
    4. Handle retry queue — any tasks ready for retry?
```

## Core Types

```rust
// In agentos-types/src/schedule.rs (new module)

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: ScheduleID,
    pub name: String,
    pub cron_expression: String,
    pub agent_name: String,
    pub task_prompt: String,
    pub permissions: Vec<String>,       // permissions scoped to this job
    pub state: ScheduleState,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub run_count: u64,
    pub max_retries: u32,
    pub retry_count: u32,
    pub output_destination: Option<String>,  // file path for results
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleState {
    Active,
    Paused,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub id: TaskID,
    pub name: String,
    pub agent_name: String,
    pub task_prompt: String,
    pub state: TaskState,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result: Option<serde_json::Value>,
    pub detached: bool,                 // if true, runs independently
}
```

## Schedule Manager

```rust
// In agentos-kernel/src/schedule_manager.rs

use cron::Schedule;
use std::str::FromStr;
use std::collections::HashMap;
use tokio::sync::RwLock;

pub struct ScheduleManager {
    jobs: RwLock<HashMap<ScheduleID, ScheduledJob>>,
}

impl ScheduleManager {
    pub fn new() -> Self;

    /// Create a new scheduled job. Validates the cron expression.
    pub async fn create_job(
        &self,
        name: String,
        cron_expression: String,
        agent_name: String,
        task_prompt: String,
        permissions: Vec<String>,
    ) -> Result<ScheduleID, AgentOSError> {
        // Validate cron expression
        Schedule::from_str(&cron_expression)
            .map_err(|e| AgentOSError::SchemaValidation(
                format!("Invalid cron expression '{}': {}", cron_expression, e)
            ))?;

        let job = ScheduledJob {
            id: ScheduleID::new(),
            name,
            cron_expression,
            agent_name,
            task_prompt,
            permissions,
            state: ScheduleState::Active,
            created_at: chrono::Utc::now(),
            last_run_at: None,
            next_run_at: None, // computed on first tick
            run_count: 0,
            max_retries: 3,
            retry_count: 0,
            output_destination: None,
        };

        let id = job.id;
        self.jobs.write().await.insert(id, job);
        Ok(id)
    }

    /// Pause a scheduled job.
    pub async fn pause(&self, id: &ScheduleID) -> Result<(), AgentOSError>;

    /// Resume a paused job.
    pub async fn resume(&self, id: &ScheduleID) -> Result<(), AgentOSError>;

    /// Delete a scheduled job.
    pub async fn delete(&self, id: &ScheduleID) -> Result<(), AgentOSError>;

    /// List all scheduled jobs.
    pub async fn list_jobs(&self) -> Vec<ScheduledJob>;

    /// Get a job by name.
    pub async fn get_by_name(&self, name: &str) -> Option<ScheduledJob>;

    /// Check all jobs and return any that are due to fire NOW.
    /// Updates last_run_at and next_run_at for fired jobs.
    pub async fn check_due_jobs(&self) -> Vec<ScheduledJob> {
        let now = chrono::Utc::now();
        let mut due = Vec::new();
        let mut jobs = self.jobs.write().await;

        for job in jobs.values_mut() {
            if job.state != ScheduleState::Active {
                continue;
            }

            // Compute next run if not set
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
                    // Compute next run
                    if let Ok(schedule) = Schedule::from_str(&job.cron_expression) {
                        job.next_run_at = schedule.upcoming(chrono::Utc).next();
                    }
                }
            }
        }

        due
    }
}
```

## Background Task Pool

```rust
// In agentos-kernel/src/background_pool.rs

pub struct BackgroundPool {
    tasks: RwLock<HashMap<TaskID, BackgroundTask>>,
}

impl BackgroundPool {
    pub fn new() -> Self;

    /// Register a new background task.
    pub async fn register(&self, task: BackgroundTask);

    /// Mark a background task as completed with result.
    pub async fn complete(&self, task_id: &TaskID, result: serde_json::Value);

    /// Mark a background task as failed.
    pub async fn fail(&self, task_id: &TaskID, error: String);

    /// List all background tasks (running + completed).
    pub async fn list_all(&self) -> Vec<BackgroundTask>;

    /// List running background tasks only.
    pub async fn list_running(&self) -> Vec<BackgroundTask>;

    /// Get logs for a background task (from episodic memory).
    pub async fn get_logs(&self, task_id: &TaskID) -> Vec<String>;

    /// Kill a running background task.
    pub async fn kill(&self, task_id: &TaskID) -> Result<(), AgentOSError>;
}
```

## Kernel Integration

The kernel's `run()` method is updated to spawn the `agentd` loop:

```rust
impl Kernel {
    pub async fn run(self: Arc<Self>) -> Result<(), anyhow::Error> {
        // ... existing spawns (acceptor, executor, timeout_checker) ...

        // Spawn agentd scheduler loop
        let agentd = tokio::spawn({
            let kernel = self.clone();
            async move {
                kernel.agentd_loop().await;
            }
        });

        tokio::select! {
            _ = acceptor => {},
            _ = executor => {},
            _ = timeout_checker => {},
            _ = agentd => {},
        }

        Ok(())
    }

    /// The agentd loop — checks scheduled jobs every second.
    async fn agentd_loop(&self) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;

            let due_jobs = self.schedule_manager.check_due_jobs().await;
            for job in due_jobs {
                tracing::info!(job_name = %job.name, "Firing scheduled job");

                // Audit log
                self.audit.append(AuditEntry {
                    event_type: AuditEventType::ScheduledJobFired,
                    // ...
                }).ok();

                // Create and enqueue the task
                // The task runs as a background task in the pool
                match self.create_background_task(&job).await {
                    Ok(_) => tracing::info!(job_name = %job.name, "Job enqueued"),
                    Err(e) => tracing::error!(job_name = %job.name, "Job failed to enqueue: {}", e),
                }
            }
        }
    }
}
```

## CLI Commands

```bash
# --- Schedule management ---

# Create a recurring job
agentctl schedule create \
  --name "daily-log-summary" \
  --cron "0 8 * * *" \
  --agent analyst \
  --task "Summarize all error logs from the last 24 hours" \
  --permissions "fs.app_logs:r,fs.user_data:w"

# List scheduled jobs
agentctl schedule list
# NAME                  CRON            AGENT      STATE    NEXT RUN         RUNS
# daily-log-summary     0 8 * * *       analyst    active   2026-03-05 08:00  12

# Pause / resume
agentctl schedule pause daily-log-summary
agentctl schedule resume daily-log-summary

# Delete
agentctl schedule delete daily-log-summary

# --- Background tasks ---

# Run a one-shot background task (detached)
agentctl bg run \
  --name "process-uploads" \
  --agent researcher \
  --task "Process all files in /data/incoming" \
  --detach

# List running background tasks
agentctl bg list

# Follow logs for a background task
agentctl bg logs process-uploads --follow

# Kill a running background task
agentctl bg kill process-uploads
```

## New KernelCommand Variants

```rust
pub enum KernelCommand {
    // ... existing ...

    // Schedule
    CreateSchedule { name: String, cron: String, agent_name: String, task: String, permissions: Vec<String> },
    ListSchedules,
    PauseSchedule { name: String },
    ResumeSchedule { name: String },
    DeleteSchedule { name: String },

    // Background
    RunBackground { name: String, agent_name: String, task: String, detach: bool },
    ListBackground,
    GetBackgroundLogs { name: String, follow: bool },
    KillBackground { name: String },
}
```

## New Audit Event Types

```rust
pub enum AuditEventType {
    // ... existing ...
    ScheduledJobCreated,
    ScheduledJobFired,
    ScheduledJobPaused,
    ScheduledJobResumed,
    ScheduledJobDeleted,
    BackgroundTaskStarted,
    BackgroundTaskCompleted,
    BackgroundTaskFailed,
    BackgroundTaskKilled,
}
```

## Tests

```rust
#[tokio::test]
async fn test_cron_expression_validation() {
    let mgr = ScheduleManager::new();
    // Valid expression
    let result = mgr.create_job(
        "test".into(), "0 8 * * *".into(), "analyst".into(), "do stuff".into(), vec![],
    ).await;
    assert!(result.is_ok());

    // Invalid expression
    let result = mgr.create_job(
        "bad".into(), "not a cron".into(), "analyst".into(), "do stuff".into(), vec![],
    ).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_due_job_detection() {
    let mgr = ScheduleManager::new();
    // Create a job with "every second" cron
    mgr.create_job(
        "frequent".into(), "* * * * * *".into(), "agent".into(), "task".into(), vec![],
    ).await.unwrap();

    // Wait 2 seconds
    tokio::time::sleep(Duration::from_secs(2)).await;

    let due = mgr.check_due_jobs().await;
    assert!(!due.is_empty(), "Job should be due");
}

#[tokio::test]
async fn test_pause_prevents_firing() {
    let mgr = ScheduleManager::new();
    let id = mgr.create_job(
        "paused-job".into(), "* * * * * *".into(), "agent".into(), "task".into(), vec![],
    ).await.unwrap();

    mgr.pause(&id).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let due = mgr.check_due_jobs().await;
    assert!(due.is_empty(), "Paused job should not fire");
}

#[tokio::test]
async fn test_background_pool_lifecycle() {
    let pool = BackgroundPool::new();
    let task_id = TaskID::new();

    pool.register(BackgroundTask {
        id: task_id,
        name: "test-bg".into(),
        agent_name: "analyst".into(),
        task_prompt: "do something".into(),
        state: TaskState::Running,
        started_at: chrono::Utc::now(),
        completed_at: None,
        result: None,
        detached: true,
    }).await;

    assert_eq!(pool.list_running().await.len(), 1);

    pool.complete(&task_id, json!({"result": "done"})).await;
    assert_eq!(pool.list_running().await.len(), 0);
}
```

## Verification

```bash
cargo test -p agentos-kernel   # schedule manager + background pool tests
cargo test -p agentos-cli      # new CLI parsing tests
```
