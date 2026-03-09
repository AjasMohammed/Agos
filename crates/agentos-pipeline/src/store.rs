use crate::types::{PipelineRun, PipelineRunStatus, StepResult, StepStatus};
use agentos_types::{AgentOSError, RunID};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Persistent pipeline registry backed by SQLite.
pub struct PipelineStore {
    conn: Mutex<Connection>,
}

/// Summary of an installed pipeline for listing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PipelineSummary {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub step_count: usize,
    pub installed_at: String,
}

impl PipelineStore {
    pub fn open(path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open pipeline store: {e}"))
        })?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_tables()?;
        Ok(store)
    }

    fn init_tables(&self) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pipelines (
                name        TEXT PRIMARY KEY,
                version     TEXT NOT NULL,
                definition  TEXT NOT NULL,
                installed_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS pipeline_runs (
                id              TEXT PRIMARY KEY,
                pipeline_name   TEXT NOT NULL REFERENCES pipelines(name) ON DELETE CASCADE,
                input           TEXT NOT NULL,
                status          TEXT NOT NULL,
                step_results    TEXT NOT NULL,
                output          TEXT,
                started_at      TEXT NOT NULL,
                completed_at    TEXT,
                error           TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_runs_pipeline ON pipeline_runs(pipeline_name);
            CREATE INDEX IF NOT EXISTS idx_runs_status ON pipeline_runs(status);

            CREATE TABLE IF NOT EXISTS step_executions (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id      TEXT NOT NULL REFERENCES pipeline_runs(id) ON DELETE CASCADE,
                step_id     TEXT NOT NULL,
                status      TEXT NOT NULL,
                output      TEXT,
                error       TEXT,
                started_at  TEXT,
                completed_at TEXT,
                attempt     INTEGER NOT NULL DEFAULT 1
            );

            CREATE INDEX IF NOT EXISTS idx_step_run ON step_executions(run_id);",
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to init pipeline tables: {e}")))?;
        Ok(())
    }

    // --- Pipeline CRUD ---

    pub fn install_pipeline(
        &self,
        name: &str,
        version: &str,
        yaml: &str,
    ) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO pipelines (name, version, definition, installed_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![name, version, yaml, now],
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to install pipeline: {e}")))?;
        Ok(())
    }

    pub fn get_pipeline_yaml(&self, name: &str) -> Result<String, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT definition FROM pipelines WHERE name = ?1",
            rusqlite::params![name],
            |row| row.get(0),
        )
        .map_err(|_| AgentOSError::KernelError {
            reason: format!("Pipeline not found: {name}"),
        })
    }

    pub fn list_pipelines(&self) -> Result<Vec<PipelineSummary>, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name, version, definition, installed_at FROM pipelines ORDER BY name")
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let version: String = row.get(1)?;
                let yaml: String = row.get(2)?;
                let installed_at: String = row.get(3)?;
                Ok((name, version, yaml, installed_at))
            })
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let mut summaries = Vec::new();
        for row in rows {
            let (name, version, yaml, installed_at) =
                row.map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            let def: Result<crate::definition::PipelineDefinition, _> = serde_yaml::from_str(&yaml);
            let (description, step_count) = match def {
                Ok(d) => (d.description, d.steps.len()),
                Err(_) => (None, 0),
            };

            summaries.push(PipelineSummary {
                name,
                version,
                description,
                step_count,
                installed_at,
            });
        }
        Ok(summaries)
    }

    pub fn remove_pipeline(&self, name: &str) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn
            .execute(
                "DELETE FROM pipelines WHERE name = ?1",
                rusqlite::params![name],
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
        if deleted == 0 {
            return Err(AgentOSError::KernelError {
                reason: format!("Pipeline not found: {name}"),
            });
        }
        Ok(())
    }

    // --- Run tracking ---

    pub fn create_run(&self, run: &PipelineRun) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let step_results_json = serde_json::to_string(&run.step_results)
            .map_err(|e| AgentOSError::Serialization(e.to_string()))?;
        conn.execute(
            "INSERT INTO pipeline_runs (id, pipeline_name, input, status, step_results, output, started_at, completed_at, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                run.id.to_string(),
                run.pipeline_name,
                run.input,
                run.status.to_string(),
                step_results_json,
                run.output,
                run.started_at.to_rfc3339(),
                run.completed_at.map(|t| t.to_rfc3339()),
                run.error,
            ],
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to create run: {e}")))?;
        Ok(())
    }

    pub fn update_run(&self, run: &PipelineRun) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let step_results_json = serde_json::to_string(&run.step_results)
            .map_err(|e| AgentOSError::Serialization(e.to_string()))?;
        conn.execute(
            "UPDATE pipeline_runs SET status = ?1, step_results = ?2, output = ?3, completed_at = ?4, error = ?5 WHERE id = ?6",
            rusqlite::params![
                run.status.to_string(),
                step_results_json,
                run.output,
                run.completed_at.map(|t| t.to_rfc3339()),
                run.error,
                run.id.to_string(),
            ],
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to update run: {e}")))?;
        Ok(())
    }

    pub fn get_run(&self, run_id: &RunID) -> Result<PipelineRun, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, pipeline_name, input, status, step_results, output, started_at, completed_at, error FROM pipeline_runs WHERE id = ?1",
            rusqlite::params![run_id.to_string()],
            |row| {
                let id_str: String = row.get(0)?;
                let pipeline_name: String = row.get(1)?;
                let input: String = row.get(2)?;
                let status_str: String = row.get(3)?;
                let step_results_json: String = row.get(4)?;
                let output: Option<String> = row.get(5)?;
                let started_at_str: String = row.get(6)?;
                let completed_at_str: Option<String> = row.get(7)?;
                let error: Option<String> = row.get(8)?;

                let id = RunID::from_uuid(uuid::Uuid::parse_str(&id_str).unwrap_or_default());
                let status = PipelineRunStatus::from_str(&status_str);
                let step_results: HashMap<String, StepResult> =
                    serde_json::from_str(&step_results_json).unwrap_or_default();
                let started_at = chrono::DateTime::parse_from_rfc3339(&started_at_str)
                    .map(|t| t.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
                let completed_at = completed_at_str.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|t| t.with_timezone(&chrono::Utc))
                        .ok()
                });

                Ok(PipelineRun {
                    id,
                    pipeline_name,
                    input,
                    status,
                    step_results,
                    output,
                    started_at,
                    completed_at,
                    error,
                })
            },
        )
        .map_err(|_| AgentOSError::KernelError {
            reason: format!("Pipeline run not found: {run_id}"),
        })
    }

    // --- Step execution tracking ---

    pub fn record_step_execution(
        &self,
        run_id: &RunID,
        result: &StepResult,
    ) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO step_executions (run_id, step_id, status, output, error, started_at, completed_at, attempt)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                run_id.to_string(),
                result.step_id,
                result.status.to_string(),
                result.output,
                result.error,
                result.started_at.map(|t| t.to_rfc3339()),
                result.completed_at.map(|t| t.to_rfc3339()),
                result.attempt,
            ],
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to record step execution: {e}")))?;
        Ok(())
    }

    pub fn get_step_logs(
        &self,
        run_id: &RunID,
        step_id: &str,
    ) -> Result<Vec<StepResult>, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT step_id, status, output, error, started_at, completed_at, attempt
                 FROM step_executions WHERE run_id = ?1 AND step_id = ?2 ORDER BY attempt",
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let rows = stmt
            .query_map(rusqlite::params![run_id.to_string(), step_id], |row| {
                let step_id: String = row.get(0)?;
                let status_str: String = row.get(1)?;
                let output: Option<String> = row.get(2)?;
                let error: Option<String> = row.get(3)?;
                let started_at_str: Option<String> = row.get(4)?;
                let completed_at_str: Option<String> = row.get(5)?;
                let attempt: u32 = row.get(6)?;

                let started_at = started_at_str.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|t| t.with_timezone(&chrono::Utc))
                        .ok()
                });
                let completed_at = completed_at_str.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|t| t.with_timezone(&chrono::Utc))
                        .ok()
                });
                let duration_ms = match (started_at, completed_at) {
                    (Some(s), Some(e)) => Some((e - s).num_milliseconds() as u64),
                    _ => None,
                };

                Ok(StepResult {
                    step_id,
                    status: StepStatus::from_str(&status_str),
                    output,
                    error,
                    started_at,
                    completed_at,
                    attempt,
                    duration_ms,
                })
            })
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| AgentOSError::StorageError(e.to_string()))?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (PipelineStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = PipelineStore::open(&dir.path().join("pipelines.db")).unwrap();
        (store, dir)
    }

    #[test]
    fn test_install_and_list() {
        let (store, _dir) = test_store();
        store
            .install_pipeline(
                "test-pipe",
                "1.0.0",
                "name: test-pipe\nversion: \"1.0.0\"\nsteps: []",
            )
            .unwrap();
        let list = store.list_pipelines().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test-pipe");
    }

    #[test]
    fn test_remove_pipeline() {
        let (store, _dir) = test_store();
        store
            .install_pipeline(
                "test-pipe",
                "1.0.0",
                "name: test-pipe\nversion: \"1.0.0\"\nsteps: []",
            )
            .unwrap();
        store.remove_pipeline("test-pipe").unwrap();
        let list = store.list_pipelines().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_remove_nonexistent_fails() {
        let (store, _dir) = test_store();
        assert!(store.remove_pipeline("nope").is_err());
    }

    #[test]
    fn test_install_replaces_existing() {
        let (store, _dir) = test_store();
        store
            .install_pipeline("pipe", "1.0.0", "name: pipe\nversion: \"1.0.0\"\nsteps: []")
            .unwrap();
        store
            .install_pipeline("pipe", "2.0.0", "name: pipe\nversion: \"2.0.0\"\nsteps: []")
            .unwrap();
        let list = store.list_pipelines().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].version, "2.0.0");
    }

    #[test]
    fn test_get_pipeline_yaml() {
        let (store, _dir) = test_store();
        let yaml = "name: pipe\nversion: \"1.0.0\"\nsteps: []";
        store.install_pipeline("pipe", "1.0.0", yaml).unwrap();
        let retrieved = store.get_pipeline_yaml("pipe").unwrap();
        assert_eq!(retrieved, yaml);
    }

    #[test]
    fn test_get_nonexistent_pipeline_fails() {
        let (store, _dir) = test_store();
        assert!(store.get_pipeline_yaml("nope").is_err());
    }

    #[test]
    fn test_create_and_get_run() {
        let (store, _dir) = test_store();
        // Need a pipeline first for FK
        store
            .install_pipeline("pipe", "1.0.0", "name: pipe\nversion: \"1.0.0\"\nsteps: []")
            .unwrap();

        let run_id = RunID::new();
        let run = PipelineRun {
            id: run_id,
            pipeline_name: "pipe".to_string(),
            input: "test input".to_string(),
            status: PipelineRunStatus::Running,
            step_results: HashMap::new(),
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            error: None,
        };

        store.create_run(&run).unwrap();
        let retrieved = store.get_run(&run_id).unwrap();
        assert_eq!(retrieved.pipeline_name, "pipe");
        assert_eq!(retrieved.input, "test input");
        assert_eq!(retrieved.status, PipelineRunStatus::Running);
    }

    #[test]
    fn test_update_run() {
        let (store, _dir) = test_store();
        store
            .install_pipeline("pipe", "1.0.0", "name: pipe\nversion: \"1.0.0\"\nsteps: []")
            .unwrap();

        let run_id = RunID::new();
        let mut run = PipelineRun {
            id: run_id,
            pipeline_name: "pipe".to_string(),
            input: "test".to_string(),
            status: PipelineRunStatus::Running,
            step_results: HashMap::new(),
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            error: None,
        };
        store.create_run(&run).unwrap();

        run.status = PipelineRunStatus::Complete;
        run.output = Some("final output".to_string());
        run.completed_at = Some(chrono::Utc::now());
        store.update_run(&run).unwrap();

        let retrieved = store.get_run(&run_id).unwrap();
        assert_eq!(retrieved.status, PipelineRunStatus::Complete);
        assert_eq!(retrieved.output.as_deref(), Some("final output"));
        assert!(retrieved.completed_at.is_some());
    }

    #[test]
    fn test_record_and_get_step_logs() {
        let (store, _dir) = test_store();
        store
            .install_pipeline("pipe", "1.0.0", "name: pipe\nversion: \"1.0.0\"\nsteps: []")
            .unwrap();

        let run_id = RunID::new();
        let run = PipelineRun {
            id: run_id,
            pipeline_name: "pipe".to_string(),
            input: "test".to_string(),
            status: PipelineRunStatus::Running,
            step_results: HashMap::new(),
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            error: None,
        };
        store.create_run(&run).unwrap();

        // Record a failed attempt
        let failed_step = StepResult {
            step_id: "step1".to_string(),
            status: StepStatus::Failed,
            output: None,
            error: Some("transient error".to_string()),
            started_at: Some(chrono::Utc::now()),
            completed_at: Some(chrono::Utc::now()),
            attempt: 1,
            duration_ms: Some(100),
        };
        store.record_step_execution(&run_id, &failed_step).unwrap();

        // Record a successful retry
        let success_step = StepResult {
            step_id: "step1".to_string(),
            status: StepStatus::Complete,
            output: Some("success output".to_string()),
            error: None,
            started_at: Some(chrono::Utc::now()),
            completed_at: Some(chrono::Utc::now()),
            attempt: 2,
            duration_ms: Some(200),
        };
        store.record_step_execution(&run_id, &success_step).unwrap();

        let logs = store.get_step_logs(&run_id, "step1").unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].attempt, 1);
        assert_eq!(logs[0].status, StepStatus::Failed);
        assert_eq!(logs[1].attempt, 2);
        assert_eq!(logs[1].status, StepStatus::Complete);
        assert_eq!(logs[1].output.as_deref(), Some("success output"));
    }

    #[test]
    fn test_get_nonexistent_run_fails() {
        let (store, _dir) = test_store();
        let fake_id = RunID::new();
        assert!(store.get_run(&fake_id).is_err());
    }
}
