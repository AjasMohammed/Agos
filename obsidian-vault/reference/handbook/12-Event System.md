---
title: Event System
tags:
  - handbook
  - events
  - subscriptions
  - v3
date: 2026-03-17
status: complete
---

# Event System

> The kernel emits signed events for every significant system action. Agents can subscribe to event types and be automatically triggered to execute tasks when those events fire.

---

## What is the Event System

The event system is a publish-subscribe bus built into the AgentOS kernel. The kernel emits an `EventMessage` for every significant lifecycle transition — a task completing, a secret being accessed, a budget threshold being crossed, and so on.

Key properties:

- **Signed events** — every event is HMAC-signed over a canonical string `event_id|event_type|timestamp|chain_depth`. This ensures events cannot be forged or replayed.
- **Audit trail** — every emitted event is appended to the audit log under `EventEmitted`.
- **Agent-triggerable** — agents subscribe to event types (or categories, or all events) and are automatically given a task to execute when a matching event fires.
- **Loop-safe** — the kernel tracks `chain_depth` on every event. If a triggered task itself emits events, those events increment the depth counter. Events exceeding `max_chain_depth` are dropped and logged as `EventLoopDetected`.

The `EventBus` (`crates/agentos-kernel/src/event_bus.rs`) is a pure subscription registry and filter evaluator. The `Kernel` orchestrates the full flow: emit → evaluate subscriptions → create triggered task.

---

## Event Types

> **Note:** Event types (`EventType`) are what agents subscribe to at runtime. These are distinct from audit event types (`AuditEventType` in the audit log), which track internal kernel operations. See [[14-Audit Log]] for the audit event types.

The `EventType` enum (`agentos-types/src/event.rs`) defines **72 event types** across **11 categories**. Each event belongs to exactly one `EventCategory`, which agents can use for category-level subscriptions.

### AgentLifecycle

| Event | Description |
|-------|-------------|
| `AgentAdded` | An agent was registered with the kernel. |
| `AgentRemoved` | An agent was unregistered from the kernel. |
| `AgentPermissionGranted` | A permission was granted to an agent. |
| `AgentPermissionRevoked` | A permission was revoked from an agent. |

### TaskLifecycle

| Event | Description |
|-------|-------------|
| `TaskStarted` | A task began execution. |
| `TaskCompleted` | A task completed successfully. |
| `TaskFailed` | A task failed after exhausting retries. |
| `TaskTimedOut` | A task was killed because it exceeded its timeout. |
| `TaskSuspended` | A task was suspended (e.g., waiting for escalation approval). |
| `TaskDelegated` | A task was delegated to another agent. |
| `TaskRetrying` | A failed task is being retried. |
| `TaskDeadlockDetected` | A circular dependency was detected between tasks. |
| `TaskPreempted` | A lower-priority task was preempted by a higher-priority one. |

### SecurityEvents

| Event | Description |
|-------|-------------|
| `PromptInjectionAttempt` | A prompt injection attack was detected. |
| `CapabilityViolation` | An operation was attempted without a valid capability token. |
| `UnauthorizedToolAccess` | A tool was invoked without the required permissions. |
| `SecretsAccessAttempt` | An unauthorized attempt to read secrets from the vault. |
| `SandboxEscapeAttempt` | A sandboxed tool attempted to escape its confinement. |
| `AuditLogTamperAttempt` | An attempt to modify or delete audit log entries was detected. |
| `AgentImpersonationAttempt` | An agent attempted to impersonate another agent. |
| `UnverifiedToolInstalled` | A tool was installed without a valid signature. |

### MemoryEvents

| Event | Description |
|-------|-------------|
| `ContextWindowNearLimit` | The context window is approaching its token limit. |
| `ContextWindowExhausted` | The context window has reached its maximum capacity. |
| `EpisodicMemoryWritten` | A new episodic memory entry was persisted. |
| `SemanticMemoryConflict` | A semantic memory write conflicts with an existing entry. |
| `MemorySearchFailed` | A memory search query returned no results or errored. |
| `WorkingMemoryEviction` | An entry was evicted from working memory due to capacity limits. |

### SystemHealth

| Event | Description |
|-------|-------------|
| `CPUSpikeDetected` | CPU usage exceeded the configured threshold. |
| `MemoryPressure` | System memory usage is critically high. |
| `DiskSpaceLow` | Available disk space is below the warning threshold. |
| `DiskSpaceCritical` | Available disk space is below the critical threshold. |
| `ProcessCrashed` | A monitored process terminated unexpectedly. |
| `NetworkInterfaceDown` | A network interface became unavailable. |
| `ContainerResourceQuotaExceeded` | A container exceeded its resource quota. |
| `KernelSubsystemError` | An internal kernel subsystem encountered an error. |
| `BudgetWarning` | An agent crossed the `warn_at_pct` budget threshold. |
| `BudgetExhausted` | An agent hit a hard budget limit. |

### HardwareEvents

| Event | Description |
|-------|-------------|
| `GPUAvailable` | A GPU became available for compute tasks. |
| `GPUMemoryPressure` | GPU memory usage is critically high. |
| `SensorReadingThresholdExceeded` | A hardware sensor reading exceeded its configured threshold. |
| `DeviceConnected` | A hardware device was connected to the system. |
| `DeviceDisconnected` | A hardware device was disconnected. |
| `HardwareAccessGranted` | An agent was granted access to a hardware device. |
| `DeviceMounted` | A device was mounted and made available. |
| `DeviceUnmounted` | A device was unmounted. |
| `DeviceEjected` | A device was safely ejected from the system. |

### ToolEvents

| Event | Description |
|-------|-------------|
| `ToolInstalled` | A tool was registered in the tool registry. |
| `ToolRemoved` | A tool was removed from the registry. |
| `ToolExecutionFailed` | A tool invocation failed. |
| `ToolSandboxViolation` | A tool violated its sandbox policy. |
| `ToolResourceQuotaExceeded` | A tool exceeded its allocated resource quota. |
| `ToolChecksumMismatch` | A tool's checksum did not match the expected value. |
| `ToolRegistryUpdated` | The tool registry was updated (e.g., new manifests loaded). |
| `ToolCallStarted` | A tool invocation began. |
| `ToolCallCompleted` | A tool invocation completed successfully. |

### AgentCommunication

| Event | Description |
|-------|-------------|
| `DirectMessageReceived` | A direct message was received from another agent. |
| `BroadcastReceived` | A broadcast message was received. |
| `DelegationReceived` | A task delegation request was received. |
| `DelegationResponseReceived` | A response to a delegation request was received. |
| `MessageDeliveryFailed` | A message could not be delivered to the target agent. |
| `AgentUnreachable` | The target agent is not connected or not responding. |

### AgentRPC

| Event | Description |
|-------|-------------|
| `AgentRpcCallStarted` | An inter-agent RPC call was initiated. |
| `AgentRpcCallCompleted` | An inter-agent RPC call completed successfully. |
| `AgentRpcCallTimedOut` | An inter-agent RPC call exceeded its timeout. |

### ScheduleEvents

| Event | Description |
|-------|-------------|
| `CronJobFired` | A cron-scheduled job triggered. |
| `ScheduledTaskMissed` | A scheduled task missed its execution window. |
| `ScheduledTaskCompleted` | A scheduled task completed successfully. |
| `ScheduledTaskFailed` | A scheduled task failed. |

### ExternalEvents

| Event | Description |
|-------|-------------|
| `WebhookReceived` | An incoming webhook was received by the kernel. |
| `ExternalFileChanged` | A watched external file was modified. |
| `ExternalAPIEvent` | An event was received from an external API integration. |
| `ExternalAlertReceived` | An alert was received from an external monitoring system. |

---

## Subscribing to Events

```bash
agentctl event subscribe \
  --agent <agent-name> \
  --event <filter> \
  [--filter "<payload-expr>"] \
  [--throttle "<policy>"] \
  [--priority <level>]
```

On success, the command prints the subscription ID:

```
Subscription created: 8f3a1b2c-4d5e-...
```

### Event Filter (`--event`)

Selects which events trigger the subscription. Three forms are accepted:

| Form | Example | Matches |
|------|---------|---------|
| `all` | `--event all` | Every event emitted by the kernel |
| `category:<name>` | `--event category:AgentLifecycle` | All events in a named category |
| Exact event type | `--event TaskCompleted` | Only the exact named event |

### Payload Filter (`--filter`)

An optional expression evaluated against the event's payload JSON. Only events whose payload matches the expression will trigger the subscription.

Syntax: `<field> <op> <value>` clauses joined by `AND`.

Supported operators: `==`, `!=`, `>`, `>=`, `<`, `<=`, `in`, `contains`.

Examples:

```bash
# Only fire when CPU exceeds 90%
--filter "cpu_percent > 90"

# Only for critical severity events
--filter "severity == Critical"

# Combined
--filter "cpu_percent > 90 AND severity == Critical"
```

### Throttle Policy (`--throttle`)

Limits how often an event is delivered to the subscription, even if it fires repeatedly.

| Policy | Format | Behavior |
|--------|--------|----------|
| None (default) | `none` | Every matching event triggers delivery. |
| At most once per duration | `once_per:<dur>` | E.g. `once_per:30s` — at most one delivery per 30-second window. |
| Max count per duration | `max:<N>/<dur>` | E.g. `max:5/60s` — at most 5 deliveries per 60-second window. |

Duration units: `s` (seconds), `m` (minutes), `h` (hours).

### Priority (`--priority`)

Controls the task priority assigned when the subscription triggers an agent.

| Level | Description |
|-------|-------------|
| `critical` | Priority 1 — scheduled before all other tasks. |
| `high` | Priority 3. |
| `normal` (default) | Priority 5. |
| `low` | Priority 8 — runs when nothing more urgent is queued. |

---

## Managing Subscriptions

### List Subscriptions

```bash
# All subscriptions
agentctl event subscriptions list

# Subscriptions for a specific agent
agentctl event subscriptions list --agent <agent-name>
```

Output columns: `ID`, `AGENT_ID`, `EVENT`, `PAYLOAD`, `PRIORITY`, `ENABLED`.

### Show Subscription Details

```bash
agentctl event subscriptions show --id <subscription-id>
```

Shows all fields: agent ID, event filter, payload filter, priority, throttle policy, enabled state, and creation time.

### Enable / Disable a Subscription

```bash
agentctl event subscriptions enable --id <subscription-id>
agentctl event subscriptions disable --id <subscription-id>
```

Disabled subscriptions remain stored but do not trigger tasks. Re-enable at any time without losing the subscription configuration.

### Remove a Subscription

```bash
agentctl event unsubscribe <subscription-id>
```

Permanently removes the subscription. The removal is logged to the audit trail as `EventSubscriptionRemoved`.

---

## Event History

```bash
agentctl event history --last <N>
```

Queries the audit log for the `N` most recent `EventEmitted` entries. Default: 20.

Output format:

```
TIMESTAMP                  EVENT TYPE                     SEVERITY   DEPTH
2026-03-17T10:12:01Z       TaskCompleted                  Info       0
2026-03-17T10:12:02Z       BudgetWarning                  Warning    0
2026-03-17T10:12:02Z       ToolCallCompleted              Info       1
```

Columns:
- **TIMESTAMP** — RFC3339 timestamp of when the event was emitted.
- **EVENT TYPE** — the `EventType` variant name.
- **SEVERITY** — `Info`, `Warning`, `Critical`.
- **DEPTH** — chain depth (0 = directly emitted by kernel; >0 = triggered by a prior event).

---

## Event-Triggered Tasks

When a subscription matches an event, the kernel automatically creates an `AgentTask` for the subscribing agent. The task's prompt is a structured description of the event that fired:

- The event type and ID.
- The event payload as JSON.
- The subscription priority determines the task's queue priority.
- The task's `trigger_source` field records the event ID, event type, subscription ID, and chain depth for audit traceability.

### Loop Detection

Every event carries a `chain_depth` counter. When an event triggers a task, and that task's execution emits further events, those events have `chain_depth + 1`. The kernel enforces a maximum chain depth (`max_chain_depth` in config). If an event arrives with a depth exceeding this limit, it is:

1. Dropped without processing.
2. Logged to the audit trail as `EventLoopDetected` with the event ID, type, and depth.

This prevents runaway chains where `A fires → B reacts → emits C → A fires again → ...`.

---

## Related

- [[13-Cost Tracking]] — `BudgetWarning` and `BudgetExceeded` events can trigger agent responses
- [[11-Pipeline and Workflows]] — pipeline runs emit events on completion
- [[08-Security Model]] — event HMAC signing and audit trail
