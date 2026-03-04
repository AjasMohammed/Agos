# Plan 03 — Audit Log (`agentos-audit` crate)

## Goal

Implement an append-only, kernel-managed audit log backed by SQLite. Every intent message, tool execution, and LLM call is logged. No tool or agent can modify the log — only the kernel writes.

## Dependencies

- `agentos-types`
- `rusqlite` (with `bundled` feature)
- `serde`, `serde_json`
- `chrono`

## Database Schema

Single SQLite database file at the configured `audit.log_path`.

```sql
CREATE TABLE IF NOT EXISTS audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   TEXT NOT NULL,           -- ISO 8601
    trace_id    TEXT NOT NULL,           -- TraceID UUID
    event_type  TEXT NOT NULL,           -- see AuditEventType enum
    agent_id    TEXT,                    -- AgentID UUID (nullable for kernel events)
    task_id     TEXT,                    -- TaskID UUID (nullable)
    tool_id     TEXT,                    -- ToolID UUID (nullable)
    details     TEXT NOT NULL,           -- JSON blob with event-specific data
    severity    TEXT NOT NULL DEFAULT 'info'  -- info, warn, error, security
);

CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp);
CREATE INDEX IF NOT EXISTS idx_audit_trace_id ON audit_log(trace_id);
CREATE INDEX IF NOT EXISTS idx_audit_event_type ON audit_log(event_type);
CREATE INDEX IF NOT EXISTS idx_audit_agent_id ON audit_log(agent_id);
```

## Core Struct: `AuditLog`

```rust
use rusqlite::Connection;
use std::sync::Mutex;
use std::path::Path;

pub struct AuditLog {
    conn: Mutex<Connection>,
}
```

Key: The `Mutex<Connection>` ensures single-writer access. The `AuditLog` is owned by the kernel and passed by reference to subsystems via `Arc<AuditLog>`.

## Event Types

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum AuditSeverity {
    Info,
    Warn,
    Error,
    Security,   // security-relevant events (permission denied, token issues)
}
```

## Audit Entry

```rust
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
```

## Public API

```rust
impl AuditLog {
    /// Open or create the audit log database at the given path.
    pub fn open(path: &Path) -> Result<Self, AgentOSError>;

    /// Append a single audit entry. This is the ONLY write operation.
    /// There is no update or delete — the log is append-only.
    pub fn append(&self, entry: AuditEntry) -> Result<(), AgentOSError>;

    /// Query recent entries (most recent first).
    pub fn query_recent(&self, limit: u32) -> Result<Vec<AuditEntry>, AgentOSError>;

    /// Query by trace ID (find all events in a single request chain).
    pub fn query_by_trace(&self, trace_id: &TraceID) -> Result<Vec<AuditEntry>, AgentOSError>;

    /// Query by event type.
    pub fn query_by_type(
        &self,
        event_type: AuditEventType,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AgentOSError>;

    /// Query by time range.
    pub fn query_by_time_range(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AgentOSError>;

    /// Count total entries (for status display).
    pub fn count(&self) -> Result<u64, AgentOSError>;
}
```

## Implementation Notes

1. **Append-only**: There are NO `UPDATE` or `DELETE` SQL statements anywhere in this crate. If an entry is wrong, a correction entry is appended.
2. **Thread safety**: `Mutex<Connection>` ensures serialized writes. Reads acquire the mutex briefly.
3. **Performance**: Use WAL mode (`PRAGMA journal_mode=WAL`) for concurrent reads while writing.
4. **Initialization**: `open()` creates the table and indexes if they don't exist.

## Tests

```rust
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
        for event in [AuditEventType::IntentReceived, AuditEventType::ToolExecutionStarted, AuditEventType::IntentCompleted] {
            log.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: trace,
                event_type: event,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            }).unwrap();
        }

        let results = log.query_by_trace(&trace).unwrap();
        assert_eq!(results.len(), 3);
    }
}
```

## Verification

```bash
cargo test -p agentos-audit
```
