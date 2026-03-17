---
title: "Phase 04 — Stale Docs Cleanup"
tags:
  - documentation
  - next-steps
  - v3
date: 2026-03-17
status: complete
effort: 1h
priority: medium
---

# Phase 04 — Stale Docs Cleanup

> Update frontmatter `status:` in 10 event-trigger-completion plan docs to reflect actual implementation state: 5 docs from `planned` to `complete`, 4 docs from `planned` to `partial`, 1 doc from `in-progress` to `partial`.

---

## Why This Phase

The `obsidian-vault/plans/event-trigger-completion/` directory contains 12 files for the 10-phase event trigger completion plan. The master plan file was correctly updated to `status: complete`, and Phase 04 (tool emission) is marked `complete`. However, 10 other files have stale `status:` frontmatter that does not reflect the actual implementation state. This misleads contributors and agents about what work remains.

**Evidence from code review:**

| Phase | Current Status | Actual Status | Evidence |
|-------|---------------|---------------|----------|
| 01 - Task Lifecycle Emission | `in-progress` | **partial** | TaskStarted/Completed/Failed/TimedOut/Delegated emitted. TaskRetrying/DeadlockDetected/TaskPreempted NOT emitted. |
| 02 - Security Event Emission | `planned` | **partial** | PromptInjectionAttempt/CapabilityViolation/UnauthorizedToolAccess emitted. SecretsAccessAttempt/SandboxEscapeAttempt/AuditLogTamperAttempt/AgentImpersonationAttempt/UnverifiedToolInstalled NOT emitted. |
| 03 - Security Trigger Prompts | `planned` | **complete** | trigger_prompt.rs has custom prompts for CapabilityViolation, PromptInjectionAttempt, UnauthorizedToolAccess. All security events that ARE emitted have custom prompts. |
| 04 - Tool Event Emission | `complete` | **complete** | Already correct. No change needed. |
| 05 - Memory Event Emission | `planned` | **partial** | ContextWindowNearLimit/EpisodicMemoryWritten/SemanticMemoryConflict emitted. ContextWindowExhausted/MemorySearchFailed/WorkingMemoryEviction NOT emitted. |
| 06 - Comms & Schedule | `planned` | **partial** | DirectMessageReceived/BroadcastReceived/MessageDeliveryFailed/DelegationReceived/CronJobFired/ScheduledTaskMissed/ScheduledTaskFailed emitted. DelegationResponseReceived/AgentUnreachable/ScheduledTaskCompleted NOT emitted. |
| 07 - Event Filter Predicates | `planned` | **complete** | `evaluate_filter()` fully implemented in event_bus.rs with 15+ unit tests. DSL supports `==`, `>`, `<`, `>=`, `<=`, `IN`, `!=`, `CONTAINS`, `AND`, nested paths. |
| 08 - Dynamic Subs & Defaults | `planned` | **complete** | `IntentType::Subscribe`/`Unsubscribe` handled in task_executor.rs (lines 118-230). `default_subscriptions_for_role()` implemented in event_bus.rs, called in cmd_connect_agent(). |
| 09 - Remaining Trigger Prompts | `planned` | **complete** | trigger_prompt.rs has 14 custom prompts covering all major event types. Remaining types use generic fallback (by design). |
| 10 - System Health & HW | `planned` | **partial** | CPUSpikeDetected/MemoryPressure/DiskSpaceLow/DiskSpaceCritical/GPUMemoryPressure emitted from health_monitor.rs. ProcessCrashed/NetworkInterfaceDown/ContainerResourceQuotaExceeded/KernelSubsystemError/GPUAvailable/SensorReadingThresholdExceeded/DeviceConnected/DeviceDisconnected/HardwareAccessGranted NOT emitted. |
| Data Flow doc | `planned` | **complete** | The document describes the event flow architecture which is fully implemented. |

---

## Current --> Target State

| File | Current `status:` | Target `status:` |
|------|-------------------|------------------|
| `01-task-lifecycle-emission.md` | `in-progress` | `partial` |
| `02-security-event-emission.md` | `planned` | `partial` |
| `03-security-trigger-prompts.md` | `planned` | `complete` |
| `05-memory-event-emission-and-prompt.md` | `planned` | `partial` |
| `06-communication-and-schedule-emission.md` | `planned` | `partial` |
| `07-event-filter-predicates.md` | `planned` | `complete` |
| `08-dynamic-subscriptions-and-role-defaults.md` | `planned` | `complete` |
| `09-remaining-trigger-prompts.md` | `planned` | `complete` |
| `10-system-health-and-hardware-emission.md` | `planned` | `partial` |
| `Event Trigger Completion Data Flow.md` | `planned` | `complete` |

---

## What to Do

For each file listed above, open it and change the `status:` line in the YAML frontmatter.

### 1. `obsidian-vault/plans/event-trigger-completion/01-task-lifecycle-emission.md`

Change line 9:
```yaml
status: in-progress
```
to:
```yaml
status: partial
```

### 2. `obsidian-vault/plans/event-trigger-completion/02-security-event-emission.md`

Change line 10:
```yaml
status: planned
```
to:
```yaml
status: partial
```

### 3. `obsidian-vault/plans/event-trigger-completion/03-security-trigger-prompts.md`

Change line 10:
```yaml
status: planned
```
to:
```yaml
status: complete
```

### 4. `obsidian-vault/plans/event-trigger-completion/05-memory-event-emission-and-prompt.md`

Change line 10:
```yaml
status: planned
```
to:
```yaml
status: partial
```

### 5. `obsidian-vault/plans/event-trigger-completion/06-communication-and-schedule-emission.md`

Change line 11:
```yaml
status: planned
```
to:
```yaml
status: partial
```

### 6. `obsidian-vault/plans/event-trigger-completion/07-event-filter-predicates.md`

Change line 9:
```yaml
status: planned
```
to:
```yaml
status: complete
```

### 7. `obsidian-vault/plans/event-trigger-completion/08-dynamic-subscriptions-and-role-defaults.md`

Change line 9:
```yaml
status: planned
```
to:
```yaml
status: complete
```

### 8. `obsidian-vault/plans/event-trigger-completion/09-remaining-trigger-prompts.md`

Change line 9:
```yaml
status: planned
```
to:
```yaml
status: complete
```

### 9. `obsidian-vault/plans/event-trigger-completion/10-system-health-and-hardware-emission.md`

Change line 10:
```yaml
status: planned
```
to:
```yaml
status: partial
```

### 10. `obsidian-vault/plans/event-trigger-completion/Event Trigger Completion Data Flow.md`

Change line 9:
```yaml
status: planned
```
to:
```yaml
status: complete
```

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/plans/event-trigger-completion/01-task-lifecycle-emission.md` | `status: in-progress` --> `status: partial` |
| `obsidian-vault/plans/event-trigger-completion/02-security-event-emission.md` | `status: planned` --> `status: partial` |
| `obsidian-vault/plans/event-trigger-completion/03-security-trigger-prompts.md` | `status: planned` --> `status: complete` |
| `obsidian-vault/plans/event-trigger-completion/05-memory-event-emission-and-prompt.md` | `status: planned` --> `status: partial` |
| `obsidian-vault/plans/event-trigger-completion/06-communication-and-schedule-emission.md` | `status: planned` --> `status: partial` |
| `obsidian-vault/plans/event-trigger-completion/07-event-filter-predicates.md` | `status: planned` --> `status: complete` |
| `obsidian-vault/plans/event-trigger-completion/08-dynamic-subscriptions-and-role-defaults.md` | `status: planned` --> `status: complete` |
| `obsidian-vault/plans/event-trigger-completion/09-remaining-trigger-prompts.md` | `status: planned` --> `status: complete` |
| `obsidian-vault/plans/event-trigger-completion/10-system-health-and-hardware-emission.md` | `status: planned` --> `status: partial` |
| `obsidian-vault/plans/event-trigger-completion/Event Trigger Completion Data Flow.md` | `status: planned` --> `status: complete` |

---

## Prerequisites

None -- this phase is purely documentation and can be done at any time.

---

## Test Plan

No code changes, so no tests needed. Verification is visual/structural.

---

## Verification

```bash
# Verify all updated statuses:
grep -r "^status:" obsidian-vault/plans/event-trigger-completion/ | sort

# Expected output:
# 01-task-lifecycle-emission.md:status: partial
# 02-security-event-emission.md:status: partial
# 03-security-trigger-prompts.md:status: complete
# 04-tool-event-emission.md:status: complete
# 05-memory-event-emission-and-prompt.md:status: partial
# 06-communication-and-schedule-emission.md:status: partial
# 07-event-filter-predicates.md:status: complete
# 08-dynamic-subscriptions-and-role-defaults.md:status: complete
# 09-remaining-trigger-prompts.md:status: complete
# 10-system-health-and-hardware-emission.md:status: partial
# Event Trigger Completion Data Flow.md:status: complete
# Event Trigger Completion Plan.md:status: complete

# Count: 7 complete, 5 partial, 0 planned, 0 in-progress
```

---

## Related

- [[Unwired Features Plan]] -- Parent plan
- [[Event Trigger Completion Plan]] -- The plan whose phases are being updated
- [[22-Unwired Features]] -- Next-steps parent index
