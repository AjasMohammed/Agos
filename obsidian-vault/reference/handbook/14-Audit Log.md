---
title: Audit Log
tags:
  - reference
  - audit
  - security
  - v3
date: 2026-03-17
status: complete
---

# Audit Log

> The AgentOS audit log is an append-only SQLite database that records every significant system action with a SHA-256 Merkle hash chain for tamper detection.

---

## What is the Audit Log

The audit log is an append-only SQLite database stored at the path configured by `[audit] log_path`. Every significant action in the system produces an `AuditEntry`. Entries form a SHA-256 Merkle hash chain: each entry incorporates the hash of its predecessor, so any tampering with a historical record breaks all subsequent hashes and is immediately detectable.

Key properties:

- **Append-only** — entries are never deleted by normal operation (only pruned by retention policy)
- **61 event types** across 15 categories, covering the full agent lifecycle
- **Merkle chain** — tamper-evident via `prev_hash` / `entry_hash` columns
- **WAL journal mode** — concurrent reads during writes

---

## Querying Audit Logs

```
agentctl audit logs [--last <N>]
```

Retrieves the most recent `N` entries (default: 50).

Output format:

```
TIMESTAMP                      EVENT TYPE                SEVERITY   DETAILS
----------------------------------------------------------------------------------------------------
2026-03-17T10:23:01.123456Z    TaskCreated               Info       {"task_id":"...","agen...
2026-03-17T10:23:01.456789Z    ToolExecutionStarted      Info       {"tool_id":"file-read"...
2026-03-17T10:23:02.001234Z    LLMInferenceCompleted     Info       {"model":"llama3.2","t...
```

Details are truncated to 30 characters in the terminal. Use `agentctl audit export` for full JSON.

---

## Audit Event Types

All 61 event types, grouped by category:

### Task Lifecycle

| Event Type | When |
|---|---|
| `TaskCreated` | A new task is submitted to the kernel |
| `TaskStateChanged` | Task transitions state (pending → running → completed, etc.) |
| `TaskCompleted` | Task finishes successfully |
| `TaskFailed` | Task finishes with an error |
| `TaskTimeout` | Task exceeds its configured timeout |

### Intent Processing

| Event Type | When |
|---|---|
| `IntentReceived` | An intent message arrives at the router |
| `IntentRouted` | Intent dispatched to a specific handler |
| `IntentCompleted` | Intent processing finishes successfully |
| `IntentFailed` | Intent processing finishes with an error |

### Capability / Permission

| Event Type | When |
|---|---|
| `PermissionGranted` | An agent is granted a new permission |
| `PermissionRevoked` | A previously granted permission is removed |
| `PermissionDenied` | An operation is blocked by the capability system |
| `TokenIssued` | A capability token is minted for an agent |
| `TokenExpired` | A capability token passes its expiry time |

### Tool Events

| Event Type | When |
|---|---|
| `ToolInstalled` | A tool manifest is registered in the tool registry |
| `ToolRemoved` | A tool is removed from the registry |
| `ToolExecutionStarted` | A tool starts executing |
| `ToolExecutionCompleted` | A tool execution returns a result |
| `ToolExecutionFailed` | A tool execution throws an error |

### LLM / Agent Events

| Event Type | When |
|---|---|
| `AgentConnected` | An agent connects to the kernel |
| `AgentDisconnected` | An agent disconnects from the kernel |
| `LLMInferenceStarted` | An inference request is sent to the LLM backend |
| `LLMInferenceCompleted` | An inference response is received |
| `LLMInferenceError` | An inference request fails |

### Secret Events

| Event Type | When |
|---|---|
| `SecretCreated` | A new secret is stored in the vault |
| `SecretAccessed` | A secret is read from the vault |
| `SecretRevoked` | A secret is permanently deleted |
| `SecretRotated` | A secret value is replaced with a new value |

### System Events

| Event Type | When |
|---|---|
| `KernelStarted` | The kernel process starts up |
| `KernelShutdown` | The kernel begins graceful shutdown |
| `KernelSubsystemRestarted` | A kernel subsystem (scheduler, bus, etc.) is restarted |

### Schedule Events

| Event Type | When |
|---|---|
| `ScheduledJobCreated` | A cron or interval job is registered |
| `ScheduledJobFired` | A scheduled job triggers and starts a task |
| `ScheduledJobPaused` | A scheduled job is suspended |
| `ScheduledJobResumed` | A previously paused job resumes |
| `ScheduledJobDeleted` | A scheduled job is removed |

### Background Task Events

| Event Type | When |
|---|---|
| `BackgroundTaskStarted` | A background task begins execution in the pool |
| `BackgroundTaskCompleted` | A background task finishes normally |
| `BackgroundTaskFailed` | A background task exits with an error |
| `BackgroundTaskKilled` | A background task is force-terminated |

### Budget Enforcement

| Event Type | When |
|---|---|
| `BudgetWarning` | Token spend approaches the task budget limit |
| `BudgetExceeded` | Token spend exceeds the budget limit; task is paused |

### Risk Classification

| Event Type | When |
|---|---|
| `RiskEscalation` | An action is classified as Level 3–4 risk and escalated |
| `ActionForbidden` | An action is blocked outright by the risk classifier |

### Snapshot / Checkpoint Events

| Event Type | When |
|---|---|
| `SnapshotTaken` | A context snapshot is saved (before write ops or on budget exhaust) |
| `SnapshotRestored` | A task context is rolled back to a snapshot |
| `SnapshotExpired` | A snapshot is deleted after its 72-hour TTL |

### Cost Attribution

| Event Type | When |
|---|---|
| `CostAttribution` | Per-inference cost data (tokens, USD) attributed to a task |

### Event Trigger System

| Event Type | When |
|---|---|
| `EventEmitted` | An event is published to the event bus |
| `EventSubscriptionCreated` | An agent registers a trigger subscription |
| `EventSubscriptionRemoved` | A trigger subscription is removed |
| `EventDelivered` | An event is successfully delivered to a subscriber |
| `EventThrottled` | Event delivery is throttled (rate limit applied) |
| `EventFilterRejected` | An event is discarded by a subscription filter |
| `EventLoopDetected` | A circular event chain is detected and broken |
| `EventTriggeredTask` | An event trigger fires and creates a new task |
| `EventTriggerFailed` | An event trigger fails to create a task |

### Hardware Abstraction Layer

| Event Type | When |
|---|---|
| `HardwareDeviceDetected` | A new hardware device is registered in the HAL registry |
| `HardwareDeviceApproved` | A device is approved for an agent's use |
| `HardwareDeviceDenied` | A device is denied for all agents |
| `HardwareDeviceRevoked` | An agent's access to a device is revoked |

---

## Severity Levels

| Severity | Usage |
|---|---|
| `Info` | Normal operations — task start, tool run, inference, etc. |
| `Warn` | Degraded state — budget warning, high LLM latency |
| `Error` | Failed operations — tool error, LLM error, task failure |
| `Security` | Security-relevant events — permission denied, token forgery, secret access |

---

## Merkle Chain Verification

```
agentctl audit verify [--from <seq>]
```

Verifies the SHA-256 hash chain from sequence number `seq` (default: the beginning). Each entry's `entry_hash` is computed over all fields including `prev_hash`. Verification walks the chain and recomputes every hash.

Output on success:

```
Audit chain VALID (1204 entries verified)
```

Output on failure:

```
Audit chain INVALID at seq 847 (846 entries checked): hash mismatch
```

The first invalid sequence number is reported, indicating the earliest tampered entry.

---

## Exporting the Audit Chain

```
agentctl audit export [--limit N] [--output <path>]
```

Exports the full audit log (or the most recent `N` entries) as newline-delimited JSON (JSONL). Each line is one `AuditEntry` serialized to JSON.

```bash
# Export all entries to a file
agentctl audit export --output /tmp/audit-backup.jsonl

# Export last 1000 entries to stdout
agentctl audit export --limit 1000
```

---

## Context Snapshots

```
agentctl audit snapshots --task <task-id>
```

Lists all context snapshots saved for a specific task.

Output:

```
SNAPSHOT        SIZE         TAKEN
--------------------------------------------------
snap_0001       4096         1742205781
snap_0002       4128         1742205892
```

**Auto-snapshot behavior:** The kernel takes a snapshot automatically before write operations (file writes, secret creation) and when a task's budget is about to be exhausted. Snapshots are stored in the audit database linked to their task ID.

**Snapshot expiry:** A background sweep runs every 10 minutes and deletes snapshots older than 72 hours.

---

## Rolling Back

```
agentctl audit rollback --task <task-id> [--snapshot <ref>]
```

Restores a task's context window to the state saved in the specified snapshot. If `--snapshot` is omitted, the most recent snapshot is used.

The same operation is also available via the dedicated `agentctl snapshot rollback` command.

```bash
# Roll back to the latest snapshot
agentctl audit rollback --task abc123

# Roll back to a specific snapshot
agentctl audit rollback --task abc123 --snapshot snap_0001
```

After rollback, the task context is restored and can resume from that point.

---

## AuditEntry Structure

Each audit record contains the following fields:

| Field | Type | Description |
|---|---|---|
| `timestamp` | `DateTime<Utc>` | RFC 3339 timestamp of the event |
| `trace_id` | `TraceID` | UUID linking related events in a single trace |
| `event_type` | `AuditEventType` | One of the 61 event type variants |
| `agent_id` | `Option<AgentID>` | The agent that triggered the event |
| `task_id` | `Option<TaskID>` | The task context for the event |
| `tool_id` | `Option<ToolID>` | The tool involved, if any |
| `details` | `JSON` | Structured event payload (event-type specific) |
| `severity` | `AuditSeverity` | `Info`, `Warn`, `Error`, or `Security` |
| `reversible` | `bool` | Whether this action can be undone via rollback |
| `rollback_ref` | `Option<String>` | Snapshot reference for reversible actions |

---

## Related

- [[15-LLM Configuration]] — LLM event types in context
- [[18-Advanced Operations]] — Snapshot and rollback operations in detail
- [[16-Configuration Reference]] — `[audit]` config section
