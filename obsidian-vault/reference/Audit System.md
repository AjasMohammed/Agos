---
title: Audit System
tags: [reference, audit, security]
---

# Audit System

The audit log provides an append-only, immutable record of every significant operation in AgentOS.

**Source:** `crates/agentos-audit/src/log.rs`

## Storage

SQLite database at the configured `audit.log_path`. Append-only by design - entries are never modified or deleted.

## AuditEntry Structure

```rust
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub trace_id: TraceID,
    pub event_type: AuditEventType,
    pub agent_id: Option<AgentID>,
    pub task_id: Option<TaskID>,
    pub tool_id: Option<ToolID>,
    pub details: serde_json::Value,
    pub severity: AuditSeverity,
}
```

## Severity Levels

| Level | Meaning |
|---|---|
| `Info` | Normal operations (task started, tool executed) |
| `Warn` | Notable events (token expired, permission close to limit) |
| `Error` | Failures (tool crash, LLM error, task timeout) |
| `Security` | Security-relevant (permission denied, token invalid, secret access) |

## Event Types (35+)

### Task Events
- `TaskCreated`, `TaskStateChanged`, `TaskCompleted`, `TaskFailed`, `TaskTimeout`

### Intent Events
- `IntentReceived`, `IntentRouted`, `IntentCompleted`, `IntentFailed`

### Permission Events
- `PermissionGranted`, `PermissionRevoked`, `PermissionDenied`

### Token Events
- `TokenIssued`, `TokenExpired`

### Tool Events
- `ToolInstalled`, `ToolRemoved`
- `ToolExecutionStarted`, `ToolExecutionCompleted`, `ToolExecutionFailed`

### Agent Events
- `AgentConnected`, `AgentDisconnected`

### LLM Events
- `LLMInferenceStarted`, `LLMInferenceCompleted`, `LLMInferenceError`

### Secret Events
- `SecretCreated`, `SecretAccessed`, `SecretRevoked`, `SecretRotated`

### System Events
- `KernelStarted`, `KernelShutdown`

### Schedule Events
- `ScheduledJobCreated`, `ScheduledJobFired`, `ScheduledJobPaused`, `ScheduledJobResumed`, `ScheduledJobDeleted`

### Background Events
- `BackgroundTaskStarted`, `BackgroundTaskCompleted`, `BackgroundTaskFailed`, `BackgroundTaskKilled`

## CLI Usage

```bash
# View recent audit logs
agentctl audit logs

# View last 50 entries
agentctl audit logs --limit 50

# Filter by severity
agentctl audit logs --severity security
agentctl audit logs --severity error
```

## Traceability

Every audit entry includes a `TraceID` that correlates related events across a single operation chain. This enables full end-to-end tracing from CLI command through kernel processing to tool execution and response.
