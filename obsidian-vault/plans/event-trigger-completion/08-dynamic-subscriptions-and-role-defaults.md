---
title: "Phase 08 — Dynamic Subscriptions & Role-Based Defaults"
tags:
  - kernel
  - event-system
  - plan
  - v3
date: 2026-03-13
status: planned
effort: 4h
priority: medium
---
# Phase 08 — Dynamic Subscriptions & Role-Based Defaults

> Allow agents to subscribe/unsubscribe from events at runtime via intents, and auto-apply default subscriptions based on agent role at connect time.

---

## Why This Phase

Currently, only the human operator can create subscriptions via CLI. Spec §5 requires two additional subscription paths:

1. **Dynamic subscriptions:** Agents emit `Subscribe`/`Unsubscribe` intents during task execution to self-register for events. Example: an agent investigating a security incident subscribes to `SecurityEvents.*` for the duration of its task.

2. **Role-based defaults:** When an agent connects with role `orchestrator`, it should automatically receive subscriptions for `AgentLifecycle.*`, `TaskLifecycle.*`, and `AgentCommunication.*` — without the operator manually creating each one.

---

## Current State

| What | Status |
|------|--------|
| CLI-driven subscriptions (`agentctl event subscribe`) | Working |
| `IntentType` enum in `agentos-types` | Exists — no `Subscribe`/`Unsubscribe` variants |
| Agent roles stored in registry | Working — agents have `role` field |
| Default subscription table per role | **Not defined** |
| Dynamic subscription via agent intent | **Not implemented** |

---

## Target State

- `IntentType::Subscribe` and `IntentType::Unsubscribe` variants added
- Intent router handles these intents by calling `event_bus.subscribe()` / `event_bus.unsubscribe()`
- Requires `event.subscribe` permission in agent's `PermissionSet`
- `SubscriptionDuration` enum: `Task` (auto-remove when task ends), `Permanent`, `TTL(Duration)`
- Default subscriptions applied in `cmd_connect_agent()` based on agent role

---

## Subtasks

### 1. Add `Subscribe` and `Unsubscribe` to `IntentType`

**File:** `crates/agentos-types/src/intent.rs`

```rust
pub enum IntentType {
    // ... existing variants ...
    Subscribe,
    Unsubscribe,
}
```

### 2. Define `SubscribePayload` and `UnsubscribePayload`

**File:** `crates/agentos-types/src/intent.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribePayload {
    pub event_filter: String,          // "SecurityEvents.*" or "TaskLifecycle.TaskFailed"
    pub filter_predicate: Option<String>,  // Optional: "severity == Critical"
    pub duration: SubscriptionDuration,
    pub priority: Option<String>,      // "critical", "high", "normal", "low"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubscriptionDuration {
    Task,                    // Auto-remove when current task completes
    Permanent,               // Persist until explicitly unsubscribed
    TTL { seconds: u64 },    // Auto-remove after duration
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribePayload {
    pub subscription_id: String,  // ID returned from subscribe
}
```

### 3. Handle `Subscribe` intent in the router

**File:** `crates/agentos-kernel/src/router.rs` (or the intent handling path in `task_executor.rs`)

**Where:** In the intent dispatch logic that routes intents to handlers. Add arms for `Subscribe` and `Unsubscribe`:

```rust
IntentType::Subscribe => {
    // 1. Check agent has "event.subscribe" permission
    if !permission_set.check("event.subscribe") {
        return Err(AgentOSError::PermissionDenied("event.subscribe".into()));
    }

    // 2. Parse the SubscribePayload from intent.payload
    let payload: SubscribePayload = serde_json::from_value(intent.payload.clone())?;

    // 3. Parse event filter
    let event_filter = parse_event_type_filter(&payload.event_filter)?;
    let priority = parse_subscription_priority(payload.priority.as_deref());

    // 4. Create subscription via event_bus
    let sub_id = self.event_bus.write().await.subscribe(EventSubscription {
        id: SubscriptionID::new(),
        agent_id: task.agent_id.clone(),
        event_type: event_filter,
        filter: payload.filter_predicate,
        priority,
        throttle: None,
        enabled: true,
        created_at: Utc::now(),
    });

    // 5. If duration is Task, register for cleanup when task completes
    if matches!(payload.duration, SubscriptionDuration::Task) {
        self.register_task_cleanup(task.id, sub_id).await;
    }

    // 6. If duration is TTL, schedule removal
    if let SubscriptionDuration::TTL { seconds } = payload.duration {
        self.schedule_subscription_removal(sub_id, Duration::from_secs(seconds)).await;
    }

    // 7. Return subscription ID to agent
    Ok(ToolOutput::text(format!("Subscribed: {}", sub_id)))
}

IntentType::Unsubscribe => {
    let payload: UnsubscribePayload = serde_json::from_value(intent.payload.clone())?;
    let sub_id = SubscriptionID::parse(&payload.subscription_id)?;

    // Verify the subscription belongs to this agent
    let sub = self.event_bus.read().await.get_subscription(&sub_id)?;
    if sub.agent_id != task.agent_id {
        return Err(AgentOSError::PermissionDenied(
            "Cannot unsubscribe another agent".into()
        ));
    }

    self.event_bus.write().await.unsubscribe(&sub_id)?;
    Ok(ToolOutput::text(format!("Unsubscribed: {}", sub_id)))
}
```

### 4. Add task-scoped subscription cleanup

**File:** `crates/agentos-kernel/src/task_executor.rs`

When a task completes (success or failure), clean up any subscriptions that had `SubscriptionDuration::Task`:

```rust
// At task completion:
self.cleanup_task_subscriptions(task.id).await;
```

This requires a mapping from `TaskID → Vec<SubscriptionID>` stored on the kernel.

### 5. Define default subscription table

**File:** `crates/agentos-kernel/src/event_bus.rs`

Define a function that returns default subscriptions for a given role:

```rust
pub fn default_subscriptions_for_role(role: &str) -> Vec<(EventTypeFilter, SubscriptionPriority)> {
    match role {
        "orchestrator" => vec![
            (EventTypeFilter::Category(EventCategory::AgentLifecycle), SubscriptionPriority::High),
            (EventTypeFilter::Category(EventCategory::TaskLifecycle), SubscriptionPriority::High),
            (EventTypeFilter::Category(EventCategory::AgentCommunication), SubscriptionPriority::Normal),
        ],
        "security-monitor" => vec![
            (EventTypeFilter::Category(EventCategory::SecurityEvents), SubscriptionPriority::Critical),
            (EventTypeFilter::Exact(EventType::ToolSandboxViolation), SubscriptionPriority::Critical),
            (EventTypeFilter::Exact(EventType::ToolChecksumMismatch), SubscriptionPriority::Critical),
        ],
        "sysops" => vec![
            (EventTypeFilter::Category(EventCategory::SystemHealth), SubscriptionPriority::High),
            (EventTypeFilter::Category(EventCategory::HardwareEvents), SubscriptionPriority::Normal),
            (EventTypeFilter::Exact(EventType::ScheduledTaskFailed), SubscriptionPriority::High),
        ],
        "memory-manager" => vec![
            (EventTypeFilter::Category(EventCategory::MemoryEvents), SubscriptionPriority::High),
        ],
        "tool-manager" => vec![
            (EventTypeFilter::Category(EventCategory::ToolEvents), SubscriptionPriority::Normal),
        ],
        _ => vec![  // "general" and unknown roles
            (EventTypeFilter::Exact(EventType::AgentAdded), SubscriptionPriority::Normal),
            (EventTypeFilter::Exact(EventType::DirectMessageReceived), SubscriptionPriority::Normal),
            (EventTypeFilter::Exact(EventType::DelegationReceived), SubscriptionPriority::Normal),
        ],
    }
}
```

### 6. Apply defaults in `cmd_connect_agent()`

**File:** `crates/agentos-kernel/src/commands/agent.rs`

**Where:** After the agent is successfully registered, before the `AgentAdded` event is emitted. Look up the agent's role and create default subscriptions:

```rust
// After agent is registered:
let defaults = default_subscriptions_for_role(&agent_role);
for (event_filter, priority) in defaults {
    self.event_bus.write().await.subscribe(EventSubscription {
        id: SubscriptionID::new(),
        agent_id: agent_id.clone(),
        event_type: event_filter,
        filter: None,
        priority,
        throttle: None, // Use category defaults
        enabled: true,
        created_at: Utc::now(),
    });
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/intent.rs` | Add `Subscribe`/`Unsubscribe` to IntentType, add payload structs |
| `crates/agentos-kernel/src/router.rs` | Handle Subscribe/Unsubscribe intents |
| `crates/agentos-kernel/src/event_bus.rs` | Add `default_subscriptions_for_role()` |
| `crates/agentos-kernel/src/commands/agent.rs` | Apply default subscriptions on agent connect |
| `crates/agentos-kernel/src/task_executor.rs` | Add task-scoped subscription cleanup |

---

## Dependencies

- **Phase 07** should be complete — dynamic subscriptions with filters need the filter evaluator working.

---

## Test Plan

1. **Subscribe intent test:** Mock an agent emitting `IntentType::Subscribe` with `event_filter: "SecurityEvents.*"`, verify subscription is created in event_bus.

2. **Permission check test:** Agent without `event.subscribe` permission attempts to subscribe, verify `PermissionDenied` error.

3. **Task-scoped cleanup test:** Agent subscribes with `SubscriptionDuration::Task`, task completes, verify subscription is removed.

4. **Unsubscribe ownership test:** Agent A tries to unsubscribe Agent B's subscription, verify rejection.

5. **Role defaults test:** Connect an agent with role `orchestrator`, verify it receives subscriptions for `AgentLifecycle`, `TaskLifecycle`, `AgentCommunication`.

6. **Role defaults test (general):** Connect an agent with role `general`, verify it receives `AgentAdded`, `DirectMessageReceived`, `DelegationReceived` subscriptions.

---

## Verification

```bash
cargo build --workspace
cargo test --workspace

grep -n "Subscribe" crates/agentos-types/src/intent.rs
grep -n "default_subscriptions_for_role" crates/agentos-kernel/src/event_bus.rs
grep -n "Subscribe =>" crates/agentos-kernel/src/router.rs
```

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[07-event-filter-predicates]] — Phase 07 (prerequisite — filter evaluation)
- [[agentos-event-trigger-system]] — Original spec §5 (Agent Subscription Model)
