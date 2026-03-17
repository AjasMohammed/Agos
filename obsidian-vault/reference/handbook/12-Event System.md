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

Every event has an `AuditEventType` recorded in the audit log and a corresponding `EventType` used for subscription matching. The full set of event types, grouped by category:

### Task Lifecycle

| Event | Description |
|-------|-------------|
| `TaskCreated` | A new task was queued. |
| `TaskStateChanged` | A task transitioned between states (e.g., Queued → Running). |
| `TaskCompleted` | A task completed successfully. |
| `TaskFailed` | A task failed after exhausting retries. |
| `TaskTimeout` | A task was killed because it exceeded its timeout. |

### Intent Processing

| Event | Description |
|-------|-------------|
| `IntentReceived` | An intent message arrived at the kernel. |
| `IntentRouted` | The intent was routed to an agent. |
| `IntentCompleted` | Intent processing completed successfully. |
| `IntentFailed` | Intent processing failed. |

### Capability and Permissions

| Event | Description |
|-------|-------------|
| `PermissionGranted` | A permission was granted to an agent. |
| `PermissionRevoked` | A permission was revoked from an agent. |
| `PermissionDenied` | An operation was denied due to insufficient permissions. |
| `TokenIssued` | A capability token was issued for a task. |
| `TokenExpired` | A capability token expired. |

### Tool Lifecycle

| Event | Description |
|-------|-------------|
| `ToolInstalled` | A tool was registered in the tool registry. |
| `ToolRemoved` | A tool was removed from the registry. |
| `ToolExecutionStarted` | A tool invocation began. |
| `ToolExecutionCompleted` | A tool invocation completed successfully. |
| `ToolExecutionFailed` | A tool invocation failed. |

### Agent Lifecycle

| Event | Description |
|-------|-------------|
| `AgentConnected` | An agent connected to the kernel. |
| `AgentDisconnected` | An agent disconnected. |

### LLM Inference

| Event | Description |
|-------|-------------|
| `LLMInferenceStarted` | An LLM inference call began. |
| `LLMInferenceCompleted` | An LLM inference call completed. |
| `LLMInferenceError` | An LLM inference call failed. |

### Secrets

| Event | Description |
|-------|-------------|
| `SecretCreated` | A new secret was stored in the vault. |
| `SecretAccessed` | A secret was read from the vault. |
| `SecretRevoked` | A secret was permanently deleted. |
| `SecretRotated` | A secret's value was updated. |

### System

| Event | Description |
|-------|-------------|
| `KernelStarted` | The kernel process started. |
| `KernelShutdown` | The kernel began graceful shutdown. |
| `KernelSubsystemRestarted` | An internal subsystem was restarted. |

### Schedule

| Event | Description |
|-------|-------------|
| `ScheduledJobCreated` | A cron/interval job was registered. |
| `ScheduledJobFired` | A scheduled job triggered. |
| `ScheduledJobPaused` | A scheduled job was paused. |
| `ScheduledJobResumed` | A paused scheduled job was resumed. |
| `ScheduledJobDeleted` | A scheduled job was removed. |

### Background Tasks

| Event | Description |
|-------|-------------|
| `BackgroundTaskStarted` | A background task began execution. |
| `BackgroundTaskCompleted` | A background task completed. |
| `BackgroundTaskFailed` | A background task failed. |
| `BackgroundTaskKilled` | A background task was forcibly terminated. |

### Budget

| Event | Description |
|-------|-------------|
| `BudgetWarning` | An agent crossed the `warn_at_pct` budget threshold. |
| `BudgetExceeded` | An agent hit a hard budget limit. |

### Risk and Security

| Event | Description |
|-------|-------------|
| `RiskEscalation` | An operation was escalated for human approval. |
| `ActionForbidden` | A high-risk action was blocked. |

### Snapshots

| Event | Description |
|-------|-------------|
| `SnapshotTaken` | A checkpoint snapshot was created. |
| `SnapshotRestored` | The kernel restored from a snapshot. |
| `SnapshotExpired` | A snapshot was deleted due to age (>72 hours). |

### Cost Attribution

| Event | Description |
|-------|-------------|
| `CostAttribution` | Structured cost data for a completed inference call was logged. |

### Event System Internals

| Event | Description |
|-------|-------------|
| `EventEmitted` | An event was emitted and pushed to the event channel. |
| `EventSubscriptionCreated` | A new event subscription was registered. |
| `EventSubscriptionRemoved` | An event subscription was removed. |
| `EventDelivered` | An event was successfully delivered to a subscribing agent (task created). |
| `EventThrottled` | An event was suppressed by the subscription's throttle policy. |
| `EventFilterRejected` | An event did not match a subscription's payload filter. |
| `EventLoopDetected` | An event was dropped because it exceeded the maximum chain depth. |
| `EventTriggeredTask` | A subscription caused a task to be created for an agent. |
| `EventTriggerFailed` | Attempting to create a triggered task failed. |

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
2026-03-17T10:12:02Z       CostAttribution                Info       0
2026-03-17T10:12:02Z       EventTriggeredTask             Info       1
```

Columns:
- **TIMESTAMP** — RFC3339 timestamp of when the event was emitted.
- **EVENT TYPE** — the `AuditEventType` name.
- **SEVERITY** — `Info`, `Warning`, `Critical`, etc.
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
