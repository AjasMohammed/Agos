---
title: API Layer Research
tags:
  - api
  - kernel
  - web
  - v3
  - research
date: 2026-03-30
status: complete
effort: 1d
priority: high
---

# API Layer Research

> Analysis of current coupling patterns between `agentos-web` and kernel internals, bus protocol coverage, and API surface gaps.

---

## Current Web Handler Coupling Analysis

### Clean Access Patterns (no changes needed)

These handlers use `api_*()` wrapper methods or well-abstracted interfaces:

| Handler | Access Pattern |
|---------|---------------|
| `agents.rs` | `api_connect_agent()`, `api_disconnect_agent()`, registry read |
| `secrets.rs` | `api_set_secret()`, `api_revoke_secret()`, vault list |
| `tools.rs` | `api_install_tool()`, `api_remove_tool()`, registry read |
| `chat.rs` | `chat_infer_with_tools()`, `chat_infer_streaming()`, registry read |
| `notifications.rs` | `notification_router.inbox()`, `route_response()` |

### Problematic Access Patterns (must fix)

| Handler | Problem | What it accesses directly |
|---------|---------|--------------------------|
| `dashboard.rs` | Aggregates 6 subsystems | `agent_registry`, `tool_registry`, `scheduler`, `audit`, `background_pool`, `started_at` |
| `events.rs` (SSE) | Polls 6 subsystems every 2-3s | Same as dashboard + continuous loop |
| `tasks.rs` | Direct mutation | `scheduler.update_state()` bypasses kernel logic |
| `pipelines.rs` | Direct store access | `pipeline_engine.store_arc()` then raw store calls |
| `pipeline_ui.rs` | Heavy store coupling | 6 direct `store_arc()` call sites |
| `agent_detail.rs` | Multi-subsystem query | `agent_registry` + `scheduler` + `cost_tracker` |
| `audit.rs` | Direct audit queries | `audit.query_recent()`, `query_by_trace()` |
| `costs.rs` | Direct tracker access | `cost_tracker.get_all_snapshots()` + `scheduler` |

### Raw Field Access (worst offenders)

- `state.kernel.started_at` — raw `DateTime<Utc>` field read (dashboard.rs, events.rs)
- `state.kernel.background_pool.list_running()` — direct pool query (dashboard.rs, events.rs)
- `state.kernel.audit.clone()` — cloning Arc for blocking spawn (dashboard.rs, tasks.rs, events.rs)

## Bus Protocol Coverage

The bus has 96 `KernelCommand` variants. The web layer currently exercises ~30 of them indirectly (through direct kernel access, not through the bus). Commands the web layer doesn't use but external consumers would want:

| Command Group | Commands | External Value |
|---------------|----------|----------------|
| Role management | `CreateRole`, `DeleteRole`, `ListRoles`, `RoleGrant`, `AssignRole` | IDE permission management |
| Scheduling | `CreateSchedule`, `ListSchedules`, `PauseSchedule`, `DeleteSchedule` | CI/CD automation |
| Resource locks | `ListResourceLocks`, `ReleaseResourceLock` | Debugging, monitoring |
| Snapshots | `ListSnapshots`, `RollbackTask` | Task recovery |
| HAL devices | `HalListDevices`, `HalApproveDevice` | Hardware monitoring |
| Event system | `EventSubscribe`, `EventHistory` | Agent orchestration |
| Context memory | `ContextMemoryRead`, `ContextMemoryHistory` | Agent debugging |
| Scratchpad | `ScratchListPages`, `ScratchReadPage` | Agent workspace access |

These can be added to `KernelService` incrementally as external consumers request them.

## Authentication Gap Analysis

| Current | Problem |
|---------|---------|
| Single bearer token printed at startup | Can't scope permissions per consumer |
| No key rotation | Compromised token = full access until restart |
| No audit of API access | Can't trace who did what via the API |
| Session cookie is HTTP-only | Can't be used by programmatic consumers |

API keys + JWT solves all four: scoped permissions, key revocation, audit logging per key, and stateless tokens for programmatic access.

## Technology Choices

| Need | Choice | Rationale |
|------|--------|-----------|
| REST framework | Axum (existing) | Already a dependency, excellent extractors |
| WebSocket | `axum::extract::ws` | Built into Axum, no new dependency |
| JWT | `jsonwebtoken` crate | Widely used, RS256 support, good Serde integration |
| OpenAPI | `utoipa` crate | Derive macros, Axum integration, generates 3.1 spec |
| API key hashing | SHA-256 (existing `sha2` dep) | Already in the workspace |

## Related

- [[API Layer Plan]] — master plan
- [[API Layer Data Flow]] — request lifecycle
