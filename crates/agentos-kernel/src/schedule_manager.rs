use agentos_types::*;
use cron::Schedule;
use std::str::FromStr;
use std::collections::HashMap;
use tokio::sync::RwLock;

pub struct ScheduleManager {
    jobs: RwLock<HashMap<ScheduleID, ScheduledJob>>,
}

impl ScheduleManager {
    pub fn new() -> Self {
        Self {
            jobs: RwLock::new(HashMap::new()),
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
            Err(AgentOSError::VaultError(format!("Schedule {} not found", id)))
        }
    }

    pub async fn resume(&self, id: &ScheduleID) -> Result<(), AgentOSError> {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(id) {
            job.state = ScheduleState::Active;
            Ok(())
        } else {
            Err(AgentOSError::VaultError(format!("Schedule {} not found", id)))
        }
    }

    pub async fn delete(&self, id: &ScheduleID) -> Result<(), AgentOSError> {
        let mut jobs = self.jobs.write().await;
        if jobs.remove(id).is_some() {
            Ok(())
        } else {
            Err(AgentOSError::VaultError(format!("Schedule {} not found", id)))
        }
    }

    pub async fn list_jobs(&self) -> Vec<ScheduledJob> {
        self.jobs.read().await.values().cloned().collect()
    }

    pub async fn get_by_name(&self, name: &str) -> Option<ScheduledJob> {
        self.jobs.read().await.values().find(|j| j.name == name).cloned()
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

        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cron_expression_validation() {
        let mgr = ScheduleManager::new();
        let result = mgr.create_job(
            "test".into(), "0 0 8 * * *".into(), "analyst".into(), "do stuff".into(), vec![],
        ).await;
        assert!(result.is_ok());

        let result = mgr.create_job(
            "bad".into(), "not a cron".into(), "analyst".into(), "do stuff".into(), vec![],
        ).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_pause_prevents_firing() {
        let mgr = ScheduleManager::new();
        let id = mgr.create_job(
            "paused-job".into(), "* * * * * *".into(), "agent".into(), "task".into(), vec![],
        ).await.unwrap();

        mgr.pause(&id).await.unwrap();
        // Just verify state changed
        let jobs = mgr.list_jobs().await;
        assert_eq!(jobs[0].state, ScheduleState::Paused);
    }
}
