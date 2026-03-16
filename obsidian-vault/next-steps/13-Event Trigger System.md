---
title: Event-Driven Agent Triggering System
tags:
  - kernel
  - v3
  - feature
  - next-steps
date: 2026-03-11
status: complete
effort: 3d
priority: high
---

# Event-Driven Agent Triggering System

> Transform agents from passive responders into active participants by emitting typed OS events that trigger agent tasks via rich contextual prompts.

---

## Current State

Agents only respond to explicit user messages (`agentctl task run`) or scheduled cron tasks (`agentd`). There is no mechanism for internal OS state changes to automatically wake agents. The audit log records events but nothing acts on them programmatically.

## Goal / Target State

Every significant state change inside AgentOS emits a typed `EventMessage`. Agents subscribe to events they care about. When an event fires, the kernel constructs a rich trigger prompt and creates a fresh `AgentTask` delivered to the subscribed agent. The agent wakes up, reasons about the situation, and decides what to do.

Phase 1 delivers: AgentLifecycle events (AgentAdded, AgentRemoved, PermissionGranted, PermissionRevoked), the EventBus kernel subsystem, trigger prompt construction, CLI commands, and end-to-end connectivity.

## Step-by-Step Plan

### Phase 1: Foundation (Minimum Viable Event System)

1. **Define core event types** in `agentos-types/src/event.rs` — `EventCategory`, `EventType`, `EventSource`, `EventSeverity`, `EventMessage`, `EventSubscription`, `EventTypeFilter`, `ThrottlePolicy`, `SubscriptionPriority`. Add `EventID` and `SubscriptionID` to `ids.rs`. `cargo build -p agentos-types`

2. **Add `TriggerSource` to `AgentTask`** — new optional field tracking event provenance. Fix all struct literals across the codebase. `cargo build --workspace`

3. **Add error variants** — `EventSubscriptionNotFound`, `EventLoopDetected`, `EventDeliveryFailed` in `error.rs`. `cargo build -p agentos-types`

4. **Add audit event types** — 8 new variants: `EventEmitted`, `EventSubscriptionCreated`, `EventSubscriptionRemoved`, `EventDelivered`, `EventThrottled`, `EventFilterRejected`, `EventLoopDetected`, `EventTriggeredTask`. `cargo build --workspace`

5. **Add HMAC signing helpers** — `sign_data()` and `verify_data_signature()` on `CapabilityEngine`. `cargo build -p agentos-capability`

6. **Build EventBus subsystem** — `event_bus.rs` in kernel: subscription registry, filter evaluation, throttle enforcement. Unit tests inline. `cargo test -p agentos-kernel`

7. **Build event dispatch** — Channel-based: `emit_event` pushes to `mpsc` channel, `EventDispatcher` (5th supervised task) consumes and creates triggered tasks. `cargo build -p agentos-kernel`

8. **Build trigger prompt system** — `trigger_prompt.rs`: structured prompt builder with [SYSTEM CONTEXT], [EVENT NOTIFICATION], [CURRENT OS STATE], [AVAILABLE ACTIONS], [GUIDANCE], [RESPONSE EXPECTATION]. Phase 1 prompts for 4 AgentLifecycle events. `cargo build -p agentos-kernel`

9. **Integrate into Kernel struct and run loop** — Add `event_bus` and `event_sender` fields to Kernel, initialize in `boot()`, add `EventDispatcher` supervised task. `cargo build --workspace`

10. **Add bus protocol** — 7 `KernelCommand::Event*` variants + 3 `KernelResponse` variants. Wire `handle_command` match arms. `cargo build --workspace`

11. **Add kernel command handlers** — `commands/event.rs` with subscribe/unsubscribe/list/get/enable/disable/history handlers. `cargo build --workspace`

12. **Wire event emission into AgentLifecycle** — `cmd_connect_agent` → AgentAdded, `cmd_disconnect_agent` → AgentRemoved, `cmd_grant_permission` → PermissionGranted, `cmd_revoke_permission` → PermissionRevoked. `cargo build --workspace`

13. **Add CLI event command group** — `agentctl event subscribe/unsubscribe/subscriptions/history`. `cargo build --workspace`

14. **Tests and connectivity verification** — Unit tests, CLI parse tests, integration tests for full event flow. `cargo test --workspace`

### Phases 2–5: Completed via `plans/event-trigger-completion/`

All remaining phases were delegated to the 10-phase completion plan and are now implemented.
See [[Event Trigger Completion Plan]] for details.

| Phase | Delegated sub-plan | Status |
|-------|--------------------|--------|
| Security & Task Events | [[02-security-event-emission]], [[03-security-trigger-prompts]] | complete |
| Tool Event Emission | [[04-tool-event-emission]] | complete |
| Memory Events | [[05-memory-event-emission-and-prompt]] | complete |
| Communication & Schedule Events | [[06-communication-and-schedule-emission]] | complete |
| Filter Predicates | [[07-event-filter-predicates]] | complete |
| Dynamic Subscriptions & Role Defaults | [[08-dynamic-subscriptions-and-role-defaults]] | complete |
| Remaining Trigger Prompts | [[09-remaining-trigger-prompts]] | complete |
| System Health & Hardware Events | [[10-system-health-and-hardware-emission]] | complete |

## Files Changed

| File | Action | Changes |
|------|--------|---------|
| `crates/agentos-types/src/event.rs` | Create | All event type definitions |
| `crates/agentos-types/src/ids.rs` | Modify | Add EventID, SubscriptionID |
| `crates/agentos-types/src/task.rs` | Modify | Add trigger_source field |
| `crates/agentos-types/src/error.rs` | Modify | Add 3 error variants |
| `crates/agentos-types/src/lib.rs` | Modify | Re-export event module |
| `crates/agentos-audit/src/log.rs` | Modify | Add 8 AuditEventType variants |
| `crates/agentos-capability/src/engine.rs` | Modify | Add sign_data methods |
| `crates/agentos-bus/src/message.rs` | Modify | Add Event command/response variants |
| `crates/agentos-kernel/src/event_bus.rs` | Create | EventBus subsystem |
| `crates/agentos-kernel/src/event_dispatch.rs` | Create | Channel-based dispatch |
| `crates/agentos-kernel/src/trigger_prompt.rs` | Create | Trigger prompt construction |
| `crates/agentos-kernel/src/commands/event.rs` | Create | Event command handlers |
| `crates/agentos-kernel/src/kernel.rs` | Modify | Add event_bus, event_sender |
| `crates/agentos-kernel/src/run_loop.rs` | Modify | Add EventDispatcher task |
| `crates/agentos-cli/src/commands/event.rs` | Create | CLI event commands |
| `crates/agentos-cli/src/main.rs` | Modify | Add Event variant |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# End-to-end: subscribe agent, connect another, verify triggered task in audit log
```

## Related

- [[agentos-event-trigger-system]] — Full design document
- [[11-Spec Enforcement Hardening]] — Escalation expiry, permission hardening
- [[Event Trigger Flow]] — Data/control flow diagram
