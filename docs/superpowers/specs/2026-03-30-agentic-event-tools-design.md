# Agentic Event Tools — Design Spec

> Enable agents to subscribe to events, emit custom events, and introspect event state at runtime — eliminating the human-in-the-loop requirement for reactive multi-agent workflows.

**Date:** 2026-03-30
**Status:** Approved
**Approach:** Pure `_kernel_action` dispatch (Approach A)

---

## Problem

AgentOS has a production-grade event system: 50+ typed events, HMAC-signed messages, subscription filtering/throttling, triggered task creation, chain-depth loop detection, and role-based auto-subscriptions. But all event management is CLI-only (`agentctl event subscribe`). Agents cannot:

1. **Self-subscribe** to events during execution
2. **Emit custom events** to signal domain-specific state changes
3. **Manage subscriptions** for other agents (orchestration)
4. **Introspect** their subscription state or event history

This forces human intervention for what should be autonomous agent coordination. A pure agentic workflow — where an orchestrator wires up a multi-agent reactive pipeline at runtime — is impossible without these capabilities.

## Solution

Add **five tools** that agents can call during task execution, using the established `_kernel_action` pattern. Tools are stateless stubs; the kernel intercepts their output and performs the privileged operation with full permission enforcement and audit logging.

### Design Decisions

1. **Custom events** via `EventType::Custom(String)` variant with `AgentDefined` category — agents define domain vocabulary at runtime
2. **Default permanent subscriptions**, opt-in task-scoped via `scope: "task"` parameter — orchestrators wire pipelines that outlive the setup task
3. **Self + others with permission** — agents with `orchestrator` role (or `event.manage:rw` permission) can manage any agent's subscriptions; others self-only
4. **Per-agent emit rate limit** (default 10/s) + existing channel backpressure (capacity 100) — defense in depth against event flooding
5. **Five tools** for full parity with CLI: `event-subscribe`, `event-unsubscribe`, `event-list-subscriptions`, `event-emit`, `event-history`

---

## Type System Changes

### `EventType` — new variant

```rust
// crates/agentos-types/src/event.rs
#[non_exhaustive]
pub enum EventType {
    // ... existing 50+ variants unchanged ...

    /// Agent-defined custom event type. The string is the event name
    /// chosen by the emitting agent (e.g., "DataPipelineComplete").
    /// Matched by exact string in subscriptions.
    Custom(String),
}
```

### `EventCategory` — new variant

```rust
pub enum EventCategory {
    // ... existing 10 categories ...

    /// Events defined and emitted by agents at runtime.
    AgentDefined,
}
```

`EventType::Custom(_)` maps to `EventCategory::AgentDefined` in the existing `category()` method.

### `EventSource` — new variant

```rust
pub enum EventSource {
    // ... existing variants ...

    /// Event was emitted by an agent via the event-emit tool.
    Agent(AgentID),
}
```

### Subscription matching

No new `EventTypeFilter` variants needed:
- `EventTypeFilter::Exact(EventType::Custom("MyEvent".into()))` — exact custom event
- `EventTypeFilter::Category(EventCategory::AgentDefined)` — all custom events
- `EventTypeFilter::All` — everything including custom events

The `parse_event_type_filter()` function in `event_bus.rs` gains support for `"Custom:MyEventName"` syntax → `EventTypeFilter::Exact(EventType::Custom("MyEventName".into()))`.

---

## KernelAction Variants

Five new variants in `crates/agentos-kernel/src/kernel_action.rs`:

```rust
pub(crate) enum KernelAction {
    // ... existing variants ...

    EventSubscribe {
        target_agent: Option<String>,   // None = self, Some = other agent
        event_filter: String,           // "all", "category:X", "Custom:MyEvent", exact type
        payload_filter: Option<String>, // SQL-like predicate
        throttle: Option<String>,       // "none", "once_per:30s", "max:5/60s"
        priority: Option<String>,       // "critical", "high", "normal", "low"
        scope: Option<String>,          // "agent" (default) or "task"
    },
    EventUnsubscribe {
        subscription_id: String,
    },
    EventListSubscriptions {
        target_agent: Option<String>,   // None = self, Some = other agent
    },
    EventEmit {
        event_type: String,             // Custom event name
        payload: serde_json::Value,     // Arbitrary JSON payload
        severity: Option<String>,       // "info" (default), "warning", "critical"
    },
    EventHistory {
        last: u32,
    },
}
```

### `from_tool_result()` parsing

Each variant is parsed from the tool's `_kernel_action` JSON output:
- `"event_subscribe"` → `EventSubscribe`
- `"event_unsubscribe"` → `EventUnsubscribe`
- `"event_list_subscriptions"` → `EventListSubscriptions`
- `"event_emit"` → `EventEmit`
- `"event_history"` → `EventHistory`

---

## Permission Model

### Two new permission resources

| Permission | Default grant | Gates |
|---|---|---|
| `event.self:rw` | All agents at connect | Subscribe/unsubscribe/list own subscriptions, emit custom events, read event history |
| `event.manage:rw` | Agents with `orchestrator` role | Subscribe/unsubscribe/list for other agents |

### Enforcement in `dispatch_kernel_action()`

- `EventSubscribe` with `target_agent: None` → check `event.self:rw`
- `EventSubscribe` with `target_agent: Some(other)` → check `event.manage:rw`
- `EventUnsubscribe` → look up subscription owner; if self → `event.self:rw`, if other → `event.manage:rw`
- `EventListSubscriptions` → same self/other split
- `EventEmit` → check `event.self:rw`
- `EventHistory` → check `event.self:rw`

### Default permission grants at agent connect

In `commands/agent.rs` during `cmd_connect_agent()`, after building the agent's `PermissionSet`:
- All agents get `event.self:rw` (read + write + execute)
- Agents with `orchestrator` role additionally get `event.manage:rw`

Granted before the `AgentAdded` event fires, consistent with role-based subscription timing.

---

## Per-Agent Emit Rate Limiting

### Kernel state

```rust
// crates/agentos-kernel/src/kernel.rs — new field on Kernel struct
pub(crate) agent_event_rate: Arc<RwLock<HashMap<AgentID, RateWindow>>>,

// Private helper in the same file
struct RateWindow {
    count: u32,
    window_start: chrono::DateTime<chrono::Utc>,
}
```

### Behavior

- Default: 10 events/second per agent
- Configurable via `[events] agent_emit_rate_limit` in `config/default.toml`
- When exceeded: kernel returns error result to the tool (agent can retry next second)
- Window resets every 1 second from `window_start`
- Rate of 0 = unlimited (for testing)

### Defense in depth

1. **Per-agent rate limit** — prevents single agent from flooding
2. **Event channel capacity** (100) — backpressure drops events if channel is full
3. **Chain depth cap** (5) — prevents infinite event→task→event loops
4. **Subscription throttle policies** — per-subscription delivery rate limiting

---

## Tool Implementations

All five tools follow the same pattern: validate input → return `_kernel_action` JSON. Each is a stateless struct implementing `AgentTool`.

### `event-subscribe`

**Input schema:**

| Field | Type | Required | Description |
|---|---|---|---|
| `event_filter` | string | yes | `"all"`, `"category:AgentDefined"`, `"Custom:MyEvent"`, or existing type name |
| `target_agent` | string | no | Agent name to subscribe (omit = self, requires `event.manage`) |
| `payload_filter` | string | no | Predicate like `"severity == Critical"` |
| `throttle` | string | no | `"none"`, `"once_per:30s"`, `"max:5/60s"` |
| `priority` | string | no | `"critical"`, `"high"`, `"normal"` (default), `"low"` |
| `scope` | string | no | `"agent"` (default, permanent) or `"task"` (auto-cleanup) |

**Returns:** `{ "subscription_id": "uuid", "event_filter": "...", "scope": "agent" }`

### `event-unsubscribe`

**Input:** `{ "subscription_id": "uuid" }`
**Returns:** `{ "removed": true }`

### `event-list-subscriptions`

**Input:** `{ "target_agent": "agent-name" }` (optional, omit for self)
**Returns:** `{ "subscriptions": [{ id, event_filter, priority, throttle, enabled, created_at }, ...] }`

### `event-emit`

**Input:**

| Field | Type | Required | Description |
|---|---|---|---|
| `event_type` | string | yes | Custom event name (e.g., `"DataPipelineComplete"`) |
| `payload` | object | no | Arbitrary JSON (default `{}`) |
| `severity` | string | no | `"info"` (default), `"warning"`, `"critical"` |

**Returns:** `{ "event_id": "uuid", "event_type": "DataPipelineComplete", "delivered": true }`

### `event-history`

**Input:** `{ "last": 20 }` (optional, default 20)
**Returns:** `{ "events": [{ id, event_type, source, severity, timestamp, payload_preview }, ...] }`

---

## Kernel Dispatch Handlers

### `EventSubscribe` handler

1. Resolve target agent — `None` → task's `agent_id`; `Some(name)` → look up in agent registry
2. Permission check — self → `event.self:rw`, other → `event.manage:rw`
3. Parse `event_filter` via extended `parse_event_type_filter()` (handles `"Custom:..."`)
4. Create `EventSubscription`, call `event_bus.subscribe()`
5. If `scope == "task"`, register in `task_scoped_subscriptions`
6. Audit: `EventSubscriptionCreated`
7. Return subscription ID

### `EventUnsubscribe` handler

1. Look up subscription in event bus
2. Ownership check — subscription's `agent_id` vs task's `agent_id`; if different → require `event.manage:rw`
3. Call `event_bus.unsubscribe()`
4. Clean up from `task_scoped_subscriptions` if present
5. Audit: `EventSubscriptionRemoved`

### `EventListSubscriptions` handler

1. Resolve target agent (self or other, same permission split)
2. Call `event_bus.list_subscriptions_for_agent()`
3. Serialize to JSON array

### `EventEmit` handler

1. Permission check: `event.self:rw`
2. **Security constraint:** Agents can only emit `Custom(...)` events. The `event_type` string is always wrapped as `EventType::Custom(event_type)`. Agents cannot emit kernel events like `TaskCompleted`, `CapabilityViolation`, or any other variant — those are reserved for kernel subsystems. If an agent passes a name matching a kernel event (e.g., `"TaskCompleted"`), it becomes `Custom("TaskCompleted")`, which is a distinct type and won't match subscriptions for the real `EventType::TaskCompleted`.
3. Rate limit check against `agent_event_rate` — if over limit, return error
4. Call `emit_signed_event()` with:
   - `event_type: EventType::Custom(event_type)`
   - `source: EventSource::Agent(task.agent_id)`
   - `severity`: parsed from string, default `Info`
   - `chain_depth`: `task.trigger_source.chain_depth + 1` if event-triggered, else `0`
5. Return event ID

### `EventHistory` handler

1. Permission check: `event.self:rw`
2. Query audit log for recent `EventEmitted` entries (same as existing `cmd_event_history`)
3. Return serialized list

---

## Chain Depth Propagation

When an agent emits a custom event from within an event-triggered task, chain depth increments:

```
User task (depth 0) → emits "PipelineReady" (depth 0)
  → triggers Agent B task (depth 0)
    → Agent B emits "StageComplete" (depth 1)
      → triggers Agent C task (depth 1)
        → Agent C emits "ResultReady" (depth 2)
          → ... up to depth 5, then dropped with EventLoopDetected audit entry
```

The kernel reads `task.trigger_source.chain_depth` and passes `chain_depth + 1` to `emit_signed_event()`. For tasks without a trigger source (user-initiated), chain depth starts at 0.

---

## Configuration

### `config/default.toml` — new section

```toml
[events]
# Maximum custom events per second per agent (0 = unlimited)
agent_emit_rate_limit = 10
```

---

## End-to-End Example

```
1. Orchestrator agent runs setup task:
   - Calls event-subscribe: { event_filter: "Custom:DataReady", target_agent: "analyzer" }
     → kernel creates permanent subscription for analyzer
   - Calls event-subscribe: { event_filter: "Custom:AnalysisComplete" }
     → kernel creates permanent subscription for orchestrator itself
   - Setup task completes. Subscriptions persist.

2. Data-ingestion agent runs a task, finishes loading data:
   - Calls event-emit: { event_type: "DataReady", payload: { dataset: "sales-q1" } }
     → kernel signs event, sends to channel
     → EventDispatcher evaluates subscriptions
     → analyzer agent gets triggered task with event details in prompt

3. Analyzer runs triggered task, completes analysis:
   - Calls event-emit: { event_type: "AnalysisComplete", payload: { report_id: "rpt-42" } }
     → orchestrator gets triggered task
     → chain depth: 2 (DataReady:0 → AnalysisComplete:1 → orchestrator task:2)

4. Pipeline completes — zero human intervention after initial agent connect.
```

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-types/src/event.rs` | Add `Custom(String)` to `EventType`, `AgentDefined` to `EventCategory`, `Agent(AgentID)` to `EventSource`, update `category()` |
| `crates/agentos-kernel/src/kernel_action.rs` | Add 5 `KernelAction` variants, `from_tool_result()` parsing, `dispatch_kernel_action()` handlers |
| `crates/agentos-kernel/src/event_bus.rs` | Extend `parse_event_type_filter()` for `"Custom:..."` syntax |
| `crates/agentos-kernel/src/commands/agent.rs` | Grant `event.self:rw` to all agents, `event.manage:rw` to orchestrators at connect |
| `crates/agentos-kernel/src/kernel.rs` | Add `agent_event_rate` field, initialize in constructor |
| `crates/agentos-tools/src/runner.rs` | Register 5 new tools |
| `crates/agentos-tools/src/event_subscribe.rs` | New — `EventSubscribeTool` impl |
| `crates/agentos-tools/src/event_unsubscribe.rs` | New — `EventUnsubscribeTool` impl |
| `crates/agentos-tools/src/event_list_subscriptions.rs` | New — `EventListSubscriptionsTool` impl |
| `crates/agentos-tools/src/event_emit.rs` | New — `EventEmitTool` impl |
| `crates/agentos-tools/src/event_history.rs` | New — `EventHistoryTool` impl |
| `crates/agentos-tools/src/lib.rs` | Add `mod` + `pub use` for 5 new modules |
| `tools/core/event-subscribe.toml` | New manifest |
| `tools/core/event-unsubscribe.toml` | New manifest |
| `tools/core/event-list-subscriptions.toml` | New manifest |
| `tools/core/event-emit.toml` | New manifest |
| `tools/core/event-history.toml` | New manifest |
| `config/default.toml` | Add `[events]` section with `agent_emit_rate_limit` |

**13 existing files modified, 10 new files created.** All changes are additive — no existing behavior modified.
