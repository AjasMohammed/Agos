---
title: "Phase 4: Full REST Endpoints"
tags:
  - api
  - rest
  - v3
  - phase-4
date: 2026-03-30
status: planned
effort: 2d
priority: high
---

# Phase 4: Full REST Endpoints

> Complete the REST API with chat, pipelines, audit, costs, and notification endpoints — achieving full parity with the HTML web UI's capabilities.

---

## Why This Phase

Phase 3 shipped the core CRUD endpoints. This phase adds the remaining domains that external consumers need: chat interaction, pipeline management, audit querying, cost monitoring, and notification handling. After this, anything the browser UI can do, an HTTP client can do.

## Current State

- `/api/v1/` router exists with agents, tasks, tools, secrets, system (Phase 3)
- `KernelService` trait has methods for all domains (Phase 1)
- Auth middleware + OpenAPI generation in place (Phases 2-3)

## Target State

- 18 additional REST endpoints across 5 domains
- Chat: create sessions, send messages, get conversations
- Pipelines: CRUD, run, import/export YAML
- Audit: query with filters, detail view
- Costs: summary, per-agent breakdown
- Notifications: list, detail, respond
- OpenAPI spec updated with all new schemas

## Detailed Subtasks

### 1. Chat endpoints

**New file: `crates/agentos-api/src/rest/chat.rs`**

```rust
pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/:id", get(get_conversation).post(send_message))
}

// GET /api/v1/chat/sessions
// Returns list of chat sessions with metadata (id, agent, created_at, message_count)
async fn list_sessions(
    State(state): State<Arc<ApiState>>,
    claims: AuthClaims,
) -> ApiResult<Vec<ChatSessionSummary>> {
    claims.require("chat:r")?;
    let sessions = state.service.list_chat_sessions().await?;
    Ok(ApiResponse::ok(sessions))
}

// POST /api/v1/chat/sessions
// Creates a new chat session with an agent
async fn create_session(
    State(state): State<Arc<ApiState>>,
    claims: AuthClaims,
    Json(req): Json<CreateSessionRequest>,
) -> ApiResult<ChatSessionSummary> {
    claims.require("chat:w")?;
    let session = state.service.create_chat_session(req).await?;
    Ok(ApiResponse::ok(session))
}

// GET /api/v1/chat/sessions/:id
// Returns full conversation with all messages
async fn get_conversation(
    State(state): State<Arc<ApiState>>,
    claims: AuthClaims,
    Path(id): Path<String>,
) -> ApiResult<ChatConversation> {
    claims.require("chat:r")?;
    let conversation = state.service.get_chat_conversation(&id).await?;
    Ok(ApiResponse::ok(conversation))
}

// POST /api/v1/chat/sessions/:id
// Sends a message and returns the full response (non-streaming)
// For streaming, use WebSocket (Phase 5)
async fn send_message(
    State(state): State<Arc<ApiState>>,
    claims: AuthClaims,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> ApiResult<ChatResponse> {
    claims.require("chat:w")?;
    let response = state.service.chat_send(ChatRequest {
        session_id: id,
        agent_name: req.agent_name,
        message: req.message,
        history: req.history.unwrap_or_default(),
    }).await?;
    Ok(ApiResponse::ok(response))
}
```

**New types in `types/chat.rs`:**
```rust
pub struct ChatSessionSummary {
    pub id: String,
    pub agent_name: String,
    pub created_at: DateTime<Utc>,
    pub message_count: u32,
    pub last_message_at: Option<DateTime<Utc>>,
}

pub struct CreateSessionRequest {
    pub agent_name: String,
}

pub struct SendMessageRequest {
    pub agent_name: Option<String>,
    pub message: String,
    pub history: Option<Vec<(String, String)>>,
}

pub struct ChatConversation {
    pub session: ChatSessionSummary,
    pub messages: Vec<ChatMessage>,
}

pub struct ChatMessage {
    pub role: String,  // "user" or "assistant"
    pub content: String,
    pub tool_calls: Vec<ToolCallSummary>,
    pub timestamp: DateTime<Utc>,
}
```

**KernelService additions:**
```rust
async fn list_chat_sessions(&self) -> Result<Vec<ChatSessionSummary>, ApiError>;
async fn create_chat_session(&self, req: CreateSessionRequest) -> Result<ChatSessionSummary, ApiError>;
async fn get_chat_conversation(&self, session_id: &str) -> Result<ChatConversation, ApiError>;
```

Note: Chat session storage currently lives in `agentos-web`'s `ChatStore` (SQLite). The `kernel_impl.rs` will need access to this store. Two options:
- Pass `ChatStore` to the `KernelService` impl (pragmatic, chosen for now)
- Move `ChatStore` into the kernel (cleaner but larger change, deferred)

### 2. Pipeline endpoints

**New file: `crates/agentos-api/src/rest/pipelines.rs`**

```rust
pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/", get(list_pipelines).post(save_pipeline))
        .route("/:name", delete(delete_pipeline))
        .route("/:name/run", post(run_pipeline))
        .route("/import", post(import_pipeline))
        .route("/export", post(export_pipeline))
}

// GET /api/v1/pipelines — List all pipelines with step counts
// POST /api/v1/pipelines — Save pipeline definition (JSON)
// DELETE /api/v1/pipelines/:name — Delete pipeline
// POST /api/v1/pipelines/:name/run — Run pipeline, returns result or run_id if detached
// POST /api/v1/pipelines/import — Import from YAML string
// POST /api/v1/pipelines/export — Export named pipeline to YAML
```

**Permissions:** `pipelines:r` for list/export, `pipelines:w` for save/delete/run/import

### 3. Audit endpoints

**New file: `crates/agentos-api/src/rest/audit.rs`**

```rust
pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/", get(query_audit))
        .route("/:trace_id", get(get_audit_detail))
}

// GET /api/v1/audit?limit=50&severity=error&from=2026-03-01&to=2026-03-30
// GET /api/v1/audit/:trace_id
```

**Query params:** `limit` (default 50, max 1000), `severity` (optional), `from`/`to` (ISO 8601 dates)
**Permissions:** `audit:r`

### 4. Cost endpoints

**New file: `crates/agentos-api/src/rest/costs.rs`**

```rust
pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/", get(get_cost_summary))
        .route("/:agent_name", get(get_agent_costs))
}

// GET /api/v1/costs — All agent cost snapshots
// GET /api/v1/costs/:agent_name — Single agent costs
```

**Permissions:** `costs:r`

### 5. Notification endpoints

**New file: `crates/agentos-api/src/rest/notifications.rs`**

```rust
pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/", get(list_notifications))
        .route("/:id", get(get_notification))
        .route("/:id/respond", post(respond_to_notification))
}

// GET /api/v1/notifications?unread_only=true&limit=20
// GET /api/v1/notifications/:id
// POST /api/v1/notifications/:id/respond { "text": "approved" }
```

**Permissions:** `notifications:r` for list/detail, `notifications:w` for respond

### 6. Update router and OpenAPI

Add new domain routers to `rest/mod.rs`:
```rust
.nest("/chat", chat::router())
.nest("/pipelines", pipelines::router())
.nest("/audit", audit::router())
.nest("/costs", costs::router())
.nest("/notifications", notifications::router())
```

Update `openapi.rs` to include all new paths and schemas.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-api/src/rest/mod.rs` | Register 5 new domain routers |
| `crates/agentos-api/src/rest/chat.rs` | 4 chat endpoints |
| `crates/agentos-api/src/rest/pipelines.rs` | 6 pipeline endpoints |
| `crates/agentos-api/src/rest/audit.rs` | 2 audit endpoints |
| `crates/agentos-api/src/rest/costs.rs` | 2 cost endpoints |
| `crates/agentos-api/src/rest/notifications.rs` | 3 notification endpoints |
| `crates/agentos-api/src/types/chat.rs` | Chat session/conversation types |
| `crates/agentos-api/src/types/pipelines.rs` | Pipeline request/response types |
| `crates/agentos-api/src/types/notifications.rs` | Notification filter/response types |
| `crates/agentos-api/src/service.rs` | Add chat session methods to trait |
| `crates/agentos-api/src/kernel_impl.rs` | Implement new methods |
| `crates/agentos-api/src/openapi.rs` | Register all new paths + schemas |
| `crates/agentos-api/tests/rest_integration.rs` | Tests for new endpoints |

## Dependencies

- **Requires:** Phase 3 (core REST infrastructure exists)
- **Blocks:** Phase 6 (web migration needs all service methods available)

## Test Plan

1. Chat: create session → send message → get conversation → verify history
2. Pipelines: save → list → export YAML → import YAML → run → delete
3. Audit: query with filters → verify limit/severity/date filtering → get detail by trace_id
4. Costs: get summary → verify per-agent snapshots → get single agent costs
5. Notifications: list → get detail → respond → verify marked as read
6. OpenAPI: spec includes all new paths and schemas
7. Permission enforcement: each domain requires correct permission string

## Verification

```bash
cargo build -p agentos-api
cargo test -p agentos-api
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
