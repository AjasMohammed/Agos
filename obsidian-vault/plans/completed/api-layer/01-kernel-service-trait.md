---
title: "Phase 1: KernelService Trait + Impl"
tags:
  - api
  - kernel
  - v3
  - phase-1
date: 2026-03-30
status: planned
effort: 3d
priority: high
---

# Phase 1: KernelService Trait + Impl

> Define the `KernelService` trait as the unified API boundary and implement it for `Kernel`, consolidating all kernel access into a single impl block.

---

## Why This Phase

This is the foundation. Every subsequent phase (REST, WebSocket, web migration, bus migration) depends on this trait existing. Without it, we'd be building API routes that still reach into kernel internals — moving the coupling problem rather than solving it.

## Current State

- `agentos-web` handlers access 35+ `pub` fields on `Kernel` directly
- 15 `api_*()` methods exist on `Kernel` but cover only a fraction of needed operations
- No abstraction boundary between consumers and kernel internals
- No `agentos-api` crate exists

## Target State

- New `agentos-api` crate with `KernelService` trait (~30 methods)
- API DTO types decoupled from internal kernel types
- `ApiError` type with HTTP status mapping
- `impl KernelService for Kernel` consolidating all kernel access
- Unit tests proving every trait method works

## Detailed Subtasks

### 1. Create `agentos-api` crate

Create `crates/agentos-api/Cargo.toml`:

```toml
[package]
name = "agentos-api"
version = "0.1.0"
edition = "2021"

[dependencies]
agentos-types = { path = "../agentos-types" }
agentos-kernel = { path = "../agentos-kernel" }
agentos-audit = { path = "../agentos-audit" }
agentos-vault = { path = "../agentos-vault" }
agentos-pipeline = { path = "../agentos-pipeline" }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tokio-stream = "0.1"
chrono = { workspace = true }
uuid = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
```

Add to workspace `Cargo.toml` members list.

Create directory structure:
```
crates/agentos-api/src/
├── lib.rs
├── service.rs
├── kernel_impl.rs
├── error.rs
└── types/
    ├── mod.rs
    ├── agents.rs
    ├── tasks.rs
    ├── tools.rs
    ├── secrets.rs
    ├── chat.rs
    ├── pipelines.rs
    ├── audit.rs
    ├── costs.rs
    ├── notifications.rs
    └── system.rs
```

### 2. Define API DTO types

Each type module defines request/response structs with `Serialize` + `Deserialize`. These are the API contract — decoupled from internal types.

**`types/agents.rs`:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id: AgentID,
    pub name: String,
    pub provider: String,
    pub model: String,
    pub status: String,
    pub roles: Vec<String>,
    pub connected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDetail {
    pub summary: AgentSummary,
    pub permissions: Vec<String>,
    pub recent_tasks: Vec<TaskSummary>,
    pub cost_snapshot: Option<CostSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectAgentRequest {
    pub name: String,
    pub provider: String, // "ollama", "openai", "anthropic", "gemini"
    pub model: String,
    pub base_url: Option<String>,
    pub roles: Vec<String>,
}
```

**`types/tasks.rs`:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: TaskID,
    pub agent_name: Option<String>,
    pub prompt_preview: String, // first 200 chars
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    pub id: TaskID,
    pub agent_name: Option<String>,
    pub prompt: String,
    pub status: String,
    pub context_messages: Vec<ContextMessage>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskFilter {
    pub status: Option<String>,
    pub agent_name: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTaskRequest {
    pub agent_name: Option<String>,
    pub prompt: String,
    pub autonomous: bool,
}
```

**`types/system.rs`:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSummary {
    pub agent_count: usize,
    pub online_agents: Vec<AgentSummary>,
    pub task_counts: TaskCounts,
    pub tool_count: usize,
    pub uptime_secs: u64,
    pub recent_audit: Vec<AuditEntrySummary>,
    pub background_task_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCounts {
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub uptime_secs: u64,
    pub agent_count: usize,
    pub task_count: usize,
    pub tool_count: usize,
    pub version: String,
}
```

Similar patterns for `tools.rs`, `secrets.rs`, `chat.rs`, `pipelines.rs`, `audit.rs`, `costs.rs`, `notifications.rs`. Each keeps it flat — no unnecessary nesting.

### 3. Define `ApiError`

**`error.rs`:**
```rust
use axum::http::StatusCode;
use thiserror::Error;

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    pub status: u16,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl ApiError {
    pub fn status_code(&self) -> StatusCode { ... }
    pub fn error_code(&self) -> &str { ... } // "NOT_FOUND", "UNAUTHORIZED", etc.
}

impl From<AgentOSError> for ApiError { ... }
```

### 4. Define `KernelService` trait

**`service.rs`:**
```rust
#[async_trait]
pub trait KernelService: Send + Sync {
    // Agents
    async fn list_agents(&self) -> Result<Vec<AgentSummary>, ApiError>;
    async fn connect_agent(&self, req: ConnectAgentRequest) -> Result<AgentSummary, ApiError>;
    async fn disconnect_agent(&self, agent_id: AgentID) -> Result<(), ApiError>;
    async fn get_agent_detail(&self, name: &str) -> Result<AgentDetail, ApiError>;
    async fn grant_permission(&self, req: PermissionRequest) -> Result<(), ApiError>;
    async fn revoke_permission(&self, req: PermissionRequest) -> Result<(), ApiError>;

    // Tasks
    async fn list_tasks(&self, filter: TaskFilter) -> Result<(Vec<TaskSummary>, u64), ApiError>;
    async fn get_task(&self, id: TaskID) -> Result<TaskDetail, ApiError>;
    async fn run_task(&self, req: RunTaskRequest) -> Result<TaskID, ApiError>;
    async fn cancel_task(&self, id: TaskID) -> Result<(), ApiError>;
    async fn get_task_trace(&self, id: TaskID) -> Result<TaskTrace, ApiError>;

    // Tools
    async fn list_tools(&self) -> Result<Vec<ToolSummary>, ApiError>;
    async fn install_tool(&self, req: InstallToolRequest) -> Result<ToolID, ApiError>;
    async fn remove_tool(&self, name: &str) -> Result<(), ApiError>;

    // Secrets
    async fn list_secrets(&self) -> Result<Vec<SecretMetadata>, ApiError>;
    async fn set_secret(&self, req: SetSecretRequest) -> Result<(), ApiError>;
    async fn revoke_secret(&self, name: &str) -> Result<(), ApiError>;

    // Chat
    async fn chat_send(&self, req: ChatRequest) -> Result<ChatResponse, ApiError>;
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ApiError>;

    // Pipelines
    async fn list_pipelines(&self) -> Result<Vec<PipelineSummary>, ApiError>;
    async fn save_pipeline(&self, req: SavePipelineRequest) -> Result<(), ApiError>;
    async fn run_pipeline(&self, req: RunPipelineRequest) -> Result<serde_json::Value, ApiError>;
    async fn delete_pipeline(&self, name: &str) -> Result<(), ApiError>;
    async fn import_pipeline(&self, yaml: &str) -> Result<(), ApiError>;
    async fn export_pipeline(&self, name: &str) -> Result<String, ApiError>;

    // Audit
    async fn query_audit(&self, filter: AuditFilter) -> Result<Vec<AuditEntrySummary>, ApiError>;
    async fn get_audit_detail(&self, trace_id: &str) -> Result<AuditEntryDetail, ApiError>;

    // Costs
    async fn get_cost_summary(&self) -> Result<Vec<CostSnapshot>, ApiError>;
    async fn get_agent_costs(&self, agent_name: &str) -> Result<CostSnapshot, ApiError>;

    // Notifications
    async fn list_notifications(&self, filter: NotificationFilter) -> Result<Vec<NotificationSummary>, ApiError>;
    async fn get_notification(&self, id: NotificationID) -> Result<NotificationDetail, ApiError>;
    async fn respond_to_notification(&self, req: NotificationResponse) -> Result<(), ApiError>;
    async fn get_unread_count(&self) -> Result<u64, ApiError>;

    // Dashboard (composite)
    async fn get_dashboard_summary(&self) -> Result<DashboardSummary, ApiError>;

    // System
    async fn get_status(&self) -> Result<SystemStatus, ApiError>;
    async fn get_uptime(&self) -> std::time::Duration;
}
```

Note: `list_tasks` returns `(Vec<TaskSummary>, u64)` — the second element is total count for pagination.

### 5. Implement `KernelService` for `Kernel`

**`kernel_impl.rs`:**

This is where all kernel internal access is consolidated. Each method wraps existing kernel operations.

```rust
#[async_trait]
impl KernelService for Kernel {
    async fn list_agents(&self) -> Result<Vec<AgentSummary>, ApiError> {
        let registry = self.agent_registry.read().await;
        Ok(registry.list_online().iter().map(|a| AgentSummary {
            id: a.id,
            name: a.name.clone(),
            provider: format!("{:?}", a.provider),
            model: a.model.clone(),
            status: "online".to_string(),
            roles: a.roles.clone(),
            connected_at: a.connected_at,
        }).collect())
    }

    async fn get_dashboard_summary(&self) -> Result<DashboardSummary, ApiError> {
        let agents = self.list_agents().await?;
        let tasks = self.scheduler.list_tasks().await;
        let tools = self.tool_registry.read().await;
        let audit = {
            let audit = self.audit.clone();
            tokio::task::spawn_blocking(move || audit.query_recent(10))
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .map_err(|e| ApiError::Internal(e.to_string()))?
        };
        let bg_count = self.background_pool.list_running().len();
        let uptime = (Utc::now() - self.started_at).num_seconds().max(0) as u64;

        Ok(DashboardSummary {
            agent_count: agents.len(),
            online_agents: agents,
            task_counts: TaskCounts::from_tasks(&tasks),
            tool_count: tools.list_all().len(),
            uptime_secs: uptime,
            recent_audit: audit.into_iter().map(AuditEntrySummary::from).collect(),
            background_task_count: bg_count,
        })
    }

    async fn cancel_task(&self, id: TaskID) -> Result<(), ApiError> {
        self.scheduler.update_state(id, agentos_types::TaskState::Cancelled).await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    // ... etc for all methods
}
```

### 6. Write unit tests

**`crates/agentos-api/tests/service_tests.rs`:**

```rust
#[tokio::test]
async fn test_list_agents_empty() {
    let kernel = setup_test_kernel().await;
    let svc: &dyn KernelService = &kernel;
    let agents = svc.list_agents().await.unwrap();
    assert!(agents.is_empty());
}

#[tokio::test]
async fn test_connect_and_list_agent() {
    let kernel = setup_test_kernel().await;
    let svc: &dyn KernelService = &kernel;
    let req = ConnectAgentRequest {
        name: "test-agent".into(),
        provider: "mock".into(),
        model: "test".into(),
        base_url: None,
        roles: vec!["general".into()],
    };
    let agent = svc.connect_agent(req).await.unwrap();
    assert_eq!(agent.name, "test-agent");

    let agents = svc.list_agents().await.unwrap();
    assert_eq!(agents.len(), 1);
}

#[tokio::test]
async fn test_dashboard_summary() {
    let kernel = setup_test_kernel().await;
    let svc: &dyn KernelService = &kernel;
    let summary = svc.get_dashboard_summary().await.unwrap();
    assert_eq!(summary.agent_count, 0);
    assert!(summary.uptime_secs < 5);
}

#[tokio::test]
async fn test_cancel_nonexistent_task() {
    let kernel = setup_test_kernel().await;
    let svc: &dyn KernelService = &kernel;
    let result = svc.cancel_task(TaskID::new()).await;
    assert!(result.is_err());
}
```

## Files Changed

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `agentos-api` to members |
| `crates/agentos-api/Cargo.toml` | New crate manifest |
| `crates/agentos-api/src/lib.rs` | Re-exports |
| `crates/agentos-api/src/service.rs` | `KernelService` trait definition |
| `crates/agentos-api/src/kernel_impl.rs` | `impl KernelService for Kernel` |
| `crates/agentos-api/src/error.rs` | `ApiError` type |
| `crates/agentos-api/src/types/*.rs` | ~10 DTO modules |
| `crates/agentos-api/tests/service_tests.rs` | Unit tests |

## Dependencies

- **Requires:** Nothing (this is the foundation)
- **Blocks:** Phase 2, 3, 4, 5, 6, 7

## Test Plan

1. `cargo build -p agentos-api` — compiles cleanly
2. `cargo test -p agentos-api` — all service method tests pass
3. `cargo test --workspace` — no regressions in other crates
4. `cargo clippy -p agentos-api -- -D warnings` — no warnings

## Verification

```bash
cargo build -p agentos-api
cargo test -p agentos-api
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
