---
title: Event Trigger Completion Data Flow
tags:
  - kernel
  - event-system
  - flow
  - v3
date: 2026-03-13
status: complete
effort: 1h
priority: high
---
# Event Trigger Completion Data Flow

> How events flow from 10 emission source categories through the EventBus to agent trigger tasks.

---

## Diagram

```
┌──────────────────────────────────────────────────────────────────────┐
│                        EVENT EMISSION SOURCES                        │
├────────────────┬─────────────────┬───────────────────────────────────┤
│                │                 │                                   │
│ TASK LIFECYCLE │ SECURITY        │ MEMORY                           │
│ (task_executor │ (task_executor, │ (context_compiler,               │
│  scheduler)    │  intent_valid,  │  episodic, semantic)             │
│                │  capability)    │                                   │
│ TaskStarted    │ PromptInjection │ ContextWindowNearLimit           │
│ TaskCompleted  │ CapabilityViol  │ ContextWindowExhausted           │
│ TaskFailed     │ UnauthorizedTool│ EpisodicMemoryWritten            │
│ TaskTimedOut   │ SecretsAccess   │ SemanticMemoryConflict           │
│ TaskDelegated  │                 │                                   │
│ TaskRetrying   │                 │                                   │
├────────────────┼─────────────────┼───────────────────────────────────┤
│                │                 │                                   │
│ TOOL EVENTS    │ AGENT COMMS     │ SCHEDULE                         │
│ (tool_registry │ (agent_message_ │ (schedule_manager)               │
│  task_executor)│  bus)           │                                   │
│                │                 │                                   │
│ ToolInstalled  │ DirectMessage   │ CronJobFired                     │
│ ToolRemoved    │ BroadcastRecvd  │ ScheduledTaskMissed              │
│ ToolExecFailed │ DelegationRecvd │ ScheduledTaskCompleted           │
│                │ MsgDelivFailed  │ ScheduledTaskFailed              │
├────────────────┼─────────────────┼───────────────────────────────────┤
│                │                 │                                   │
│ SYSTEM HEALTH  │ HARDWARE        │ AGENT LIFECYCLE (existing)       │
│ (health.rs /   │ (hal/registry)  │ (commands/agent.rs,              │
│  new monitor)  │                 │  commands/permission.rs)         │
│                │                 │                                   │
│ CPUSpikeDetect │ GPUAvailable    │ AgentAdded          ✅ done      │
│ MemoryPressure │ DeviceConnected │ AgentRemoved         ✅ done      │
│ DiskSpaceLow   │ DeviceDisconnect│ PermissionGranted    ✅ done      │
│ ProcessCrashed │                 │ PermissionRevoked    ✅ done      │
└────────┬───────┴────────┬────────┴──────────┬────────────────────────┘
         │                │                   │
         ▼                ▼                   ▼
   ┌─────────────────────────────────────────────────┐
   │            kernel.emit_event()                    │
   │                                                   │
   │  1. Build EventMessage {                          │
   │       id: EventID::new(),                         │
   │       event_type,                                 │
   │       source,                                     │
   │       severity,                                   │
   │       payload: serde_json::Value,                 │
   │       signature: capability_engine.sign_data(),   │
   │       chain_depth: 0 (or parent+1),               │
   │     }                                             │
   │  2. Audit log: EventEmitted                       │
   │  3. Send to event_sender channel                  │
   └─────────────────────┬───────────────────────────┘
                         │
                         ▼
   ┌─────────────────────────────────────────────────┐
   │     EventDispatcher (supervised task in run_loop) │
   │                                                   │
   │  Receives EventMessage from channel               │
   │                                                   │
   │  1. Check chain_depth <= 5                        │
   │     └─ if exceeded → audit EventLoopDetected, drop│
   │                                                   │
   │  2. event_bus.evaluate_subscriptions(&event)      │
   │     ├─ Match EventTypeFilter (Exact/Category/All) │
   │     ├─ Check subscription.enabled                 │
   │     ├─ Evaluate filter predicate (Phase 07)  ←NEW│
   │     └─ Check throttle policy                      │
   │                                                   │
   │  3. For each matched subscription:                │
   │     ├─ build_trigger_prompt(event, agent)         │
   │     │   ├─ Custom prompt (13 types)          ←NEW│
   │     │   └─ Generic fallback (remaining types)     │
   │     ├─ Issue CapabilityToken for triggered task   │
   │     ├─ Map SubscriptionPriority → scheduler prio  │
   │     │   (Critical=1, High=3, Normal=5, Low=8)     │
   │     ├─ Create AgentTask with trigger_source       │
   │     ├─ Enqueue to TaskScheduler                   │
   │     └─ Audit: EventTriggeredTask, EventDelivered  │
   └─────────────────────────────────────────────────┘
```

---

## Steps

1. **Emission**: A kernel subsystem calls `self.emit_event(type, source, severity, payload, chain_depth)`. The event is signed with HMAC, logged to audit, and pushed to the mpsc channel.

2. **Dispatch**: The `EventDispatcher` supervised task receives the event. It first checks chain depth (max 5) to prevent infinite loops.

3. **Subscription Matching**: The event bus iterates all subscriptions. For each: match type filter → check enabled → evaluate filter predicate → check throttle. Returns a list of `(SubscriptionID, AgentID)` pairs.

4. **Prompt Construction**: For each matched agent, `build_trigger_prompt()` selects a custom prompt template based on event type (13 custom templates) or falls back to the generic template.

5. **Task Creation**: A new `AgentTask` is created with `trigger_source` metadata linking back to the event. A `CapabilityToken` is issued. The task is enqueued to the scheduler with priority mapped from the subscription.

6. **Agent Execution**: The agent receives the trigger prompt as its task context, reasons about it, and emits zero or more intents (tool calls, messages, escalations, or silence).

7. **Chain Propagation**: If the agent's actions emit new events (e.g., agent sends a broadcast → `BroadcastReceived`), those events enter the pipeline at step 1 with `chain_depth + 1`.

---

## Subsystem → event_sender Threading

| Subsystem | How It Gets event_sender | Phase |
|-----------|--------------------------|-------|
| `Kernel` (self) | Direct field access: `self.event_sender` | Existing |
| `TaskExecutor` (via `Arc<Kernel>`) | `self.emit_event()` — already works | Phase 01 |
| `ToolRegistry` | Inject `Option<UnboundedSender>` at construction | Phase 04 |
| `AgentMessageBus` | Inject `Option<UnboundedSender>` at construction | Phase 06 |
| `ScheduleManager` | Inject `Option<UnboundedSender>` at construction | Phase 06 |
| `ContextCompiler` | Emit from `task_executor` after compile returns | Phase 05 |
| `CapabilityEngine` | Emit from caller (task_executor / intent_validator) | Phase 02 |
| `HealthMonitor` (new) | Receives sender at construction | Phase 10 |

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[agentos-event-trigger-system]] — Original design spec
- [[Event Trigger Flow]] — Existing flow diagram
