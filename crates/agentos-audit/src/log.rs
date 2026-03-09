use agentos_types::*;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

pub struct AuditLog {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditEventType {
    // Task lifecycle
    TaskCreated,
    TaskStateChanged,
    TaskCompleted,
    TaskFailed,
    TaskTimeout,

    // Intent processing
    IntentReceived,
    IntentRouted,
    IntentCompleted,
    IntentFailed,

    // Capability / Permission
    PermissionGranted,
    PermissionRevoked,
    PermissionDenied,
    TokenIssued,
    TokenExpired,

    // Tool events
    ToolInstalled,
    ToolRemoved,
    ToolExecutionStarted,
    ToolExecutionCompleted,
    ToolExecutionFailed,

    // LLM events
    AgentConnected,
    AgentDisconnected,
    LLMInferenceStarted,
    LLMInferenceCompleted,
    LLMInferenceError,

    // Secret events
    SecretCreated,
    SecretAccessed,
    SecretRevoked,
    SecretRotated,

    // System events
    KernelStarted,
    KernelShutdown,

    // agentd - Schedule
    ScheduledJobCreated,
    ScheduledJobFired,
    ScheduledJobPaused,
    ScheduledJobResumed,
    ScheduledJobDeleted,

    // agentd - Background Tasks
    BackgroundTaskStarted,
    BackgroundTaskCompleted,
    BackgroundTaskFailed,
    BackgroundTaskKilled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditSeverity {
    Info,
    Warn,
    Error,
    Security,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub trace_id: TraceID,
    pub event_type: AuditEventType,
    pub agent_id: Option<AgentID>,
    pub task_id: Option<TaskID>,
    pub tool_id: Option<ToolID>,
    pub details: serde_json::Value,
    pub severity: AuditSeverity,
}

impl AuditLog {
    pub fn open(path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(path)
            .map_err(|e| AgentOSError::VaultError(format!("AuditLog DB open failed: {}", e)))?;

        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;

            CREATE TABLE IF NOT EXISTS audit_log (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp   TEXT NOT NULL,
                trace_id    TEXT NOT NULL,
                event_type  TEXT NOT NULL,
                agent_id    TEXT,
                task_id     TEXT,
                tool_id     TEXT,
                details     TEXT NOT NULL,
                severity    TEXT NOT NULL DEFAULT 'info'
            );

            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_trace_id ON audit_log(trace_id);
            CREATE INDEX IF NOT EXISTS idx_audit_event_type ON audit_log(event_type);
            CREATE INDEX IF NOT EXISTS idx_audit_agent_id ON audit_log(agent_id);
            ",
        )
        .map_err(|e| AgentOSError::VaultError(format!("AuditLog init failed: {}", e)))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn append(&self, entry: AuditEntry) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap();

        let details = serde_json::to_string(&entry.details).map_err(|e| {
            AgentOSError::Serialization(format!("AuditEntry serialize failed: {}", e))
        })?;

        let event_type_str = serde_json::to_string(&entry.event_type).map_err(|e| {
            AgentOSError::Serialization(format!("AuditEventType serialize failed: {}", e))
        })?;
        let event_type_str = event_type_str.trim_matches('"');

        let severity_str = serde_json::to_string(&entry.severity).map_err(|e| {
            AgentOSError::Serialization(format!("AuditSeverity serialize failed: {}", e))
        })?;
        let severity_str = severity_str.trim_matches('"');

        conn.execute(
            "INSERT INTO audit_log (timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.timestamp.to_rfc3339(),
                entry.trace_id.to_string(),
                event_type_str,
                entry.agent_id.map(|id| id.to_string()),
                entry.task_id.map(|id| id.to_string()),
                entry.tool_id.map(|id| id.to_string()),
                details,
                severity_str,
            ],
        ).map_err(|e| AgentOSError::VaultError(format!("AuditLog append failed: {}", e)))?;

        Ok(())
    }

    pub fn query_recent(&self, limit: u32) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity FROM audit_log ORDER BY id DESC LIMIT ?1")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(&mut stmt, params![limit])
    }

    pub fn query_by_trace(&self, trace_id: &TraceID) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity FROM audit_log WHERE trace_id = ?1 ORDER BY id ASC")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(&mut stmt, params![trace_id.to_string()])
    }

    pub fn query_by_type(
        &self,
        event_type: AuditEventType,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap();

        let event_type_str = serde_json::to_string(&event_type).map_err(|e| {
            AgentOSError::Serialization(format!("AuditEventType serialize failed: {}", e))
        })?;
        let event_type_str = event_type_str.trim_matches('"');

        let mut stmt = conn.prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity FROM audit_log WHERE event_type = ?1 ORDER BY id DESC LIMIT ?2")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(&mut stmt, params![event_type_str, limit])
    }

    pub fn query_by_time_range(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity FROM audit_log WHERE timestamp >= ?1 AND timestamp <= ?2 ORDER BY id DESC LIMIT ?3")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(
            &mut stmt,
            params![from.to_rfc3339(), to.to_rfc3339(), limit],
        )
    }

    pub fn count(&self) -> Result<u64, AgentOSError> {
        let conn = self.conn.lock().unwrap();
        let count: u64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Ok(count)
    }

    fn execute_query<P>(
        stmt: &mut rusqlite::Statement,
        params: P,
    ) -> Result<Vec<AuditEntry>, AgentOSError>
    where
        P: rusqlite::Params,
    {
        let rows = stmt
            .query_map(params, |row| {
                let timestamp_str: String = row.get(0)?;
                let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_str)
                    .unwrap()
                    .with_timezone(&chrono::Utc);

                let trace_id_str: String = row.get(1)?;
                let trace_id = TraceID::from_uuid(uuid::Uuid::parse_str(&trace_id_str).unwrap());

                let event_type_str: String = row.get(2)?;
                let event_type: AuditEventType =
                    serde_json::from_str(&format!("\"{}\"", event_type_str)).unwrap();

                let agent_id_str: Option<String> = row.get(3)?;
                let agent_id =
                    agent_id_str.map(|s| AgentID::from_uuid(uuid::Uuid::parse_str(&s).unwrap()));

                let task_id_str: Option<String> = row.get(4)?;
                let task_id =
                    task_id_str.map(|s| TaskID::from_uuid(uuid::Uuid::parse_str(&s).unwrap()));

                let tool_id_str: Option<String> = row.get(5)?;
                let tool_id =
                    tool_id_str.map(|s| ToolID::from_uuid(uuid::Uuid::parse_str(&s).unwrap()));

                let details_str: String = row.get(6)?;
                let details: serde_json::Value = serde_json::from_str(&details_str).unwrap();

                let severity_str: String = row.get(7)?;
                let severity: AuditSeverity =
                    serde_json::from_str(&format!("\"{}\"", severity_str)).unwrap();

                Ok(AuditEntry {
                    timestamp,
                    trace_id,
                    event_type,
                    agent_id,
                    task_id,
                    tool_id,
                    details,
                    severity,
                })
            })
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        let mut entries = Vec::new();
        for row_result in rows {
            entries.push(row_result.map_err(|e| AgentOSError::VaultError(e.to_string()))?);
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_append_and_query() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();

        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCreated,
            agent_id: Some(AgentID::new()),
            task_id: Some(TaskID::new()),
            tool_id: None,
            details: serde_json::json!({"prompt": "Summarize logs"}),
            severity: AuditSeverity::Info,
        };

        log.append(entry.clone()).unwrap();
        let results = log.query_recent(10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_type, AuditEventType::TaskCreated);
    }

    #[test]
    fn test_query_by_trace() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let trace = TraceID::new();

        // Append 3 entries with same trace_id
        for event in [
            AuditEventType::IntentReceived,
            AuditEventType::ToolExecutionStarted,
            AuditEventType::IntentCompleted,
        ] {
            log.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: trace,
                event_type: event,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            })
            .unwrap();
        }

        let results = log.query_by_trace(&trace).unwrap();
        assert_eq!(results.len(), 3);
    }
}
