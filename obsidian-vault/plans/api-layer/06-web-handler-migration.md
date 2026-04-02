---
title: "Phase 6: Web Handler Migration"
tags:
  - api
  - web
  - v3
  - phase-6
date: 2026-03-30
status: planned
effort: 3d
priority: high
---

# Phase 6: Web Handler Migration

> Migrate all `agentos-web` HTML handlers from direct `Arc<Kernel>` access to `Arc<dyn KernelService>`, then remove `agentos-kernel` as a direct dependency of the web crate.

---

## Why This Phase

This is the decoupling payoff. After this phase, `agentos-web` has zero knowledge of kernel internals — it only knows the `KernelService` trait and its DTOs. Changes to kernel registries, schedulers, or stores can never break the web layer again.

## Current State

- 13 handler files in `crates/agentos-web/src/handlers/` accessing `Arc<Kernel>` directly
- `AppState` holds `kernel: Arc<Kernel>`
- Dashboard, events, and agent_detail handlers reach into 6+ kernel subsystems per request
- `agentos-web/Cargo.toml` depends on `agentos-kernel`, `agentos-audit`, `agentos-vault`, `agentos-pipeline`

## Target State

- `WebState` holds `service: Arc<dyn KernelService>` (no `Arc<Kernel>`)
- All handlers call `KernelService` methods — no direct kernel field access
- `agentos-web/Cargo.toml` depends only on `agentos-api` (and `agentos-types` for ID types)
- All existing XSS tests pass
- All SSE endpoints unchanged (they render HTML partials from service data)

## Detailed Subtasks

### 1. Replace AppState with WebState

**Edit: `crates/agentos-web/src/state.rs`**

Before:
```rust
pub struct AppState {
    pub kernel: Arc<Kernel>,
    pub templates: Arc<Environment<'static>>,
    pub csrf_tokens: Arc<DashMap<String, (String, Instant)>>,
    pub allowed_tool_dirs: Arc<Vec<PathBuf>>,
    pub chat_store: Arc<ChatStore>,
    pub notification_tx: broadcast::Sender<NotificationSsePayload>,
}
```

After:
```rust
pub struct WebState {
    pub service: Arc<dyn KernelService>,
    pub templates: Arc<Environment<'static>>,
    pub csrf_tokens: Arc<DashMap<String, (String, Instant)>>,
    pub chat_store: Arc<ChatStore>,
    pub notification_tx: broadcast::Sender<NotificationSsePayload>,
}
```

Changes:
- `kernel: Arc<Kernel>` → `service: Arc<dyn KernelService>`
- `allowed_tool_dirs` removed (tool path validation now inside `KernelService::install_tool()`)
- Name change `AppState` → `WebState` (update all handler files)

### 2. Migrate handlers one domain at a time

**Migration order** (simplest → most coupled):

#### 2a. Secrets handler (simplest — already clean)

**`handlers/secrets.rs`** — Currently uses `kernel.vault.list()`, `kernel.api_set_secret()`, `kernel.api_revoke_secret()`

After:
```rust
async fn list_secrets(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let secrets = state.service.list_secrets().await.map_err(render_error)?;
    render_template(&state.templates, "secrets.html", context! { secrets })
}

async fn create_secret(State(state): State<Arc<WebState>>, Form(form): Form<SecretForm>) -> impl IntoResponse {
    state.service.set_secret(SetSecretRequest {
        name: form.name,
        value: std::mem::take(&mut form.value),
        scope: form.scope,
    }).await.map_err(render_error)?;
    redirect("/secrets")
}
```

#### 2b. Tools handler

**`handlers/tools.rs`** — Currently uses `kernel.tool_registry.read()`, `kernel.api_install_tool()`, `kernel.api_remove_tool()`, `kernel.audit.clone()`

After:
```rust
async fn list_tools(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let tools = state.service.list_tools().await.map_err(render_error)?;
    render_template(&state.templates, "tools.html", context! { tools })
}
```

Audit logging for blocked installs moves into `KernelService::install_tool()` (it should log there anyway). Remove `allowed_tool_dirs` check from handler — it's now in kernel_impl.

#### 2c. Agents handler

**`handlers/agents.rs`** — Currently uses `kernel.agent_registry.read()`, `kernel.api_connect_agent()`, `kernel.api_disconnect_agent()`

After: direct mapping to `service.list_agents()`, `service.connect_agent()`, `service.disconnect_agent()`.

#### 2d. Agent detail handler

**`handlers/agent_detail.rs`** — Currently aggregates `agent_registry` + `scheduler` + `cost_tracker`

After:
```rust
async fn agent_detail(State(state): State<Arc<WebState>>, Path(name): Path<String>) -> impl IntoResponse {
    let detail = state.service.get_agent_detail(&name).await.map_err(render_error)?;
    render_template(&state.templates, "agents/detail.html", context! { agent: detail })
}
```

The `AgentDetail` struct already includes `recent_tasks` and `cost_snapshot` — the aggregation moved into `kernel_impl.rs`.

#### 2e. Tasks handler

**`handlers/tasks.rs`** — Currently uses `scheduler.list_tasks()`, `scheduler.get_task()`, `scheduler.update_state()`, `trace_collector.get_trace()`, `audit.clone()`

After:
```rust
async fn list_tasks(State(state): State<Arc<WebState>>, Query(params): Query<TaskParams>) -> impl IntoResponse {
    let filter = TaskFilter { status: params.status, agent_name: params.agent, limit: Some(100), offset: None };
    let (tasks, total) = state.service.list_tasks(filter).await.map_err(render_error)?;
    render_template(&state.templates, "tasks.html", context! { tasks, total })
}

async fn cancel_task(State(state): State<Arc<WebState>>, Path(id): Path<TaskID>) -> impl IntoResponse {
    state.service.cancel_task(id).await.map_err(render_error)?;
    redirect(&format!("/tasks/{}", id))
}
```

#### 2f. Chat handler

**`handlers/chat.rs`** — Currently uses `kernel.agent_registry.read()` for validation, `kernel.chat_infer_with_tools()`, `kernel.chat_infer_streaming()`

After: `service.chat_send()` and `service.chat_stream()`. Agent validation moves into the service method.

#### 2g. Notifications handler

Already clean — direct mapping to service methods.

#### 2h. Pipelines handler + pipeline_ui handler

**`handlers/pipelines.rs`** and **`handlers/pipeline_ui.rs`** — Currently use `pipeline_engine.store_arc()` extensively

After: all pipeline operations go through `service.list_pipelines()`, `service.save_pipeline()`, `service.run_pipeline()`, `service.delete_pipeline()`, `service.import_pipeline()`, `service.export_pipeline()`.

The pipeline builder UI (`pipeline_ui.rs`) needs `list_agents()` and `list_tools()` for the builder's dropdowns — these are already on the trait.

#### 2i. Audit handler

**`handlers/audit.rs`** — Currently uses `kernel.audit.clone()` for direct queries

After: `service.query_audit()`, `service.get_audit_detail()`.

#### 2j. Costs handler

**`handlers/costs.rs`** — Currently uses `kernel.cost_tracker.get_all_snapshots()`, `kernel.scheduler.list_tasks()`

After: `service.get_cost_summary()`.

#### 2k. Dashboard handler (most complex)

**`handlers/dashboard.rs`** — Currently aggregates 6 subsystems

After:
```rust
async fn dashboard(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let summary = state.service.get_dashboard_summary().await.map_err(render_error)?;
    render_template(&state.templates, "dashboard.html", context! { summary })
}
```

One call replaces six. Template will need minor updates to read from `summary.agent_count` instead of `agents.len()`, etc.

#### 2l. Events handler (SSE streams)

**`handlers/events.rs`** — Currently polls 6 subsystems every 2-3 seconds

After:
```rust
async fn dashboard_events(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let stream = async_stream::stream! {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            if let Ok(summary) = state.service.get_dashboard_summary().await {
                let html = render_partial(&state.templates, "partials/dashboard_stats.html", &summary);
                yield Ok::<_, Infallible>(Event::default().event("stats").data(html));
            }
        }
    };
    Sse::new(stream)
}
```

### 3. Update templates for DTO field names

Some templates reference internal types (e.g., `agent.id.0` for UUID unwrapping). After migration, DTOs have flattened fields. Audit each template for field name changes:

- `dashboard.html` — use `summary.agent_count`, `summary.task_counts.running`, etc.
- `agents/detail.html` — use `agent.summary.name`, `agent.permissions`, `agent.cost_snapshot`
- `task_detail.html` — use `task.prompt`, `task.status`, `task.context_messages`

### 4. Update Cargo.toml dependencies

**Edit: `crates/agentos-web/Cargo.toml`**

Remove:
```toml
agentos-kernel = { path = "../agentos-kernel" }
agentos-audit = { path = "../agentos-audit" }
agentos-vault = { path = "../agentos-vault" }
agentos-pipeline = { path = "../agentos-pipeline" }
```

Add/keep:
```toml
agentos-api = { path = "../agentos-api" }
agentos-types = { path = "../agentos-types" }  # for ID types used in URL paths
```

### 5. Update server.rs to pass service instead of kernel

**Edit: `crates/agentos-web/src/server.rs`**

```rust
pub async fn new(
    bind_addr: SocketAddr,
    service: Arc<dyn KernelService>,
    chat_store: Arc<ChatStore>,
    notification_tx: broadcast::Sender<NotificationSsePayload>,
) -> Result<Self> {
    let web_state = Arc::new(WebState {
        service,
        templates: build_templates()?,
        csrf_tokens: Arc::new(DashMap::new()),
        chat_store,
        notification_tx,
    });
    // ... build router with web_state
}
```

The caller (in `agentos-cli` or wherever the web server is started) constructs the `Arc<dyn KernelService>` from the kernel.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/Cargo.toml` | Remove kernel/audit/vault/pipeline deps, add agentos-api |
| `crates/agentos-web/src/state.rs` | `AppState` → `WebState` with `Arc<dyn KernelService>` |
| `crates/agentos-web/src/server.rs` | Accept `Arc<dyn KernelService>` instead of `Arc<Kernel>` |
| `crates/agentos-web/src/handlers/secrets.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/tools.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/agents.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/agent_detail.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/tasks.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/chat.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/notifications.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/pipelines.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/pipeline_ui.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/audit.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/costs.rs` | Migrate to service calls |
| `crates/agentos-web/src/handlers/dashboard.rs` | Migrate to `get_dashboard_summary()` |
| `crates/agentos-web/src/handlers/events.rs` | Migrate SSE to service calls |
| `crates/agentos-web/src/templates/*.html` | Update field names for API DTOs |
| `crates/agentos-web/src/auth.rs` | Keep for session cookie auth (unchanged) |
| `crates/agentos-cli/src/commands/web.rs` | Construct `Arc<dyn KernelService>` from kernel |

## Dependencies

- **Requires:** Phase 4 (all REST endpoints → all service methods exist), Phase 5 (WebSocket → broadcaster wired)
- **Blocks:** Nothing (this is the payoff phase)

## Test Plan

1. **XSS tests still pass** — `tests/xss_tests.rs` unchanged in behavior
2. **Smoke test every page** — dashboard, agents, tasks, tools, secrets, pipelines, chat, audit, costs, notifications
3. **CRUD operations** — connect agent via UI, run task, install tool, create secret — all still work
4. **SSE streams** — dashboard auto-updates, task log streaming, notification bell counter
5. **Build verification** — `cargo build -p agentos-web` succeeds without `agentos-kernel` in deps
6. **No regressions** — `cargo test --workspace` all green

## Verification

```bash
# Verify decoupling: agentos-web should NOT depend on agentos-kernel
cargo tree -p agentos-web | grep -c agentos-kernel  # should be 0

cargo build -p agentos-web
cargo test -p agentos-web
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
