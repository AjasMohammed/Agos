---
title: Agent Self Event Subscription
tags:
  - kernel
  - events
  - permissions
  - tools
  - agent-manual
date: 2026-04-11
status: in-progress
effort: 1d
priority: high
---

# Agent Self Event Subscription

> Let agents subscribe themselves to kernel events from inside their tool loop, gated by per-category observe permissions, with a discoverability tool and full agent-manual coverage.

---

## Problem

Today, only operators (via CLI/bus) can subscribe an agent to events. The kernel command `EventSubscribe` (handled in [commands/event.rs](../../crates/agentos-kernel/src/commands/event.rs)) takes an `agent_name` argument and is not reachable from inside an agent's LLM loop. The role-based defaults seeded in [event_bus.rs::default_subscriptions_for_role](../../crates/agentos-kernel/src/event_bus.rs) are the only subscriptions an agent ever gets.

A long-running agent that discovers a new responsibility (e.g., "I should react to disk-pressure events") has no way to express that — it has to wait for an operator to grant it.

## Goal

1. Agents can subscribe / unsubscribe / list / discover events from inside their LLM loop via tools.
2. Subscriptions are gated by **per-category observe permissions**, so an agent can only subscribe to events it already has permission to observe (e.g., subscribing to `HardwareEvents` requires `events.hardware:observe`).
3. Agents can list which event categories and types exist, and which ones they have permission to subscribe to.
4. The agent manual documents the entire flow so a freshly-spawned agent can self-discover the capability.

## Design

### Permission model

A new permission resource string per `EventCategory`:

| Category | Permission resource |
|----------|---------------------|
| `AgentLifecycle` | `events.agent_lifecycle` |
| `TaskLifecycle` | `events.task_lifecycle` |
| `SecurityEvents` | `events.security` |
| `MemoryEvents` | `events.memory` |
| `SystemHealth` | `events.system_health` |
| `HardwareEvents` | `events.hardware` |
| `ToolEvents` | `events.tool` |
| `AgentCommunication` | `events.agent_communication` |
| `ScheduleEvents` | `events.schedule` |
| `ExternalEvents` | `events.external` |

Operation: `PermissionOp::Observe` (already exists in [capability.rs](../../crates/agentos-types/src/capability.rs)).

Subscribe-permission rule:

- `EventTypeFilter::Exact(et)` → must hold `events.<et.category()>:observe`
- `EventTypeFilter::Category(cat)` → must hold `events.<cat>:observe`
- `EventTypeFilter::All` → must hold observe on **every** category (effectively root-only)

The existing `events.stream:observe` permission stays as a coarse tool-level gate (declared in each tool's `required_permissions`). The per-category check happens at kernel-action dispatch, defense in depth.

### Default grants

`default_permissions_for_agent` ([commands/agent.rs](../../crates/agentos-kernel/src/commands/agent.rs)) gets baseline observe perms matching the universal default subscriptions:

- `events.agent_lifecycle:observe`
- `events.agent_communication:observe`
- `events.task_lifecycle:observe`

Specialized roles (orchestrator, security-monitor, sysops, memory-manager, tool-manager) get matching extra observe perms via a new `event_observe_permissions_for_role` helper in `event_bus.rs`, applied inside `cmd_connect_agent` right next to the role-based subscription seeding.

### KernelAction variants

Four new variants in [kernel_action.rs](../../crates/agentos-kernel/src/kernel_action.rs):

```rust
EventSubscribeAction { event_filter, payload_filter, throttle, priority }
EventUnsubscribeAction { subscription_id }
EventListSubscriptionsAction
EventListAvailableAction
```

Each is parsed from `_kernel_action` in tool output, dispatched in `dispatch_kernel_action`, and writes audit entries.

`execute_event_subscribe`:
1. Parse the filter via `parse_event_type_filter`
2. Run `check_subscribe_permission(&task.capability_token.permissions, &filter)` — fail-closed
3. Build `EventSubscription { agent_id: task.agent_id, ... }` and call `event_bus.subscribe`
4. Audit `EventSubscriptionCreated`

`execute_event_unsubscribe`:
1. Parse subscription_id
2. Look up via `event_bus.get_subscription` — must belong to `task.agent_id`
3. Call `event_bus.unsubscribe`

`execute_event_list_subscriptions`:
1. Always scoped to `task.agent_id` (no `target_agent` parameter)

`execute_event_list_available`:
1. Returns the static category→type table plus, for each category, a `subscribable: bool` based on the agent's current permissions

### New tools

| Tool | Manifest | Returns |
|------|----------|---------|
| `event-subscribe` | `tools/core/event-subscribe.toml` | `_kernel_action: event_subscribe` |
| `event-unsubscribe` | `tools/core/event-unsubscribe.toml` | `_kernel_action: event_unsubscribe` |
| `event-list-subscriptions` | `tools/core/event-list-subscriptions.toml` | `_kernel_action: event_list_subscriptions` |
| `event-list-available` | `tools/core/event-list-available.toml` | `_kernel_action: event_list_available` |

All four declare `required_permissions = vec![("events.stream", PermissionOp::Observe)]` as a coarse gate.

### Agent manual

`section_events()` ([agent_manual.rs](../../crates/agentos-tools/src/agent_manual.rs)) is updated to:

- Add a `permission` field to each category entry, telling the agent which observe permission is required
- Add a `tools` block listing the four self-subscription tools with their JSON shapes
- Replace the cryptic `subscribe_hint` with a step-by-step "how to subscribe" walkthrough

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/event_permissions.rs` | NEW — permission mapping + check function |
| `crates/agentos-kernel/src/lib.rs` | declare new module |
| `crates/agentos-kernel/src/event_bus.rs` | add `event_observe_permissions_for_role` |
| `crates/agentos-kernel/src/commands/agent.rs` | grant observe perms in default + role seeding |
| `crates/agentos-kernel/src/kernel_action.rs` | 4 variants + parsing + dispatch handlers |
| `crates/agentos-tools/src/event_subscribe.rs` | NEW tool stub |
| `crates/agentos-tools/src/event_unsubscribe.rs` | NEW tool stub |
| `crates/agentos-tools/src/event_list_subscriptions.rs` | NEW tool stub |
| `crates/agentos-tools/src/event_list_available.rs` | NEW tool stub |
| `crates/agentos-tools/src/lib.rs` | declare 4 new modules |
| `crates/agentos-tools/src/runner.rs` | register 4 new tools |
| `crates/agentos-tools/src/agent_manual.rs` | extend `section_events` |
| `tools/core/event-subscribe.toml` | NEW manifest |
| `tools/core/event-unsubscribe.toml` | NEW manifest |
| `tools/core/event-list-subscriptions.toml` | NEW manifest |
| `tools/core/event-list-available.toml` | NEW manifest |

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel event_permissions
cargo test -p agentos-tools
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

## Related

[[Multi-Agent Coordination Plan]] · [[Agent Manual Reference]]
