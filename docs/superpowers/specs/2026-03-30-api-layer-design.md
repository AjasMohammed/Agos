# AgentOS API Layer — Design Spec

> Extract a `KernelService` trait as the unified API boundary, build REST + WebSocket on top, and migrate the web UI to consume the trait — decoupling the web layer from kernel internals while enabling external integrations.

**Date:** 2026-03-30
**Status:** Approved
**Approach:** "Extract & Layer" (Approach A)

---

## Problem

The current `agentos-web` crate is an HTML admin panel that holds `Arc<Kernel>` and reaches directly into 35+ public fields — `scheduler`, `agent_registry`, `tool_registry`, `audit`, `cost_tracker`, `pipeline_engine`, `background_pool`, raw `started_at`, and more. This creates three problems:

1. **No external integration surface.** There is no JSON API. IDE plugins, third-party services, and agents themselves cannot interact with AgentOS programmatically. Only 4 ad-hoc `/api/*` routes exist (pipelines, costs, task trace).

2. **Tight coupling.** Web handlers aggregate data from 6+ kernel subsystems per page. Changes to kernel internals (renaming a field, restructuring a registry) break the web layer. There is no abstraction boundary.

3. **Duplicated logic.** The bus protocol (96 `KernelCommand` variants) and the web handlers implement overlapping functionality through different codepaths. Bug fixes must be applied in both places.

---

## Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | `KernelService` trait as the single API boundary | All consumers (REST, WebSocket, HTML handlers, bus dispatch) call the same trait. One place to test, mock, version. |
| 2 | REST + WebSocket (no gRPC) | REST covers 90% of use cases. WebSocket adds bidirectional real-time for streaming chat and event subscriptions. gRPC adds protobuf maintenance overhead not justified by current consumer base. |
| 3 | API keys + JWT authentication | API keys for service-to-service (stored in vault, scoped to PermissionSet). JWT for interactive sessions (1-hour expiry, RS256-signed). Universally understood, fits existing security model. |
| 4 | URL-prefixed versioning (`/api/v1/`) | One line in the router, avoids painful migration later. HTML routes stay unversioned. |
| 5 | New `agentos-api` crate | Owns the trait, DTOs, REST router, WebSocket handler, auth system, OpenAPI generation. Keeps `agentos-web` as a thin HTML skin. |
| 6 | WebSocket supplements SSE, doesn't replace it | HTML web UI keeps SSE (HTMX native). WebSocket is for programmatic consumers. Avoids rewriting templates. |
| 7 | Phased migration — never break the web UI | Each phase ships with existing HTML UI working. API layer is additive. Web handlers migrate last, one domain at a time. |

---

## Architecture

### Component Diagram

```
┌─────────────┐   ┌──────────────┐   ┌───────────────┐
│  External    │   │  Browser     │   │  CLI          │
│  Consumers   │   │  (HTML UI)   │   │  (agentctl)   │
│  (IDE, CI)   │   │              │   │               │
└──────┬───────┘   └──────┬───────┘   └───────┬───────┘
       │                  │                    │
  REST/WS API        HTML routes          Bus (UDS)
  /api/v1/*          /* (unchanged)       KernelCommand
       │                  │                    │
       ▼                  ▼                    ▼
┌─────────────────────────────────────────────────────┐
│              KernelService trait                      │
│  (agentos-api/src/service.rs)                        │
├─────────────────────────────────────────────────────┤
│              impl KernelService for Kernel            │
│  (agentos-api/src/kernel_impl.rs)                    │
├─────────────────────────────────────────────────────┤
│              Kernel internals                         │
│  (scheduler, registries, vault, audit, etc.)         │
└─────────────────────────────────────────────────────┘
```

### Dependency Graph (After)

```
agentos-api  ──→ agentos-kernel (kernel_impl.rs only)
             ──→ agentos-types, agentos-audit, agentos-vault

agentos-web  ──→ agentos-api (KernelService trait + API types)
             ──✗ agentos-kernel (removed)

agentos-cli  ──→ agentos-bus (unchanged)
```

---

## KernelService Trait

The trait is the central abstraction. All API consumers call these methods.

```rust
#[async_trait]
pub trait KernelService: Send + Sync {
    // --- Agents ---
    async fn list_agents(&self) -> Result<Vec<AgentSummary>, ApiError>;
    async fn connect_agent(&self, req: ConnectAgentRequest) -> Result<AgentSummary, ApiError>;
    async fn disconnect_agent(&self, agent_id: AgentID) -> Result<(), ApiError>;
    async fn get_agent_detail(&self, name: &str) -> Result<AgentDetail, ApiError>;
    async fn grant_permission(&self, req: PermissionRequest) -> Result<(), ApiError>;
    async fn revoke_permission(&self, req: PermissionRequest) -> Result<(), ApiError>;

    // --- Tasks ---
    async fn list_tasks(&self, filter: TaskFilter) -> Result<(Vec<TaskSummary>, u64), ApiError>; // (items, total)
    async fn get_task(&self, id: TaskID) -> Result<TaskDetail, ApiError>;
    async fn run_task(&self, req: RunTaskRequest) -> Result<TaskID, ApiError>;
    async fn cancel_task(&self, id: TaskID) -> Result<(), ApiError>;
    async fn get_task_trace(&self, id: TaskID) -> Result<TaskTrace, ApiError>;
    async fn stream_task_logs(&self, id: TaskID) -> Result<LogStream, ApiError>;

    // --- Tools ---
    async fn list_tools(&self) -> Result<Vec<ToolSummary>, ApiError>;
    async fn install_tool(&self, req: InstallToolRequest) -> Result<ToolID, ApiError>;
    async fn remove_tool(&self, name: &str) -> Result<(), ApiError>;

    // --- Secrets ---
    async fn list_secrets(&self) -> Result<Vec<SecretMetadata>, ApiError>;
    async fn set_secret(&self, req: SetSecretRequest) -> Result<(), ApiError>;
    async fn revoke_secret(&self, name: &str) -> Result<(), ApiError>;

    // --- Chat ---
    async fn list_chat_sessions(&self) -> Result<Vec<ChatSessionSummary>, ApiError>;
    async fn create_chat_session(&self, req: CreateSessionRequest) -> Result<ChatSessionSummary, ApiError>;
    async fn get_chat_conversation(&self, session_id: &str) -> Result<ChatConversation, ApiError>;
    async fn chat_send(&self, req: ChatRequest) -> Result<ChatResponse, ApiError>;
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ApiError>;

    // --- Pipelines ---
    async fn list_pipelines(&self) -> Result<Vec<PipelineSummary>, ApiError>;
    async fn save_pipeline(&self, req: SavePipelineRequest) -> Result<(), ApiError>;
    async fn run_pipeline(&self, req: RunPipelineRequest) -> Result<Value, ApiError>;
    async fn delete_pipeline(&self, name: &str) -> Result<(), ApiError>;
    async fn import_pipeline(&self, yaml: &str) -> Result<(), ApiError>;
    async fn export_pipeline(&self, name: &str) -> Result<String, ApiError>;

    // --- Audit ---
    async fn query_audit(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>, ApiError>;
    async fn get_audit_detail(&self, trace_id: &str) -> Result<AuditEntry, ApiError>;

    // --- Costs ---
    async fn get_cost_summary(&self) -> Result<CostSummary, ApiError>;
    async fn get_agent_costs(&self, agent_name: &str) -> Result<CostSnapshot, ApiError>;

    // --- Notifications ---
    async fn list_notifications(&self, filter: NotificationFilter) -> Result<Vec<UserMessage>, ApiError>;
    async fn get_notification(&self, id: NotificationID) -> Result<UserMessage, ApiError>;
    async fn respond_to_notification(&self, req: NotificationResponse) -> Result<(), ApiError>;
    async fn get_unread_count(&self) -> Result<u64, ApiError>;

    // --- Dashboard (composite) ---
    async fn get_dashboard_summary(&self) -> Result<DashboardSummary, ApiError>;

    // --- System ---
    async fn get_status(&self) -> Result<SystemStatus, ApiError>;
    async fn get_uptime(&self) -> Duration;
}
```

### API Error Type

```rust
pub struct ApiError {
    pub code: ErrorCode,      // Stable string enum: TaskNotFound, Unauthorized, etc.
    pub message: String,      // Human-readable
    pub status: StatusCode,   // HTTP status code
}

impl From<AgentOSError> for ApiError { ... }
impl IntoResponse for ApiError { ... }  // Renders JSON error envelope
```

### API DTO Types

Request/response types live in `agentos-api::types`. They are the API contract, decoupled from internal kernel types. Conversion happens in `kernel_impl.rs`.

Key types:
- `AgentSummary` / `AgentDetail` — flattened view of agent + permissions + cost
- `TaskSummary` / `TaskDetail` / `TaskFilter` — filterable task views with pagination
- `DashboardSummary` — composite: agent count, task counts by status, tool count, uptime, recent audit entries, background task count
- `PermissionRequest` — agent name + permission string for grant/revoke
- `ChatSessionSummary` / `ChatConversation` / `ChatRequest` / `ChatResponse` / `ChatStream` — chat types
- `CostSummary` / `CostSnapshot` — cost aggregation views
- `ApiKey` / `AuthClaims` / `TokenRequest` / `TokenResponse` — auth types

---

## REST API

### Route Structure

```
/api/v1/
├── /auth
│   ├── POST /token              — Exchange API key for JWT
│   └── POST /refresh            — Refresh JWT
├── /agents
│   ├── GET  /                   — List agents
│   ├── POST /                   — Connect agent
│   ├── GET  /:name              — Agent detail
│   ├── DELETE /:name            — Disconnect agent
│   ├── POST /:name/permissions  — Grant permission
│   └── DELETE /:name/permissions — Revoke permission
├── /tasks
│   ├── GET  /                   — List (query: status, agent, limit, offset)
│   ├── POST /                   — Run task
│   ├── GET  /:id                — Task detail
│   ├── DELETE /:id              — Cancel task
│   └── GET  /:id/trace          — Execution trace
├── /tools
│   ├── GET  /                   — List tools
│   ├── POST /                   — Install tool
│   └── DELETE /:name            — Remove tool
├── /secrets
│   ├── GET  /                   — List (metadata only)
│   ├── POST /                   — Create secret
│   └── DELETE /:name            — Revoke secret
├── /chat
│   ├── GET  /sessions           — List sessions
│   ├── POST /sessions           — Create session
│   ├── GET  /sessions/:id       — Get conversation
│   └── POST /sessions/:id       — Send message
├── /pipelines
│   ├── GET  /                   — List
│   ├── POST /                   — Save
│   ├── DELETE /:name            — Delete
│   ├── POST /:name/run          — Run
│   ├── POST /import             — Import YAML
│   └── POST /export             — Export YAML
├── /audit
│   ├── GET  /                   — Query (query: limit, severity, from, to)
│   └── GET  /:trace_id          — Detail
├── /costs
│   ├── GET  /                   — Summary
│   └── GET  /:agent_name        — Per-agent
├── /notifications
│   ├── GET  /                   — List (query: unread_only, limit)
│   ├── GET  /:id                — Detail
│   └── POST /:id/respond        — Respond
├── /system
│   └── GET  /status             — System status + uptime
├── /openapi.json                — OpenAPI 3.1 spec
└── /ws                          — WebSocket upgrade
```

### Response Envelope

```json
// Success (single item)
{ "data": { ... } }

// Success (list with pagination)
{ "data": [ ... ], "meta": { "total": 142, "limit": 50, "offset": 0 } }

// Error
{ "error": { "code": "TASK_NOT_FOUND", "message": "Task abc-123 not found", "status": 404 } }
```

### Pagination

List endpoints accept `?limit=50&offset=0`. Default limit: 50, max: 200. Response includes `meta.total`.

### Handler Pattern

Every REST handler is thin — service trait call + response envelope:

```rust
async fn list_agents(
    State(svc): State<Arc<dyn KernelService>>,
    claims: AuthClaims,
) -> ApiResult<Vec<AgentSummary>> {
    claims.require("agents:r")?;
    let agents = svc.list_agents().await?;
    Ok(ApiResponse::ok(agents))
}
```

### OpenAPI

Using `utoipa` crate. `ToSchema` derived on all DTOs, `#[utoipa::path]` on handlers. Served at `GET /api/v1/openapi.json`.

---

## WebSocket Layer

### Endpoint

Single upgrade at `GET /api/v1/ws`. JWT passed as query param (`?token=eyJ...`) or `Authorization` header during upgrade.

### Protocol

JSON frames over the WebSocket connection.

**Client → Server:**

```json
{ "type": "subscribe", "channel": "tasks", "filter": { "status": "running" } }
{ "type": "unsubscribe", "subscription_id": "sub_1" }
{ "type": "chat.send", "session_id": "...", "message": "..." }
{ "type": "chat.cancel", "session_id": "..." }
{ "type": "task.cancel", "task_id": "..." }
{ "type": "notification.respond", "id": "...", "text": "..." }
{ "type": "ping" }
```

**Server → Client:**

```json
{ "type": "subscribed", "channel": "tasks", "subscription_id": "sub_1" }
{ "type": "event", "channel": "tasks", "event": "task.updated", "data": { ... } }
{ "type": "chat.chunk", "session_id": "...", "delta": "Hello" }
{ "type": "chat.done", "session_id": "...", "tool_calls": [...] }
{ "type": "error", "code": "INVALID_CHANNEL", "message": "..." }
{ "type": "pong" }
```

### Channels

| Channel | Events | Description |
|---------|--------|-------------|
| `dashboard` | `stats.updated` | Composite stats snapshot |
| `agents` | `agent.connected`, `agent.disconnected`, `agent.status` | Agent lifecycle |
| `tasks` | `task.created`, `task.updated`, `task.completed`, `task.failed` | Task state changes |
| `tasks:{id}` | `log.line`, `task.updated` | Single task log stream |
| `notifications` | `notification.new`, `notification.read` | Notification pushes |
| `pipelines:{run_id}` | `step.started`, `step.completed`, `pipeline.done` | Pipeline run |
| `costs` | `cost.updated` | Cost changes |

### Bidirectional Actions

| Action | Purpose |
|--------|---------|
| `chat.send` | Send message, receive streaming chunks |
| `chat.cancel` | Cancel in-progress inference |
| `task.cancel` | Cancel running task |
| `notification.respond` | Respond to ask-user |

### Connection Management

- Per-connection `WsSession` holds subscription state + `mpsc` sender
- `WsBroadcaster` fans out from kernel `event_bus` and `status_update_sender` to subscribed sessions
- Backpressure: buffer 256 messages per connection, drop oldest on overflow
- Heartbeat: server ping every 30s, disconnect after 90s without pong
- Auth: JWT validated on upgrade; connection closed if token expires (client must reconnect with refreshed token)

---

## Authentication

### API Keys

- Format: `agos_` + 32 random bytes (hex) = 68 characters
- Stored: key hash (SHA-256) in `api_keys` SQLite table; plaintext shown once on creation
- Scoped: each key has a `PermissionSet` (reuses existing capability system)
- Metadata: `name`, `key_hash`, `permissions`, `created_at`, `last_used_at`, `expires_at`
- Management: `agentctl auth create-key`, `list-keys`, `revoke-key`
- Audit: creation, usage, and revocation logged

### JWT

- Algorithm: RS256 (RSA key pair generated at first boot, stored in vault)
- Lifetime: 1 hour access token, 24 hour refresh token
- Claims: `sub` (key name), `permissions` (ceiling from API key), `iat`, `exp`, `jti`
- Revocation: `jti` tracked in a revocation set (in-memory, bounded)
- Exchange: `POST /api/v1/auth/token` with API key → JWT pair
- Refresh: `POST /api/v1/auth/refresh` with refresh token → new access token

### Middleware

```rust
async fn api_auth_middleware(req: Request, next: Next) -> Response {
    let claims = extract_and_validate_jwt(&req)?;
    req.extensions_mut().insert(claims);
    next.run(req).await
}
```

Permission checked per-handler via `claims.require("resource:rwx")`.

### Backwards Compatibility

The existing startup bearer token and session cookie flow continue to work for the HTML web UI. API key auth is a separate path. Both coexist.

---

## Crate Structure

```
crates/agentos-api/
├── Cargo.toml
├── src/
│   ├── lib.rs                    # Re-exports
│   ├── service.rs                # KernelService trait
│   ├── kernel_impl.rs            # impl KernelService for Kernel
│   ├── types/
│   │   ├── mod.rs
│   │   ├── agents.rs             # AgentSummary, AgentDetail, ConnectAgentRequest
│   │   ├── tasks.rs              # TaskSummary, TaskDetail, TaskFilter, RunTaskRequest
│   │   ├── tools.rs              # ToolSummary, InstallToolRequest
│   │   ├── secrets.rs            # SetSecretRequest
│   │   ├── chat.rs               # ChatRequest, ChatResponse, ChatStream
│   │   ├── pipelines.rs          # PipelineSummary, SavePipelineRequest
│   │   ├── audit.rs              # AuditFilter
│   │   ├── costs.rs              # CostSummary
│   │   ├── notifications.rs      # NotificationFilter, NotificationResponse
│   │   ├── system.rs             # SystemStatus, DashboardSummary
│   │   └── auth.rs               # ApiKey, AuthClaims, TokenRequest, TokenResponse
│   ├── error.rs                  # ApiError with HTTP status mapping
│   ├── rest/
│   │   ├── mod.rs                # REST router builder
│   │   ├── agents.rs
│   │   ├── tasks.rs
│   │   ├── tools.rs
│   │   ├── secrets.rs
│   │   ├── chat.rs
│   │   ├── pipelines.rs
│   │   ├── audit.rs
│   │   ├── costs.rs
│   │   ├── notifications.rs
│   │   ├── system.rs
│   │   └── auth.rs
│   ├── ws/
│   │   ├── mod.rs                # WebSocket upgrade handler
│   │   ├── session.rs            # WsSession per-connection state
│   │   ├── protocol.rs           # Frame types
│   │   └── broadcaster.rs        # Fan-out from kernel events
│   ├── auth/
│   │   ├── mod.rs
│   │   ├── api_keys.rs           # Key storage, hashing, validation
│   │   ├── jwt.rs                # Sign, verify, refresh, revocation
│   │   └── middleware.rs         # Axum JWT extraction + permission check
│   └── openapi.rs                # utoipa spec generation
```

---

## Migration Strategy

### Phase Dependency Graph

```
Phase 1 (trait) → Phase 2 (auth) → Phase 3 (core REST) → Phase 4 (full REST)
                                          │                      │
                                          └──→ Phase 5 (WS) ────┘
                                                                  │
                                                      Phase 6 (web migration)
                                                                  │
                                                      Phase 7 (bus migration)
```

### Phase Summary

| Phase | Name | Effort | Dependencies | What Ships |
|-------|------|--------|-------------|------------|
| 1 | KernelService trait + impl | 3d | None | Trait, DTOs, kernel_impl, unit tests |
| 2 | Auth system | 2d | Phase 1 | API keys in vault, JWT, CLI commands, middleware |
| 3 | Core REST endpoints | 3d | Phase 2 | agents, tasks, tools, secrets, system + OpenAPI |
| 4 | Full REST endpoints | 2d | Phase 3 | chat, pipelines, audit, costs, notifications |
| 5 | WebSocket layer | 3d | Phase 3 | WsSession, broadcaster, channels, bidirectional actions |
| 6 | Web handler migration | 3d | Phase 4, 5 | agentos-web decoupled, no kernel dependency |
| 7 | Bus dispatch migration | 2d | Phase 1 | run_loop.rs uses KernelService, cmd_* removed |

**Total: ~18 working days**

Phases 3→4 and 3→5 can run in parallel. Phase 7 can start after Phase 1 (independent of REST/WS work).

### Risk Mitigations

| Risk | Mitigation |
|------|------------|
| Web UI breaks during migration | Phase 6 migrates one handler file at a time; each commit is deployable |
| DTO explosion | Start with flat structs; only nest when warranted by real endpoints |
| OpenAPI spec drift | utoipa derives from code — spec can't diverge |
| WebSocket complexity | Phase 5 starts with subscribe/event; bidirectional actions added incrementally |
| Performance regression | `Arc<dyn KernelService>` is one vtable lookup — negligible vs network I/O |

---

## Testing Strategy

### Unit Tests (per phase)
- `KernelService` methods tested against booted kernel with `MockLLMCore`
- API key creation, validation, revocation
- JWT signing, verification, expiry, refresh
- DTO conversions (internal types ↔ API types)

### Integration Tests
- Full HTTP round-trips: create API key → exchange for JWT → call REST endpoints → verify responses
- WebSocket: connect → subscribe → trigger kernel event → verify frame received
- Auth enforcement: verify 401 without token, 403 without permission
- Pagination: verify `meta.total`, `limit`, `offset` behavior

### Existing Tests Preserved
- XSS tests continue to pass throughout Phase 6
- All `cargo test --workspace` green at every phase boundary

---

## Out of Scope

- gRPC — can be added later as a thin layer over KernelService
- Agent self-service UI — future work, enabled by this API layer
- CLI migration to HTTP — CLI stays on bus protocol
- Chat store unification — `chat.db` stays separate for now; can be moved into KernelService later
- Rate limiting per API key — uses existing global rate limiter initially
- Circular dependency resolution for Phase 7 — `agentos-kernel` needs API DTO types for bus response conversion; resolved via `service_self: Arc<dyn KernelService>` pattern on the Kernel struct (see Phase 7 detail doc)
