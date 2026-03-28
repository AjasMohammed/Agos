use crate::task_trace::{
    ActiveTrace, IterationBuilder, PermissionCheckTrace, TaskTrace, TaskTraceSummary, ToolCallTrace,
};
use agentos_types::{AgentID, AgentOSError, TaskID};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

/// Manages in-memory task trace accumulation and persists completed traces to
/// a SQLite database at `{data_dir}/traces.db`.
pub struct TraceCollector {
    active: RwLock<HashMap<TaskID, ActiveTrace>>,
    db: Arc<Mutex<rusqlite::Connection>>,
}

impl TraceCollector {
    pub fn new(db_path: &Path) -> Result<Self, AgentOSError> {
        let conn = rusqlite::Connection::open(db_path).map_err(|e| {
            AgentOSError::StorageError(format!("TraceCollector DB open failed: {e}"))
        })?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_traces (
                task_id      TEXT PRIMARY KEY,
                agent_id     TEXT NOT NULL,
                started_at   TEXT NOT NULL,
                finished_at  TEXT,
                status       TEXT NOT NULL,
                trace_json   TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_traces_agent ON task_traces(agent_id, started_at DESC);",
        )
        .map_err(|e| {
            AgentOSError::StorageError(format!("TraceCollector schema init failed: {e}"))
        })?;

        Ok(Self {
            active: RwLock::new(HashMap::new()),
            db: Arc::new(Mutex::new(conn)),
        })
    }

    /// Register a new task. Must be called before any other method for a given task.
    pub async fn start_task(&self, task_id: TaskID, agent_id: AgentID, prompt: &str) {
        let trace = ActiveTrace {
            agent_id,
            started_at: Utc::now(),
            prompt_preview: prompt.chars().take(200).collect(),
            completed_iterations: Vec::new(),
            current_iter: None,
            snapshot_ids: Vec::new(),
            total_cost_usd: 0.0,
        };
        self.active.write().await.insert(task_id, trace);
    }

    /// Open a new iteration. Closes and stores the previous iteration if one
    /// is in progress. Call this immediately after the LLM inference result is
    /// available so tokens and stop_reason can be recorded.
    #[allow(clippy::too_many_arguments)]
    pub async fn begin_iteration(
        &self,
        task_id: &TaskID,
        iteration: u32,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        stop_reason: &str,
        snapshot_id: Option<String>,
    ) {
        let mut active = self.active.write().await;
        let trace = match active.get_mut(task_id) {
            Some(t) => t,
            None => return,
        };

        // Close the previous iteration if open.
        if let Some(prev) = trace.current_iter.take() {
            trace.completed_iterations.push(prev.build());
        }

        trace.current_iter = Some(IterationBuilder {
            iteration,
            started_at: Utc::now(),
            model: model.to_string(),
            input_tokens,
            output_tokens,
            stop_reason: stop_reason.to_string(),
            tool_calls: Vec::new(),
            snapshot_id,
        });
    }

    /// Append a tool call to the current iteration.
    /// If no iteration is open (shouldn't happen in practice), the call is silently discarded.
    pub async fn record_tool_call(&self, task_id: &TaskID, tool_call: ToolCallTrace) {
        let mut active = self.active.write().await;
        if let Some(trace) = active.get_mut(task_id) {
            if let Some(iter) = trace.current_iter.as_mut() {
                iter.tool_calls.push(tool_call);
            }
        }
    }

    /// Record a snapshot reference taken at any point during task execution.
    pub async fn record_snapshot(&self, task_id: &TaskID, snapshot_ref: &str) {
        let mut active = self.active.write().await;
        if let Some(trace) = active.get_mut(task_id) {
            trace.snapshot_ids.push(snapshot_ref.to_string());
        }
    }

    /// Update the cumulative cost for the active trace.
    pub async fn update_cost(&self, task_id: &TaskID, cost_usd: f64) {
        let mut active = self.active.write().await;
        if let Some(trace) = active.get_mut(task_id) {
            trace.total_cost_usd = cost_usd;
        }
    }

    /// Finalise the trace, persist it to SQLite, and remove it from the active map.
    /// Returns the assembled TaskTrace, or `None` if no active trace exists.
    pub async fn finish_task(
        &self,
        task_id: &TaskID,
        status: &str,
        finished_at: DateTime<Utc>,
    ) -> Option<TaskTrace> {
        let mut active = self.active.write().await;
        let mut trace = active.remove(task_id)?;

        // Close any open iteration.
        if let Some(last_iter) = trace.current_iter.take() {
            trace.completed_iterations.push(last_iter.build());
        }

        let (total_in, total_out) = trace
            .completed_iterations
            .iter()
            .fold((0u64, 0u64), |acc, it| {
                (acc.0 + it.input_tokens, acc.1 + it.output_tokens)
            });

        let task_trace = TaskTrace {
            task_id: *task_id,
            agent_id: trace.agent_id,
            started_at: trace.started_at,
            finished_at: Some(finished_at),
            status: status.to_string(),
            prompt_preview: trace.prompt_preview,
            iterations: trace.completed_iterations,
            snapshot_ids: trace.snapshot_ids,
            total_input_tokens: total_in,
            total_output_tokens: total_out,
            total_cost_usd: trace.total_cost_usd,
        };

        // Persist synchronously (awaited) so get_trace callers always find the record.
        let db = self.db.clone();
        let trace_clone = task_trace.clone();
        let task_id_str = task_id.to_string();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            let json = match serde_json::to_string(&trace_clone) {
                Ok(j) => j,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to serialize task trace");
                    return;
                }
            };
            let conn = match db.lock() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "TraceCollector DB lock poisoned");
                    return;
                }
            };
            if let Err(e) = conn.execute(
                "INSERT OR REPLACE INTO task_traces
                 (task_id, agent_id, started_at, finished_at, status, trace_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    task_id_str,
                    trace_clone.agent_id.to_string(),
                    trace_clone.started_at.to_rfc3339(),
                    trace_clone.finished_at.map(|dt| dt.to_rfc3339()),
                    trace_clone.status,
                    json,
                ],
            ) {
                tracing::warn!(error = %e, task_id = task_id_str, "Failed to persist task trace");
            }
        })
        .await
        {
            tracing::warn!(error = %e, "TraceCollector: persist blocking task panicked");
        }

        Some(task_trace)
    }

    /// Retrieve a completed trace from the SQLite store.
    pub async fn get_trace(&self, task_id: &TaskID) -> Result<Option<TaskTrace>, AgentOSError> {
        let db = self.db.clone();
        let id_str = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("TraceCollector DB lock poisoned".into())
            })?;
            let mut stmt = conn
                .prepare("SELECT trace_json FROM task_traces WHERE task_id = ?1")
                .map_err(|e| {
                    AgentOSError::StorageError(format!("TraceCollector query failed: {e}"))
                })?;

            let result: Option<String> = stmt
                .query_row(params![id_str], |row| row.get::<_, String>(0))
                .optional()
                .map_err(|e| {
                    AgentOSError::StorageError(format!("TraceCollector row fetch failed: {e}"))
                })?;

            match result {
                Some(json) => serde_json::from_str::<TaskTrace>(&json)
                    .map(Some)
                    .map_err(|e| {
                        AgentOSError::StorageError(format!(
                            "TraceCollector deserialize failed: {e}"
                        ))
                    }),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| {
            AgentOSError::StorageError(format!("TraceCollector spawn_blocking failed: {e}"))
        })?
    }

    /// List recent traces ordered by start time descending.
    /// If `agent_id` is provided, only traces for that agent are returned.
    pub async fn list_traces(
        &self,
        agent_id: Option<AgentID>,
        limit: u32,
    ) -> Result<Vec<TaskTraceSummary>, AgentOSError> {
        let limit = limit.min(1000);
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("TraceCollector DB lock poisoned".into())
            })?;

            // Collect row JSON strings; use explicit for-loop to avoid rusqlite
            // MappedRows lifetime issues when stmt lives inside an if/else branch.
            let mut rows: Vec<String> = Vec::new();
            if let Some(ref aid) = agent_id {
                let mut stmt = conn
                    .prepare(
                        "SELECT trace_json FROM task_traces \
                         WHERE agent_id = ?1 ORDER BY started_at DESC LIMIT ?2",
                    )
                    .map_err(|e| {
                        AgentOSError::StorageError(format!("TraceCollector list query failed: {e}"))
                    })?;
                for r in stmt
                    .query_map(params![aid.to_string(), limit], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(|e| {
                        AgentOSError::StorageError(format!("TraceCollector query_map failed: {e}"))
                    })?
                {
                    match r {
                        Ok(v) => rows.push(v),
                        Err(e) => {
                            tracing::warn!(error = %e, "TraceCollector: failed to read trace row")
                        }
                    }
                }
            } else {
                let mut stmt = conn
                    .prepare("SELECT trace_json FROM task_traces ORDER BY started_at DESC LIMIT ?1")
                    .map_err(|e| {
                        AgentOSError::StorageError(format!("TraceCollector list query failed: {e}"))
                    })?;
                for r in stmt
                    .query_map(params![limit], |row| row.get::<_, String>(0))
                    .map_err(|e| {
                        AgentOSError::StorageError(format!("TraceCollector query_map failed: {e}"))
                    })?
                {
                    match r {
                        Ok(v) => rows.push(v),
                        Err(e) => {
                            tracing::warn!(error = %e, "TraceCollector: failed to read trace row")
                        }
                    }
                }
            };

            let mut summaries = Vec::with_capacity(rows.len());
            for json in rows {
                match serde_json::from_str::<TaskTrace>(&json) {
                    Ok(trace) => summaries.push(trace.summary()),
                    Err(e) => {
                        tracing::warn!(error = %e, "TraceCollector: skipping malformed trace");
                    }
                }
            }
            Ok(summaries)
        })
        .await
        .map_err(|e| {
            AgentOSError::StorageError(format!("TraceCollector spawn_blocking failed: {e}"))
        })?
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Maximum byte length stored for any single `input_json` / `output_json` value.
    /// Prevents large file-read results from inflating the trace database.
    const MAX_PAYLOAD_BYTES: usize = 4096;

    /// Truncate a JSON value to a string of at most `MAX_PAYLOAD_BYTES` bytes.
    /// If truncation is needed the value is replaced with a string summary.
    fn truncate_payload(v: serde_json::Value) -> serde_json::Value {
        let s = v.to_string();
        if s.len() <= Self::MAX_PAYLOAD_BYTES {
            v
        } else {
            // Safe: we truncate at a char boundary by converting via chars
            let truncated: String = s.chars().take(Self::MAX_PAYLOAD_BYTES).collect();
            serde_json::Value::String(format!("{truncated}…<truncated>"))
        }
    }

    /// Build a `ToolCallTrace` for a *denied* tool call (permission check failed
    /// before execution started).
    pub fn denied_tool_call(
        tool_name: &str,
        input_json: serde_json::Value,
        deny_reason: &str,
    ) -> ToolCallTrace {
        ToolCallTrace {
            tool_name: tool_name.to_string(),
            input_json: Self::truncate_payload(input_json),
            output_json: None,
            error: Some(format!("Permission denied: {deny_reason}")),
            duration_ms: 0,
            permission_check: PermissionCheckTrace {
                granted: false,
                deny_reason: Some(deny_reason.to_string()),
            },
            injection_score: None,
            snapshot_ref: None,
        }
    }

    /// Build a `ToolCallTrace` for a *completed* (success) tool call.
    pub fn success_tool_call(
        tool_name: &str,
        input_json: serde_json::Value,
        output_json: serde_json::Value,
        duration_ms: u64,
        snapshot_ref: Option<String>,
        injection_score: Option<f32>,
    ) -> ToolCallTrace {
        ToolCallTrace {
            tool_name: tool_name.to_string(),
            input_json: Self::truncate_payload(input_json),
            output_json: Some(Self::truncate_payload(output_json)),
            error: None,
            duration_ms,
            permission_check: PermissionCheckTrace {
                granted: true,
                deny_reason: None,
            },
            injection_score,
            snapshot_ref,
        }
    }

    /// Build a `ToolCallTrace` for a *failed* (execution error) tool call.
    pub fn failed_tool_call(
        tool_name: &str,
        input_json: serde_json::Value,
        error: &str,
        duration_ms: u64,
        snapshot_ref: Option<String>,
    ) -> ToolCallTrace {
        ToolCallTrace {
            tool_name: tool_name.to_string(),
            input_json: Self::truncate_payload(input_json),
            output_json: None,
            error: Some(error.to_string()),
            duration_ms,
            permission_check: PermissionCheckTrace {
                granted: true, // permission was granted; execution itself failed
                deny_reason: None,
            },
            injection_score: None,
            snapshot_ref,
        }
    }
}
