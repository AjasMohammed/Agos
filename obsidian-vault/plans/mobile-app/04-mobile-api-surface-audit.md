---
title: Phase 4 — Mobile API Surface Audit
tags:
  - mobile
  - api
  - openapi
  - phase-4
date: 2026-04-19
status: planned
effort: 2d
priority: high
---

# Phase 4 — Mobile API Surface Audit

> Inventory every existing `agentos-api` endpoint, close gaps needed for mobile (JSON pipeline submission, SSE task progress, uniform pagination, escalation resolve endpoint), and emit a versioned `openapi.json` that generates the mobile TypeScript client.

---

## Why this phase

Mobile is more sensitive to API design than web. Unstable payloads, ad-hoc pagination, and HTML-first endpoints (which agentos-web has) break the generated client on every change. This phase locks the contract: document what exists, fix the mobile-hostile spots, emit OpenAPI, and generate a typed client used by every mobile screen in Phases 6-9.

## Current → Target state

**Current:**
- 50+ handlers across `handlers/agents.rs`, `chat.rs`, `tasks.rs`, `pipelines.rs`, `audit.rs`, etc.
- No OpenAPI schema; contracts live in prose and handler source.
- Some endpoints return HTML fragments (web UI leftovers).
- Pagination varies: some use `?page=N&per_page=M`, some `?limit=N&cursor=X`.
- Pipeline creation goes through an HTMX form, not JSON.
- Escalation resolution currently only callable via internal web UI.

**Target:**
- Audit spreadsheet: `obsidian-vault/plans/mobile-app/API Surface Audit.md` (a tracking table).
- Annotate every handler with `utoipa` or `aide` derive macros → emit `openapi.json` at `GET /openapi.json`.
- Unify pagination to `?cursor=...&limit=...` returning `{ items, next_cursor }`.
- Every endpoint returns JSON (web UI HTML is under `/ui/*`, already split).
- New/extended endpoints:
  - `POST /v1/pipelines` accepts JSON body (in addition to existing form)
  - `POST /v1/pipelines/:id/run` returns `{ run_id }`
  - `GET /v1/pipelines/:id/runs/:run_id` — run status JSON
  - `GET /v1/pipelines/:id/runs/:run_id/stream` — SSE progress
  - `GET /v1/tasks/:id/stream` — SSE progress (delta events)
  - `POST /v1/escalations/:id/resolve` — `{ decision: "approve"|"deny", reason?: string }`
  - `GET /v1/escalations?status=pending&cursor=...` — list
  - `GET /v1/chat/conversations` / `POST /v1/chat/conversations` — persistent chat threads
- Mobile client generated into `mobile/src/api/generated.ts` via `openapi-typescript`.

## Detailed subtasks

### 4.1 Enumerate the current surface

Run a one-shot script:

```bash
grep -rn "Router::new\|\.route(" crates/agentos-api/src/ | grep -v test | sort -u > /tmp/api-surface.txt
```

Produce `API Surface Audit.md` with columns:

| Method | Path | Handler | Body | Response | Pagination | OpenAPI | Mobile-ready? | Gap |
|--------|------|---------|------|----------|------------|---------|---------------|-----|

Fill every row. The "Gap" column drives the rest of this phase's work.

### 4.2 Pick OpenAPI emitter

Choose `utoipa` (most mature in Rust ecosystem, integrates with Axum via `utoipa-axum`).

Add:

```toml
[dependencies]
utoipa = { version = "4", features = ["axum_extras", "uuid", "chrono"] }
utoipa-axum = "0.1"
utoipa-swagger-ui = { version = "6", features = ["axum"] }
```

In `service.rs`:

```rust
#[derive(OpenApi)]
#[openapi(
    info(title = "AgentOS API", version = env!("CARGO_PKG_VERSION")),
    paths(
        handlers::chat::completions,
        handlers::tasks::create, handlers::tasks::get, handlers::tasks::list, handlers::tasks::resume,
        handlers::pipelines::create, handlers::pipelines::run, handlers::pipelines::get_run,
        handlers::auth::authorize, handlers::auth::token, handlers::auth::refresh,
        handlers::devices::register, handlers::devices::list,
        handlers::escalations::resolve, handlers::escalations::list,
        ...
    ),
    components(schemas(
        AgentTask, TaskStatus, PipelineDefinition, PipelineStep, Escalation,
        ChatCompletionRequest, ChatCompletionChunk,
        Device, RegisterDeviceRequest, NotificationPreferences,
        TokenRequest, TokenResponse,
        PageCursor,
    )),
    security(("bearer_auth" = []), ("api_key" = [])),
)]
pub struct ApiDoc;
```

Every handler annotated with `#[utoipa::path(...)]`. Mount:

```rust
router.route("/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))
      .merge(SwaggerUi::new("/docs").url("/openapi.json", ApiDoc::openapi()));
```

### 4.3 Unify pagination

Introduce `crates/agentos-api/src/pagination.rs`:

```rust
#[derive(Serialize, Deserialize, ToSchema)]
pub struct Page<T: ToSchema> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct PageParams {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}
fn default_limit() -> u32 { 50 }
```

Cursors are opaque base64 of `{id, ts}` tuple — clients treat them as strings.

Migrate: `GET /v1/tasks`, `/v1/audit`, `/v1/escalations`, `/v1/chat/conversations`, `/v1/pipelines`, `/v1/pipelines/:id/runs`. Keep 1 release of back-compat by accepting both old `page`/`per_page` and new `cursor`/`limit`.

### 4.4 JSON pipeline creation

File: `crates/agentos-api/src/handlers/pipelines.rs`.

```rust
#[derive(Deserialize, ToSchema)]
pub struct CreatePipelineJson {
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<PipelineStep>,
}

#[utoipa::path(post, path = "/v1/pipelines", request_body = CreatePipelineJson, responses((status = 201, body = Pipeline)))]
pub async fn create_json(
    Extension(principal): Extension<AuthPrincipal>,
    State(s): State<AppState>,
    Json(req): Json<CreatePipelineJson>,
) -> Result<(StatusCode, Json<Pipeline>), ApiError> {
    let p = s.kernel.create_pipeline(principal.user_id()?, req.into()).await?;
    Ok((StatusCode::CREATED, Json(p)))
}
```

Existing HTMX-form handler stays at the same route path — negotiate via `Content-Type` (form vs JSON). Axum supports this via two separate handlers behind an extractor helper.

### 4.5 Task + pipeline-run progress SSE

Extend existing task subscription infra (grep `task_completion.rs` / `subscription`). New route:

```rust
#[utoipa::path(get, path = "/v1/tasks/{id}/stream")]
pub async fn stream_task(Path(id): Path<Uuid>, State(s): State<AppState>) -> impl IntoResponse {
    let rx = s.kernel.subscribe_task_events(id).await?;
    Sse::new(BroadcastStream::new(rx).map(|e| Ok(Event::default().json_data(e?).unwrap())))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
```

Events: `TaskEvent::{Started, StepCompleted, ToolInvoked, CheckpointWritten, Finished}`. Schemas in types crate.

### 4.6 Escalation endpoints

File: `crates/agentos-api/src/handlers/escalations.rs` (new).

```rust
pub async fn list(
    Extension(p): Extension<AuthPrincipal>, State(s): State<AppState>, Query(q): Query<EscalationQuery>,
) -> Result<Json<Page<Escalation>>, ApiError> { ... }

pub async fn resolve(
    Extension(p): Extension<AuthPrincipal>, State(s): State<AppState>,
    Path(id): Path<Uuid>, Json(body): Json<ResolveEscalation>,
) -> Result<Json<Escalation>, ApiError> {
    // Send kernel command ResolveEscalation { id, decision, resolved_by: p.user_id()? }.
    // Kernel records audit, resumes or aborts the task.
}
```

**Authz:** only users with scope `escalations:resolve` (issued by default) can resolve.

### 4.7 Generate mobile client

Add CI job `generate-mobile-api`:

```yaml
- run: cargo run -p agentos-cli -- openapi > mobile/openapi.json
- run: npx openapi-typescript mobile/openapi.json -o mobile/src/api/generated.ts
- run: git diff --exit-code mobile/src/api/generated.ts  # fail if client out of sync
```

Add `agentos openapi` CLI subcommand that prints the schema (offline — doesn't need the kernel running).

### 4.8 Version policy

- Path is `/v1/*` for the entire mobile contract.
- Any breaking change → `/v2/*` (both run in parallel for ≥ 1 minor release).
- Additive changes (new fields, new endpoints) are non-breaking by convention — document in `CHANGELOG.md`.

## Files changed

| File | Change |
|------|--------|
| `obsidian-vault/plans/mobile-app/API Surface Audit.md` | new — living inventory |
| `crates/agentos-api/Cargo.toml` | add utoipa, utoipa-axum, utoipa-swagger-ui |
| `crates/agentos-api/src/pagination.rs` | new |
| `crates/agentos-api/src/service.rs` | mount OpenAPI + swagger + new routes |
| `crates/agentos-api/src/handlers/pipelines.rs` | JSON handler, utoipa annotations |
| `crates/agentos-api/src/handlers/tasks.rs` | `/v1/tasks/:id/stream`, utoipa annotations, pagination migration |
| `crates/agentos-api/src/handlers/escalations.rs` | new |
| `crates/agentos-api/src/handlers/audit.rs`, `chat.rs`, `agents.rs` | utoipa annotations, pagination migration |
| `crates/agentos-cli/src/commands/openapi.rs` | new — emits schema to stdout |
| `CHANGELOG.md` | document additions |
| `mobile/openapi.json` | generated artifact (checked in for visibility) |
| `mobile/src/api/generated.ts` | generated typed client (checked in) |

## Dependencies

- [[02-mobile-oauth2-auth-layer]] — auth routes must be annotated too.

## Test plan

- Unit: Page<T> serializes with `next_cursor` omitted when None.
- Integration: `GET /openapi.json` parses as valid OpenAPI 3.1 (use `oasdiff` in CI).
- Integration: Pagination roundtrip — `limit=10`, follow `next_cursor` until empty, assert counts match.
- Integration: Legacy `?page=1&per_page=20` still works for one release.
- Integration: JSON pipeline creation returns 201 with valid `Pipeline`.
- Integration: `/v1/tasks/:id/stream` delivers at least `Started` and `Finished` events for a sample task.
- Integration: Mobile codegen — snapshot `mobile/src/api/generated.ts`; assert compiles (`tsc --noEmit`).

## Verification

```bash
cargo test -p agentos-api
cargo run -p agentos-cli -- openapi | jq . > /tmp/openapi.json
npx @redocly/cli lint /tmp/openapi.json       # fails on obvious spec mistakes
npx openapi-typescript /tmp/openapi.json -o /tmp/client.ts
cd mobile && npx tsc --noEmit                 # if mobile scaffold exists (Phase 5)
```

## Related

- [[Mobile App Plan]]
- [[05-mobile-app-scaffold-and-auth]] — consumes `mobile/src/api/generated.ts`
- [[06-agent-chat-screen-sse]]
- [[07-task-management-screens]]
- [[08-workflow-pipeline-builder]]
