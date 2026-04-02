---
title: "Phase 3: Core REST Endpoints"
tags:
  - api
  - rest
  - v3
  - phase-3
date: 2026-03-30
status: planned
effort: 3d
priority: high
---

# Phase 3: Core REST Endpoints

> Build the REST router at `/api/v1/` with the highest-value endpoints: agents, tasks, tools, secrets, system status, and OpenAPI spec generation.

---

## Why This Phase

These five domains cover the core operations every external consumer needs: manage agents, run and monitor tasks, discover tools, handle secrets, and check system health. Shipping these first gives IDE plugins and CI/CD a usable integration surface.

## Current State

- No REST API exists (only 4 ad-hoc `/api/*` HTML/JSON hybrid routes)
- `KernelService` trait exists (Phase 1)
- Auth system exists (Phase 2)
- All endpoint logic currently lives in `agentos-web` HTML handlers

## Target State

- REST router mounted at `/api/v1/` alongside existing HTML routes
- 20 JSON endpoints: agents (6), tasks (5), tools (3), secrets (3), system (1), auth (2)
- Consistent JSON response envelope with pagination
- OpenAPI 3.1 spec at `/api/v1/openapi.json`
- Integration tests with full HTTP round-trips

## Detailed Subtasks

### 1. Add REST infrastructure dependencies

Add to `crates/agentos-api/Cargo.toml`:
```toml
utoipa = { version = "5", features = ["axum_extras", "chrono", "uuid"] }
utoipa-axum = "0.2"
```

### 2. Response envelope and extractors

**New file: `crates/agentos-api/src/rest/mod.rs`**

```rust
use axum::{Router, Json, response::IntoResponse};
use serde::Serialize;

/// Standard JSON response envelope
#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<PaginationMeta>,
}

#[derive(Serialize)]
pub struct PaginationMeta {
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

/// Standard error response
#[derive(Serialize)]
pub struct ApiErrorResponse {
    pub error: ApiErrorBody,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Json<Self> {
        Json(Self { data, meta: None })
    }

    pub fn paginated(data: T, total: u64, limit: u32, offset: u32) -> Json<Self> {
        Json(Self {
            data,
            meta: Some(PaginationMeta { total, limit, offset }),
        })
    }
}

/// Result type for handlers
pub type ApiResult<T> = Result<Json<ApiResponse<T>>, ApiError>;

/// Pagination query params
#[derive(Debug, Deserialize)]
pub struct Pagination {
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 { 50 }

impl Pagination {
    pub fn clamp(&self) -> (u32, u32) {
        (self.limit.min(200).max(1), self.offset)
    }
}

/// Build the /api/v1 router
pub fn router(
    service: Arc<dyn KernelService>,
    jwt_manager: Arc<JwtManager>,
    key_store: Arc<ApiKeyStore>,
    audit: Arc<AuditLog>,
) -> Router {
    let state = Arc::new(ApiState { service, jwt_manager, key_store, audit });

    Router::new()
        // Auth (no auth middleware - these endpoints issue tokens)
        .route("/auth/token", post(auth::exchange_token))
        .route("/auth/refresh", post(auth::refresh_token))
        // Protected routes
        .nest("/agents", agents::router())
        .nest("/tasks", tasks::router())
        .nest("/tools", tools::router())
        .nest("/secrets", secrets::router())
        .route("/system/status", get(system::get_status))
        .route("/openapi.json", get(openapi::spec))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api_auth_layer,
        ))
        .with_state(state)
}
```

### 3. Agent endpoints

**New file: `crates/agentos-api/src/rest/agents.rs`**

```rust
pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/", get(list_agents).post(connect_agent))
        .route("/:name", get(get_agent).delete(disconnect_agent))
        .route("/:name/permissions", post(grant_perm).delete(revoke_perm))
}

#[utoipa::path(
    get, path = "/api/v1/agents",
    responses((status = 200, body = Vec<AgentSummary>)),
    security(("bearer" = []))
)]
async fn list_agents(
    State(state): State<Arc<ApiState>>,
    claims: AuthClaims,
) -> ApiResult<Vec<AgentSummary>> {
    claims.require("agents:r")?;
    let agents = state.service.list_agents().await?;
    Ok(ApiResponse::ok(agents))
}

#[utoipa::path(
    post, path = "/api/v1/agents",
    request_body = ConnectAgentRequest,
    responses((status = 201, body = AgentSummary)),
    security(("bearer" = []))
)]
async fn connect_agent(
    State(state): State<Arc<ApiState>>,
    claims: AuthClaims,
    Json(req): Json<ConnectAgentRequest>,
) -> ApiResult<AgentSummary> {
    claims.require("agents:w")?;
    let agent = state.service.connect_agent(req).await?;
    Ok(ApiResponse::ok(agent))
}

// Similar pattern for get_agent, disconnect_agent, grant_perm, revoke_perm
```

### 4. Task endpoints

**New file: `crates/agentos-api/src/rest/tasks.rs`**

```rust
pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/", get(list_tasks).post(run_task))
        .route("/:id", get(get_task).delete(cancel_task))
        .route("/:id/trace", get(get_trace))
}

#[utoipa::path(
    get, path = "/api/v1/tasks",
    params(
        ("status" = Option<String>, Query, description = "Filter by status"),
        ("agent" = Option<String>, Query, description = "Filter by agent name"),
        ("limit" = Option<u32>, Query, description = "Max results (default 50, max 200)"),
        ("offset" = Option<u32>, Query, description = "Offset for pagination"),
    ),
    responses((status = 200, body = Vec<TaskSummary>)),
    security(("bearer" = []))
)]
async fn list_tasks(
    State(state): State<Arc<ApiState>>,
    claims: AuthClaims,
    Query(pagination): Query<Pagination>,
    Query(filter): Query<TaskFilterParams>,
) -> ApiResult<Vec<TaskSummary>> {
    claims.require("tasks:r")?;
    let (limit, offset) = pagination.clamp();
    let filter = TaskFilter {
        status: filter.status,
        agent_name: filter.agent,
        limit: Some(limit),
        offset: Some(offset),
    };
    let (tasks, total) = state.service.list_tasks(filter).await?;
    Ok(ApiResponse::paginated(tasks, total, limit, offset))
}

#[utoipa::path(
    post, path = "/api/v1/tasks",
    request_body = RunTaskRequest,
    responses((status = 201, body = TaskID)),
    security(("bearer" = []))
)]
async fn run_task(
    State(state): State<Arc<ApiState>>,
    claims: AuthClaims,
    Json(req): Json<RunTaskRequest>,
) -> ApiResult<TaskID> {
    claims.require("tasks:w")?;
    let id = state.service.run_task(req).await?;
    Ok(ApiResponse::ok(id))
}

// Similar for get_task, cancel_task, get_trace
```

### 5. Tool, secret, and system endpoints

Follow identical patterns to agents/tasks. Each file:
- Defines a `router()` function returning `Router<Arc<ApiState>>`
- Each handler: extract claims → check permission → call service → wrap in envelope
- `#[utoipa::path]` on every handler for OpenAPI generation

**Tools permissions:** `tools:r` for list, `tools:w` for install/remove
**Secrets permissions:** `secrets:r` for list, `secrets:w` for set/revoke
**System permissions:** `system:r` for status

### 6. OpenAPI spec generation

**New file: `crates/agentos-api/src/openapi.rs`**

```rust
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(title = "AgentOS API", version = "1.0.0", description = "AgentOS kernel REST API"),
    paths(
        rest::agents::list_agents,
        rest::agents::connect_agent,
        rest::agents::get_agent,
        rest::agents::disconnect_agent,
        rest::tasks::list_tasks,
        rest::tasks::run_task,
        rest::tasks::get_task,
        rest::tasks::cancel_task,
        rest::tasks::get_trace,
        rest::tools::list_tools,
        rest::tools::install_tool,
        rest::tools::remove_tool,
        rest::secrets::list_secrets,
        rest::secrets::set_secret,
        rest::secrets::revoke_secret,
        rest::system::get_status,
    ),
    components(schemas(
        AgentSummary, AgentDetail, ConnectAgentRequest,
        TaskSummary, TaskDetail, TaskFilter, RunTaskRequest,
        ToolSummary, InstallToolRequest,
        SetSecretRequest,
        SystemStatus,
        ApiErrorBody,
    )),
    security(("bearer" = []))
)]
pub struct ApiDoc;

pub async fn spec() -> impl IntoResponse {
    Json(ApiDoc::openapi())
}
```

### 7. Mount API router in web server

**Edit: `crates/agentos-web/src/server.rs`**

```rust
// In WebServer::new():
let service: Arc<dyn KernelService> = Arc::new(kernel.clone());
let key_store = Arc::new(ApiKeyStore::new(kernel.data_dir())?);
let jwt_manager = Arc::new(JwtManager::new(&kernel.vault)?);
let api_router = agentos_api::rest::router(
    service, jwt_manager, key_store, kernel.audit.clone()
);

let app = Router::new()
    .nest("/api/v1", api_router)    // NEW: JSON API
    .merge(existing_html_router)     // Existing HTML routes unchanged
    .nest_service("/static", serve_dir);
```

### 8. Integration tests

**New file: `crates/agentos-api/tests/rest_integration.rs`**

```rust
#[tokio::test]
async fn test_full_agent_lifecycle() {
    let (server, api_key) = setup_test_server().await;

    // Exchange key for JWT
    let token_resp: TokenResponse = server
        .post("/api/v1/auth/token")
        .json(&json!({ "api_key": api_key }))
        .send().await.json();
    let jwt = token_resp.access_token;

    // List agents (empty)
    let resp: ApiResponse<Vec<AgentSummary>> = server
        .get("/api/v1/agents")
        .bearer(&jwt)
        .send().await.json();
    assert!(resp.data.is_empty());

    // Connect agent
    let resp: ApiResponse<AgentSummary> = server
        .post("/api/v1/agents")
        .bearer(&jwt)
        .json(&json!({
            "name": "test-agent",
            "provider": "mock",
            "model": "test",
            "roles": ["general"]
        }))
        .send().await.json();
    assert_eq!(resp.data.name, "test-agent");

    // List agents (1)
    let resp: ApiResponse<Vec<AgentSummary>> = server
        .get("/api/v1/agents")
        .bearer(&jwt)
        .send().await.json();
    assert_eq!(resp.data.len(), 1);

    // Disconnect
    server.delete("/api/v1/agents/test-agent")
        .bearer(&jwt)
        .send().await.assert_ok();
}

#[tokio::test]
async fn test_auth_enforcement() {
    let (server, _) = setup_test_server().await;

    // No token → 401
    let status = server.get("/api/v1/agents").send().await.status();
    assert_eq!(status, 401);

    // Invalid token → 401
    let status = server.get("/api/v1/agents")
        .bearer("invalid")
        .send().await.status();
    assert_eq!(status, 401);
}

#[tokio::test]
async fn test_permission_enforcement() {
    let (server, _) = setup_test_server().await;

    // Create read-only key
    let key = create_key(&server, "readonly", &["agents:r"]).await;
    let jwt = exchange(&server, &key).await;

    // Read → 200
    server.get("/api/v1/agents").bearer(&jwt).send().await.assert_ok();

    // Write → 403
    let status = server.post("/api/v1/agents")
        .bearer(&jwt)
        .json(&json!({"name": "x", "provider": "mock", "model": "t", "roles": []}))
        .send().await.status();
    assert_eq!(status, 403);
}

#[tokio::test]
async fn test_task_pagination() {
    let (server, jwt) = setup_authenticated_server().await;
    // Create 5 tasks, then query with limit=2&offset=2
    // Verify meta.total=5, data.len()=2
}

#[tokio::test]
async fn test_openapi_spec() {
    let (server, jwt) = setup_authenticated_server().await;
    let spec: serde_json::Value = server
        .get("/api/v1/openapi.json")
        .bearer(&jwt)
        .send().await.json();
    assert_eq!(spec["info"]["title"], "AgentOS API");
    assert!(spec["paths"].as_object().unwrap().len() > 10);
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-api/Cargo.toml` | Add `utoipa`, `utoipa-axum` deps |
| `crates/agentos-api/src/rest/mod.rs` | Router builder, response envelope, pagination |
| `crates/agentos-api/src/rest/agents.rs` | 6 agent endpoints |
| `crates/agentos-api/src/rest/tasks.rs` | 5 task endpoints |
| `crates/agentos-api/src/rest/tools.rs` | 3 tool endpoints |
| `crates/agentos-api/src/rest/secrets.rs` | 3 secret endpoints |
| `crates/agentos-api/src/rest/system.rs` | 1 status endpoint |
| `crates/agentos-api/src/openapi.rs` | OpenAPI spec generation |
| `crates/agentos-web/src/server.rs` | Mount `/api/v1` router |
| `crates/agentos-api/tests/rest_integration.rs` | Integration tests |

## Dependencies

- **Requires:** Phase 1 (trait), Phase 2 (auth)
- **Blocks:** Phase 4 (remaining endpoints), Phase 5 (WebSocket — shares state), Phase 6 (web migration uses same types)

## Test Plan

1. Auth enforcement: 401 without token, 403 without permission, 200 with valid token+permission
2. Agent CRUD: connect → list → get detail → grant permission → disconnect
3. Task lifecycle: run → list → get → cancel → get trace
4. Tool lifecycle: list → install → list → remove
5. Secret lifecycle: list → set → list → revoke
6. System status: returns uptime, counts
7. Pagination: limit, offset, meta.total correctness
8. OpenAPI spec: valid JSON, all paths present, schemas defined
9. Error responses: consistent envelope with code + message + status

## Verification

```bash
cargo build -p agentos-api
cargo test -p agentos-api
cargo build -p agentos-web  # verify mounting works
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
