//! HTTP router — maps all `/api/v1/*` routes to handler functions with middleware.
//!
//! The middleware stack (outermost → innermost on requests):
//! 1. Rate limiting (tower-governor)
//! 2. CORS
//! 3. Tracing
//! 4. Compression
//! 5. Security headers
//! 6. Bearer auth (on protected routes only)

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
    agents, audit, chat, costs, notifications, pipelines, secrets, system, tasks, tools, webhooks,
};
use crate::service::KernelService;
use crate::ws;
use crate::ws::broadcaster::WsBroadcaster;

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
pub fn build_router(
    service: Arc<dyn KernelService>,
    key_store: ApiKeyStore,
    broadcaster: WsBroadcaster,
    bind_addr: SocketAddr,
) -> Result<Router, String> {
    // ── Public routes (no auth via header — WS uses query param) ────────
    let public_routes = Router::new()
        .route("/api/v1/health", get(system::health))
        .route("/api/v1/ws", get(ws::ws_upgrade))
        // Telegram webhook — public, authenticated via secret_token header.
        .route(
            "/api/v1/webhooks/telegram/{channel_id}",
            post(webhooks::telegram_webhook),
        );

    // ── Protected routes (require Bearer token) ─────────────────────────
    let protected_routes = Router::new()
        // System
        .route("/api/v1/status", get(system::status))
        // OpenAI-compatible chat
        .route("/api/v1/chat/completions", post(chat::completions))
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
            "/api/v1/agents/{name}/permissions/revoke",
            post(agents::revoke_permission),
        )
        // Tasks
        .route("/api/v1/tasks", get(tasks::list))
        .route("/api/v1/tasks/run", post(tasks::run))
        .route("/api/v1/tasks/{id}", get(tasks::get))
        .route("/api/v1/tasks/{id}/cancel", post(tasks::cancel))
        .route("/api/v1/tasks/{id}/trace", get(tasks::trace))
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
        .route("/api/v1/pipelines/{name}", delete(pipelines::delete))
        .route("/api/v1/pipelines/{name}/run", post(pipelines::run))
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
        .route("/api/v1/notifications/{id}", get(notifications::get))
        .route(
            "/api/v1/notifications/{id}/respond",
            post(notifications::respond),
        )
        // Apply auth middleware to all protected routes.
        .layer(axum::middleware::from_fn(crate::auth::require_api_key))
        .layer(Extension(key_store.clone()));

    // ── Merge public + protected, add shared middleware ──────────────────
    // Extensions available to all routes (including WS upgrade).
    let app = public_routes
        .merge(protected_routes)
        .layer(Extension(key_store))
        .layer(Extension(broadcaster))
        .with_state(service);

    // CORS origin: use the bind address, replacing 0.0.0.0 with 127.0.0.1.
    let origin_addr = if bind_addr.ip().is_unspecified() {
        SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            bind_addr.port(),
        )
    } else {
        bind_addr
    };
    let origin = format!("http://{origin_addr}");
    let cors = CorsLayer::new()
        .allow_origin(
            origin
                .parse::<HeaderValue>()
                .map_err(|e| format!("invalid CORS origin '{origin}': {e}"))?,
        )
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ]);

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
