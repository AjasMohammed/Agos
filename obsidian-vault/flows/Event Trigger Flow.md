---
title: Event Trigger Flow
tags:
  - kernel
  - v3
  - flow
  - event-driven
date: 2026-03-11
status: in-progress
---

# Event Trigger Flow

> Data and control flow for the event-driven agent triggering system.

---

## End-to-End Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                     EVENT EMISSION                              │
│                                                                 │
│  cmd_connect_agent()                                            │
│  cmd_grant_permission()          ┌──────────────┐               │
│  task_executor (completion)  ──► │ emit_event() │               │
│  injection_scanner (detect)      └──────┬───────┘               │
│  scheduler (timeout)                    │                       │
│  HAL health (thresholds)                ▼                       │
│                              ┌─────────────────────┐            │
│                              │ Build EventMessage   │            │
│                              │ - Sign with HMAC     │            │
│                              │ - Audit log          │            │
│                              │ - Send to channel    │            │
│                              └─────────┬───────────┘            │
└────────────────────────────────────────┼────────────────────────┘
                                         │
                                         │ mpsc::UnboundedSender
                                         ▼
┌─────────────────────────────────────────────────────────────────┐
│              EVENT DISPATCHER (5th Supervised Task)              │
│                                                                 │
│  ┌─────────────────────┐                                        │
│  │ Receive EventMessage │◄─── mpsc::UnboundedReceiver            │
│  └─────────┬───────────┘                                        │
│            │                                                    │
│            ▼                                                    │
│  ┌─────────────────────────────────────┐                        │
│  │ Check chain_depth > max (5)?        │──► YES: Log loop, skip │
│  └─────────┬───────────────────────────┘                        │
│            │ NO                                                 │
│            ▼                                                    │
│  ┌─────────────────────────────────────┐                        │
│  │ EventBus.evaluate_subscriptions()   │                        │
│  │  - Match event type vs filter       │                        │
│  │  - Check subscription enabled       │                        │
│  │  - Evaluate throttle policy         │                        │
│  │  - Check event filter predicate     │                        │
│  └─────────┬───────────────────────────┘                        │
│            │                                                    │
│            ▼  For each matching subscription                    │
│  ┌─────────────────────────────────────┐                        │
│  │ build_trigger_prompt()              │                        │
│  │  - [SYSTEM CONTEXT]                 │                        │
│  │  - [EVENT NOTIFICATION]             │                        │
│  │  - [CURRENT OS STATE]               │                        │
│  │  - [AVAILABLE ACTIONS]              │                        │
│  │  - [GUIDANCE]                       │                        │
│  │  - [RESPONSE EXPECTATION]           │                        │
│  └─────────┬───────────────────────────┘                        │
│            │                                                    │
│            ▼                                                    │
│  ┌─────────────────────────────────────┐                        │
│  │ create_triggered_task()             │                        │
│  │  - Issue CapabilityToken            │                        │
│  │  - Build AgentTask with             │                        │
│  │    trigger_source = Some(...)       │                        │
│  │  - Enqueue to TaskScheduler         │                        │
│  │  - Audit log EventTriggeredTask     │                        │
│  └─────────┬───────────────────────────┘                        │
└────────────┼────────────────────────────────────────────────────┘
             │
             ▼
┌─────────────────────────────────────────────────────────────────┐
│              EXISTING TASK EXECUTION PIPELINE                   │
│                                                                 │
│  TaskScheduler.dequeue()                                        │
│       │                                                         │
│       ▼                                                         │
│  task_executor_loop() → execute_task()                          │
│       │                                                         │
│       ├─► LLM inference with trigger prompt                     │
│       ├─► Tool calls (capability-validated)                     │
│       ├─► Audit logging                                         │
│       └─► Task completion / failure                             │
│            │                                                    │
│            ▼                                                    │
│  Agent actions may emit new events (chain_depth + 1)            │
└─────────────────────────────────────────────────────────────────┘
```

## Throttle Evaluation Flow

```
EventMessage arrives
      │
      ▼
┌─────────────────────┐
│ Get subscription's   │
│ ThrottlePolicy       │
└──────┬──────────────┘
       │
       ├── None ──────────────────► DELIVER
       │
       ├── MaxOncePerDuration(d) ──► last_delivered + d < now?
       │                               YES → DELIVER
       │                               NO  → THROTTLE (audit log)
       │
       └── MaxCountPerDuration(n,d) → count_in_window < n?
                                       YES → DELIVER
                                       NO  → THROTTLE (audit log)
```

## Loop Detection

```
Event emitted with chain_depth = 0 (origin)
      │
      ▼
Agent task created → agent acts → emit_event(chain_depth=1)
      │
      ▼
Agent task created → agent acts → emit_event(chain_depth=2)
      │
      ...
      ▼
emit_event(chain_depth=5) → REJECTED: EventLoopDetected
```

## Related

- [[agentos-event-trigger-system]] — Design document
- [[13-Event Trigger System]] — Implementation plan
- [[Task Execution Flow]] — Existing task pipeline
