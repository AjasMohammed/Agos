//! HTTP router — maps all `/api/v1/*` routes to handler functions with middleware.
//!
//! The middleware stack (outermost → innermost on requests):
//! 1. Rate limiting (tower-governor)
//! 2. CORS
//! 3. Tracing
//! 4. Compression
//! 5. Security headers
//! 6. Bearer auth (on protected routes only)

use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Method, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::GovernorLayer;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::api_key::ApiKeyStore;
use crate::handlers::{
    agent_chats, agents, audit, auth, channels, chat, chat_sessions, config, connectors, costs,
    dashboard, doctor, escalations, events, files, identity, keys, logs, marketplace, mcp,
    notifications, pipelines, plugins, prefs, roles, schedules, scratchpad, secrets, sse, system,
    system_info, tasks, tools, webhooks, webhooks_admin, workflows,
};
use crate::service::KernelService;
use crate::ws;
use crate::ws::broadcaster::WsBroadcaster;
use utoipa::OpenApi;

/// Middleware that sets security headers on every response.
async fn add_security_headers(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        axum::http::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store"),
    );
    response
}

/// Build the complete Axum router for the REST API.
///
/// # Arguments
/// * `service` — The `KernelService` implementation (real kernel or mock).
/// * `key_store` — API key store for authentication.
/// * `bind_addr` — The socket address the server will bind to (used for CORS origin).
/// * `docs_enabled` — When false, the interactive Scalar docs UI at `GET /api/v1/docs`
///   is not registered. The `GET /api/v1/openapi.json` contract endpoint stays public
///   regardless. Disable on internet-exposed deployments.
/// * `cors_allowed_origins` — Explicit CORS origin allowlist (`[api] cors_allowed_origins`).
///   When empty, CORS falls back to the API's own bind origin (same-origin only).
/// * `refresh_enabled` — When false, `POST /api/v1/auth/refresh` is not registered.
pub fn build_router(
    service: Arc<dyn KernelService>,
    key_store: ApiKeyStore,
    broadcaster: WsBroadcaster,
    bind_addr: SocketAddr,
    docs_enabled: bool,
    cors_allowed_origins: Vec<String>,
    refresh_enabled: bool,
) -> Result<Router, String> {
    // ── Public routes (no auth via header — WS uses query param) ────────
    let public_routes = Router::new()
        .route("/api/v1/health", get(system::health))
        .route("/api/v1/ws", get(ws::ws_upgrade))
        // OpenAPI contract. Public so the frontend can fetch the contract; the schema
        // exposes only the API shape, no secrets.
        .route("/api/v1/openapi.json", get(openapi_json))
        // Telegram webhook — public, authenticated via secret_token header.
        .route(
            "/api/v1/webhooks/telegram/{channel_id}",
            post(webhooks::telegram_webhook),
        )
        // WhatsApp webhook — public; GET verify handshake + POST HMAC-verified.
        .route(
            "/api/v1/webhooks/whatsapp/{channel_id}",
            get(webhooks::whatsapp_webhook_verify).post(webhooks::whatsapp_webhook),
        );

    // Interactive Scalar docs UI. Gated by the `docs_enabled` arg (`[api] docs_enabled`)
    // so it can be turned off on internet-exposed deployments.
    let public_routes = if docs_enabled {
        public_routes.route("/api/v1/docs", get(scalar_ui))
    } else {
        public_routes
    };

    // ── Protected routes (require Bearer token) ─────────────────────────
    let protected_routes = Router::new()
        // System
        .route("/api/v1/status", get(system::status))
        // Observability & system
        .route("/api/v1/dashboard", get(dashboard::get))
        .route("/api/v1/config", get(config::get_tree))
        .route(
            "/api/v1/config/{key}",
            get(config::get_key).put(config::set_key),
        )
        .route("/api/v1/doctor", get(doctor::checks))
        .route("/api/v1/doctor/fix", post(doctor::fix))
        .route("/api/v1/logs", get(logs::query))
        .route("/api/v1/resources", get(system_info::resources))
        .route("/api/v1/hal", get(system_info::hal))
        // OpenAI-compatible chat
        .route("/api/v1/chat/completions", post(chat::completions))
        // Chat sessions (persisted; read/manage — sending is via /chat/completions)
        .route(
            "/api/v1/chat/sessions",
            get(chat_sessions::list).post(chat_sessions::create),
        )
        .route(
            "/api/v1/chat/sessions/{id}",
            get(chat_sessions::get)
                .patch(chat_sessions::rename)
                .delete(chat_sessions::delete),
        )
        .route("/api/v1/chat/sessions/{id}/fork", post(chat_sessions::fork))
        .route(
            "/api/v1/chat/sessions/{id}/messages",
            get(chat_sessions::messages).post(chat_sessions::send),
        )
        .route(
            "/api/v1/chat/sessions/{id}/messages/stream",
            post(chat_sessions::send_stream),
        )
        .route(
            "/api/v1/chat/sessions/{id}/export",
            get(chat_sessions::export),
        )
        // Agent conversations (multi-agent convos)
        .route(
            "/api/v1/agent-chats",
            get(agent_chats::list).post(agent_chats::create),
        )
        .route("/api/v1/agent-chats/{id}", get(agent_chats::get))
        .route("/api/v1/agent-chats/{id}/stop", post(agent_chats::stop))
        // Agents
        .route("/api/v1/agents", get(agents::list).post(agents::connect))
        .route(
            "/api/v1/agents/{name}",
            get(agents::detail).delete(agents::disconnect),
        )
        .route(
            "/api/v1/agents/{name}/permissions",
            post(agents::grant_permission),
        )
        .route(
            "/api/v1/agents/{name}/settings",
            post(agents::update_settings),
        )
        .route(
            "/api/v1/agents/{name}/permissions/revoke",
            post(agents::revoke_permission),
        )
        // Files (upload+list share /api/v1/files; added separately with a larger body limit)
        .route("/api/v1/files/{id}/download", get(files::download))
        .route("/api/v1/files/{id}", get(files::get).delete(files::delete))
        // Scratchpad — global
        .route("/api/v1/scratchpad", get(scratchpad::list_global))
        .route(
            "/api/v1/scratchpad/{page}",
            get(scratchpad::get_global)
                .put(scratchpad::put_global)
                .delete(scratchpad::delete_global),
        )
        // Scratchpad — per-agent
        .route(
            "/api/v1/agents/{name}/scratchpad",
            get(scratchpad::list_agent),
        )
        .route(
            "/api/v1/agents/{name}/scratchpad/{page}",
            get(scratchpad::get_agent)
                .put(scratchpad::put_agent)
                .delete(scratchpad::delete_agent),
        )
        // Tasks
        .route("/api/v1/tasks", get(tasks::list))
        .route("/api/v1/tasks/run", post(tasks::run))
        .route("/api/v1/tasks/{id}", get(tasks::get))
        .route("/api/v1/tasks/{id}/cancel", post(tasks::cancel))
        .route("/api/v1/tasks/{id}/trace", get(tasks::trace))
        .route("/api/v1/tasks/{id}/resume", post(tasks::resume))
        .route("/api/v1/tasks/{id}/checkpoints", get(tasks::checkpoints))
        // Tools
        .route("/api/v1/tools", get(tools::list).post(tools::install))
        .route(
            "/api/v1/tools/{name}",
            get(tools::get).delete(tools::remove),
        )
        // Pipelines
        .route(
            "/api/v1/pipelines",
            get(pipelines::list).post(pipelines::save),
        )
        .route("/api/v1/pipelines/import", post(pipelines::import))
        .route(
            "/api/v1/pipelines/runs/{run_id}/events",
            get(pipelines::run_events),
        )
        .route("/api/v1/pipelines/{name}/run", post(pipelines::run))
        .route("/api/v1/pipelines/{name}/export", get(pipelines::export))
        .route(
            "/api/v1/pipelines/{name}",
            get(pipelines::get).delete(pipelines::delete),
        )
        // Secrets
        .route("/api/v1/secrets", get(secrets::list).post(secrets::set))
        .route("/api/v1/secrets/{name}", delete(secrets::revoke))
        // Audit
        .route("/api/v1/audit/logs", get(audit::logs))
        .route("/api/v1/audit/logs/{trace_id}", get(audit::detail))
        .route("/api/v1/audit/verify", get(audit::verify))
        // Costs
        .route("/api/v1/costs/summary", get(costs::summary))
        .route("/api/v1/costs/agents/{name}", get(costs::agent_costs))
        // Notifications
        .route("/api/v1/notifications", get(notifications::list))
        .route(
            "/api/v1/notifications/unread",
            get(notifications::unread_count),
        )
        .route(
            "/api/v1/notifications/read",
            delete(notifications::clear_read),
        )
        .route(
            "/api/v1/notifications/{id}",
            get(notifications::get).delete(notifications::dismiss),
        )
        .route(
            "/api/v1/notifications/{id}/respond",
            post(notifications::respond),
        )
        // Escalations (HITL)
        .route("/api/v1/escalations", get(escalations::list))
        .route("/api/v1/escalations/{id}", get(escalations::get))
        .route(
            "/api/v1/escalations/{id}/resolve",
            post(escalations::resolve),
        )
        // User-preference proposals (governance)
        .route("/api/v1/prefs/proposals", get(prefs::list_proposals))
        .route("/api/v1/prefs/proposals/{id}/accept", post(prefs::accept))
        .route("/api/v1/prefs/proposals/{id}/reject", post(prefs::reject))
        .route("/api/v1/prefs/stats", get(prefs::stats))
        // Roles (governance)
        .route("/api/v1/roles", get(roles::list).post(roles::create))
        .route(
            "/api/v1/roles/{name}",
            get(roles::get).delete(roles::delete),
        )
        // Schedules (cron automation)
        .route(
            "/api/v1/schedules",
            get(schedules::list).post(schedules::create),
        )
        .route("/api/v1/schedules/preview", post(schedules::preview))
        .route("/api/v1/schedules/{id}/pause", post(schedules::pause))
        .route("/api/v1/schedules/{id}/resume", post(schedules::resume))
        .route("/api/v1/schedules/{id}/runs", get(schedules::runs))
        .route("/api/v1/schedules/{id}", delete(schedules::delete))
        // Workflows (visual builder)
        .route(
            "/api/v1/workflows",
            get(workflows::list).post(workflows::create),
        )
        .route(
            "/api/v1/workflows/{id}",
            get(workflows::get)
                .put(workflows::update)
                .delete(workflows::delete),
        )
        // Plugins
        .route("/api/v1/plugins", get(plugins::list))
        .route("/api/v1/plugins/discover", post(plugins::discover))
        .route("/api/v1/plugins/{id}", get(plugins::detail))
        .route("/api/v1/plugins/{id}/enable", post(plugins::enable))
        .route("/api/v1/plugins/{id}/disable", post(plugins::disable))
        // Channels
        .route("/api/v1/channels", get(channels::list))
        .route("/api/v1/channels/{id}", get(channels::detail))
        .route(
            "/api/v1/channels/{id}/disconnect",
            post(channels::disconnect),
        )
        // MCP
        .route("/api/v1/mcp", get(mcp::list))
        .route("/api/v1/mcp/{name}/detach", post(mcp::detach))
        // Connectors
        .route("/api/v1/connectors", get(connectors::list))
        .route("/api/v1/connectors/{id}", get(connectors::detail))
        .route(
            "/api/v1/connectors/{id}/disconnect",
            post(connectors::disconnect),
        )
        // Events
        .route(
            "/api/v1/events/subscriptions",
            get(events::list_subscriptions).post(events::create_subscription),
        )
        .route(
            "/api/v1/events/subscriptions/{id}",
            delete(events::delete_subscription),
        )
        .route("/api/v1/events/emit", post(events::emit))
        // Realtime SSE stream (alternative to the WebSocket endpoint)
        .route("/api/v1/events/stream", get(sse::events_stream))
        // Marketplace (proxy to external registry)
        .route("/api/v1/marketplace", get(marketplace::search))
        .route("/api/v1/marketplace/{name}", get(marketplace::detail))
        .route(
            "/api/v1/marketplace/{name}/reviews",
            post(marketplace::review),
        )
        // Webhook endpoint management
        .route(
            "/api/v1/webhooks",
            get(webhooks_admin::list).post(webhooks_admin::create),
        )
        .route("/api/v1/webhooks/{id}/rotate", post(webhooks_admin::rotate))
        .route("/api/v1/webhooks/{id}", delete(webhooks_admin::delete))
        // Agent identity
        .route("/api/v1/agents/{name}/identity", get(identity::get))
        // Auth identity + API key management
        .route("/api/v1/auth/me", get(auth::me))
        .route("/api/v1/keys", get(keys::list).post(keys::create))
        .route("/api/v1/keys/{id}", delete(keys::revoke));

    // Optional key rotation, gated by `[api] refresh_enabled`.
    let protected_routes = if refresh_enabled {
        protected_routes.route("/api/v1/auth/refresh", post(auth::refresh))
    } else {
        protected_routes
    };

    // Files upload+list share `/api/v1/files`; this route alone gets a raised body
    // limit (100 MiB + 1 MiB form slack) so large uploads aren't rejected by the
    // default 2 MiB cap (other routes keep the default).
    let protected_routes = protected_routes.route(
        "/api/v1/files",
        get(files::list)
            .post(files::upload)
            .layer(DefaultBodyLimit::max(100 * 1024 * 1024 + 1024 * 1024)),
    );

    // Apply auth middleware to all protected routes.
    let protected_routes = protected_routes
        .layer(axum::middleware::from_fn(crate::auth::require_api_key))
        .layer(Extension(key_store.clone()));

    // ── Login: public but aggressively rate-limited to resist brute force ──
    // Separate, stricter governor than the global limiter (10-request burst,
    // ~1 req/s replenish) applied to just the login route.
    let login_governor = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(1)
            .burst_size(10)
            .finish()
            .ok_or_else(|| "invalid login rate-limit config".to_string())?,
    );
    let login_routes = Router::new()
        .route("/api/v1/auth/login", post(auth::login))
        .layer(GovernorLayer::new(login_governor));

    // ── Merge public + login + protected, add shared middleware ──────────
    // Extensions available to all routes (including WS upgrade and login).
    let app = public_routes
        .merge(login_routes)
        .merge(protected_routes)
        .layer(Extension(key_store))
        .layer(Extension(broadcaster))
        .with_state(service);

    // CORS: explicit allowlist from config, or same-origin (the bind address)
    // fallback when the list is empty.
    let cors = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
        .max_age(std::time::Duration::from_secs(3600));
    let cors = if cors_allowed_origins.is_empty() {
        let origin_addr = if bind_addr.ip().is_unspecified() {
            SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                bind_addr.port(),
            )
        } else {
            bind_addr
        };
        let origin = format!("http://{origin_addr}");
        cors.allow_origin(
            origin
                .parse::<HeaderValue>()
                .map_err(|e| format!("invalid CORS origin '{origin}': {e}"))?,
        )
    } else {
        let origins = cors_allowed_origins
            .iter()
            .map(|o| {
                o.parse::<HeaderValue>()
                    .map_err(|e| format!("invalid CORS origin '{o}': {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        cors.allow_origin(origins)
    };

    // Rate limiting: 120 req/min burst, 2 req/s steady replenishment.
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(120)
            .finish()
            .ok_or_else(|| "invalid governor rate-limit config".to_string())?,
    );

    Ok(app
        .layer(axum::middleware::from_fn(add_security_headers))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(GovernorLayer::new(governor_conf)))
}

/// Serve the generated OpenAPI 3.1 document as JSON (the React panel's contract).
async fn openapi_json() -> axum::Json<utoipa::openapi::OpenApi> {
    axum::Json(crate::openapi::ApiDoc::openapi())
}

/// Serve a Scalar API-reference UI that loads the spec from `/api/v1/openapi.json`.
async fn scalar_ui() -> axum::response::Html<&'static str> {
    axum::response::Html(SCALAR_HTML)
}

const SCALAR_HTML: &str = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>AgentOS API Reference</title>
  </head>
  <body>
    <script id="api-reference" data-url="/api/v1/openapi.json"></script>
    <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@1" crossorigin="anonymous"></script>
  </body>
</html>
"#;
