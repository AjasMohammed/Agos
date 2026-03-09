# Plan 05 — Web UI (`agentos-web`)

## Goal

Build a minimal, fast management dashboard for AgentOS using **Axum** (backend) and **HTMX + Alpine.js** (frontend). No JavaScript framework — just server-rendered HTML with inline reactivity. The UI makes everything visible that the CLI manages.

---

## Why Axum + HTMX

| Requirement   | Choice                   | Reason                                                            |
| ------------- | ------------------------ | ----------------------------------------------------------------- |
| Backend       | Axum 0.8                 | Same async runtime (Tokio) as the kernel; minimal, composable     |
| Templates     | MiniJinja                | Rust-native Jinja2 port; safe, fast, no JavaScript at build time  |
| Reactivity    | HTMX 2.x                 | "Islands of interactivity" with zero JS framework overhead        |
| Interactivity | Alpine.js                | 15KB — handles dropdowns, toggles, modals without React           |
| Styling       | Pico CSS + custom        | Minimal semantic CSS; no Tailwind build step needed               |
| Real-time     | SSE (Server-Sent Events) | Task logs and agent status streamed to browser without WebSockets |

---

## Dependencies

```toml
# New workspace dependencies
axum             = { version = "0.8", features = ["ws", "macros"] }
tower            = "0.5"
tower-http       = { version = "0.6", features = ["cors", "trace", "fs", "compression-gzip"] }
minijinja        = { version = "2", features = ["loader"] }
minijinja-embed  = "2"
tokio-stream     = "0.1"
```

---

## New Crate: `agentos-web`

```
crates/agentos-web/
├── Cargo.toml
└── src/
    ├── lib.rs              # WebServer — binds Axum, mounts routes
    ├── server.rs           # WebServer::start(), graceful shutdown
    ├── router.rs           # Route table
    ├── state.rs            # AppState — Arc<Kernel> + Arc<TemplateEngine>
    ├── handlers/
    │   ├── mod.rs
    │   ├── dashboard.rs    # GET /
    │   ├── agents.rs       # GET /agents, POST /agents/connect, DELETE /agents/:name
    │   ├── tasks.rs        # GET /tasks, GET /tasks/:id/logs (SSE)
    │   ├── tools.rs        # GET /tools, POST /tools/install, DELETE /tools/:name
    │   ├── secrets.rs      # GET /secrets, POST /secrets, DELETE /secrets/:name
    │   ├── pipelines.rs    # GET /pipelines, POST /pipelines/run
    │   ├── audit.rs        # GET /audit
    │   └── settings.rs     # GET /settings, POST /settings
    └── templates/          # MiniJinja templates (embedded in binary)
        ├── base.html
        ├── dashboard.html
        ├── agents.html
        ├── tasks.html
        ├── task_detail.html
        ├── tools.html
        ├── secrets.html
        ├── pipelines.html
        ├── audit.html
        └── partials/
            ├── agent_card.html
            ├── task_row.html
            ├── tool_card.html
            └── log_line.html
```

---

## Page Specification

### Dashboard (`GET /`)

- System health banner: kernel status, uptime, connected LLMs, task queue depth
- Active tasks — live count from kernel
- Agent grid: each agent card shows name, model, status (Online/Busy/Idle/Offline), current task
- Tool registry summary: N core, N user, N WASM tools installed
- Recent audit events: last 10 entries

### Agent Manager (`GET /agents`)

- List all connected agents with full profiles
- **Connect new agent** button → modal form (provider, model, name, API key prompt)
- Disconnect / remove agent
- View agent permissions (read-only table)
- Grant/revoke permissions inline

### Task Inspector (`GET /tasks`)

- Table of all tasks: ID, agent, status, created_at, duration
- Color-coded status badges (Queued=grey, Running=blue, Complete=green, Failed=red)
- Click row → task detail page

### Task Detail (`GET /tasks/:id`)

- Context window viewer — scrollable, collapsible intent messages
- Tool call timeline — which tools were called, in order, with latency
- **Live log stream** via SSE: `GET /tasks/:id/logs/stream` → `text/event-stream`

### Tool Manager (`GET /tools`)

- Grid of installed tools (core + user)
- Filter by type: Inline / WASM
- **Install tool** button → file upload or path input for TOML manifest
- Remove tool (with confirmation)
- View tool manifest

### Secrets Manager (`GET /secrets`)

- Table: name, scope, last_used, created_at (no values ever shown)
- **Add secret** → modal with hidden text input (sent via POST, never appears in URL)
- Revoke secret
- Rotate secret (new value prompt)

### Pipeline Manager (`GET /pipelines`)

- Installed pipeline list with step counts
- **Run pipeline** → input prompt → shows run status with step progress
- Run history table

### Audit Log (`GET /audit`)

- Infinite-scroll or paginated table
- Columns: timestamp, event_type, agent, tool/target, success/fail
- Filter by: date range, agent, event type

---

## API Routes for HTMX

HTMX swaps partial HTML fragments, not full pages. Each route has two variants:

| Route                        | Response for                   | Notes             |
| ---------------------------- | ------------------------------ | ----------------- |
| `GET /agents`                | Full page                      | First load        |
| `GET /agents?partial=list`   | `<tbody>` fragment             | HTMX poll/refresh |
| `POST /agents/connect`       | Redirect or agent card partial | After form submit |
| `DELETE /agents/:name`       | Empty 204                      | Swaps row out     |
| `GET /tasks/:id/logs/stream` | `text/event-stream`            | SSE real-time     |

### SSE for Live Task Logs

```rust
// handlers/tasks.rs
async fn task_log_stream(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Sse<impl Stream<Item = Event>> {
    let rx = state.kernel.subscribe_task_logs(&task_id).await;
    let stream = ReceiverStream::new(rx)
        .map(|log_line| Event::default().data(log_line));
    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

---

## AppState

```rust
#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Kernel>,
    pub templates: Arc<Environment<'static>>,
}
```

The `Kernel` is shared — the web server is just another client of the kernel, same as the CLI.

---

## WebServer

```rust
pub struct WebServer {
    bind_addr: SocketAddr,
    state: AppState,
}

impl WebServer {
    pub fn new(bind_addr: SocketAddr, kernel: Arc<Kernel>) -> Self;

    pub async fn start(self) -> Result<(), anyhow::Error> {
        let app = Router::new()
            .route("/", get(dashboard::index))
            .route("/agents", get(agents::list).post(agents::connect))
            .route("/agents/:name", delete(agents::disconnect))
            .route("/tasks", get(tasks::list))
            .route("/tasks/:id", get(tasks::detail))
            .route("/tasks/:id/logs/stream", get(tasks::log_stream))
            .route("/tools", get(tools::list).post(tools::install))
            .route("/tools/:name", delete(tools::remove))
            .route("/secrets", get(secrets::list).post(secrets::create))
            .route("/secrets/:name", delete(secrets::revoke))
            .route("/pipelines", get(pipelines::list))
            .route("/pipelines/run", post(pipelines::run))
            .route("/audit", get(audit::list))
            .nest_service("/static", ServeDir::new("crates/agentos-web/static"))
            .with_state(self.state)
            .layer(
                ServiceBuilder::new()
                    .layer(TraceLayer::new_for_http())
                    .layer(CompressionLayer::new())
                    .layer(CorsLayer::new().allow_origin([
                        "http://127.0.0.1:8080".parse().unwrap(),
                        "http://localhost:8080".parse().unwrap(),
                    ]))
            );

        let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }
}
```

---

## Kernel Integration

The web server starts inside the kernel boot sequence:

```rust
// In kernel.rs boot():
if config.web.enabled {
    let web_kernel = Arc::clone(&kernel_arc);
    tokio::spawn(async move {
        let server = WebServer::new(config.web.bind_addr, web_kernel);
        if let Err(e) = server.start().await {
            tracing::error!(error = %e, "Web UI server failed");
        }
    });
}
```

### Config (`config/default.toml`)

```toml
[web]
enabled  = true
bind     = "127.0.0.1:8080"
# Binds to localhost by default — do NOT expose 0.0.0.0 without auth
```

---

## Security Considerations

| Risk                   | Mitigation                                                               |
| ---------------------- | ------------------------------------------------------------------------ |
| Unauthenticated access | In v3: localhost-only by default. Auth (session cookie or API key) in v4 |
| Secret exposure via UI | Secrets page never renders values — only names and metadata              |
| XSS via LLM output     | MiniJinja auto-escapes HTML; all dynamic content is escaped              |
| CSRF                   | Verify `Origin`/`Referer` headers on POST; per-session CSRF token in forms; `HX-Request` as extra heuristic |

> [!IMPORTANT]
> Authentication (login page, session cookies) is NOT in this plan. The Web UI listens on `127.0.0.1:8080` by default, making it accessible only from the host machine. Adding auth is a Phase 4 item. Do NOT expose `0.0.0.0:8080` without auth on any internet-facing deployment.

---

## Tests

```rust
#[tokio::test]
async fn test_dashboard_returns_200() {
    let app = build_test_app().await;
    let response = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_agents_page_lists_connected_agents() {
    let app = build_test_app_with_agent("test-agent").await;
    let body = get_body(app, "/agents").await;
    assert!(body.contains("test-agent"));
}

#[tokio::test]
async fn test_secret_value_never_in_response() {
    let app = build_test_app_with_secret("MY_KEY", "super-secret").await;
    let body = get_body(app, "/secrets").await;
    assert!(!body.contains("super-secret"));
    assert!(body.contains("MY_KEY")); // Name appears, value does not
}
```

---

## Verification

```bash
# Start kernel (web enabled by default)
cargo run --bin agentos-cli -- start

# Open browser
open http://localhost:8080

# Expected: Dashboard shows kernel status, 0 agents, N tools
```
