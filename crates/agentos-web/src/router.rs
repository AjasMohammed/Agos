use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{HeaderValue, Method, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::auth::AuthToken;
use crate::handlers::{
    a2a, agent_convo, agent_detail, agents, audit, channels, chat, config_page, connectors, costs,
    dashboard, doctor, escalations, events, events_log, files, hal_page, identity_page, logs,
    management, manual_page, marketplace, mcp_page, notifications, oauth, observability,
    pipeline_ui, pipelines, plugins, prefs, profile, resources_page, roles, schedules, scratchpad,
    secrets, tasks, teams, tools, webhooks, webhooks_page,
};
use crate::state::AppState;

/// Middleware that sets security headers on every response.
async fn add_security_headers(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        axum::http::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            // 'unsafe-eval' is required by the standard Alpine.js build, which compiles
            // x-data / x-show / @click expression strings via `new Function(...)`. The
            // CSP-friendly Alpine build (alpinejs/csp) avoids this but disallows inline
            // expressions in templates — switching would require rewriting every template
            // that embeds an Alpine component.
            "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
             style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             font-src 'self' https://fonts.gstatic.com; \
             img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        axum::http::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

pub fn build_router(
    state: AppState,
    bind_addr: SocketAddr,
    auth_token: AuthToken,
) -> Result<Router, anyhow::Error> {
    // CORS: allow only the bound address origin.
    // Replace INADDR_ANY (0.0.0.0) with 127.0.0.1 so the header value is a valid origin.
    let origin = format!(
        "http://{}",
        if bind_addr.ip().is_unspecified() {
            std::net::SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                bind_addr.port(),
            )
        } else {
            bind_addr
        }
    );
    let cors = CorsLayer::new()
        .allow_origin(
            origin
                .parse::<HeaderValue>()
                .map_err(|e| anyhow::anyhow!("invalid CORS origin '{}': {}", origin, e))?,
        )
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_credentials(true)
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderName::from_static("x-csrf-token"),
        ]);

    // Rate limiting: 60 req/min burst, 1 req/s steady replenishment.
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(1)
            .burst_size(60)
            .finish()
            .ok_or_else(|| anyhow::anyhow!("invalid governor rate-limit config"))?,
    );

    // Unauthenticated routes — external services cannot carry our auth token.
    // These are merged AFTER the authenticated router so they bypass the auth layer
    // but still get security headers, compression, tracing, and rate limiting.
    let webhook_routes = Router::new()
        .route(
            "/api/v1/webhooks/incoming/{endpoint_id}",
            axum::routing::post(webhooks::incoming_webhook),
        )
        // Restrict body size on the unauthenticated webhook endpoint to prevent
        // memory exhaustion DoS. Most webhook payloads are well under 100 KiB.
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024)) // 256 KiB
        .with_state(Arc::new(state.clone()));

    // Telegram Bot API webhooks — same handler as `agentos-api`; must live on the
    // Web UI server because `agentos web serve` only exposes this Axum app (not the
    // standalone REST API). Without this route, `setWebhook` succeeds but Telegram
    // POSTs hit 404 and chat_id auto-discovery never runs.
    let telegram_webhook_routes = Router::new()
        .route(
            "/api/v1/webhooks/telegram/{channel_id}",
            axum::routing::post(agentos_api::handlers::webhooks::telegram_webhook),
        )
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .with_state(state.service.clone());

    Ok(Router::new()
        .route("/", axum::routing::get(dashboard::index))
        // Login (bypasses auth middleware — see require_auth)
        .route(
            "/login",
            axum::routing::get(crate::auth::login_page).post(crate::auth::login_submit),
        )
        // Agents
        .route(
            "/agents",
            axum::routing::get(agents::list).post(agents::connect),
        )
        .route("/agents/{name}", axum::routing::delete(agents::disconnect))
        // Agent detail
        .route(
            "/agents/{name}/detail",
            axum::routing::get(agent_detail::detail),
        )
        .route(
            "/agents/{name}/identity",
            axum::routing::get(identity_page::page),
        )
        .route(
            "/agents/{name}/permissions",
            axum::routing::post(agent_detail::grant_permission),
        )
        .route(
            "/agents/{name}/permissions/revoke",
            axum::routing::post(agent_detail::revoke_permission),
        )
        .route(
            "/agents/{name}/settings",
            axum::routing::post(agent_detail::update_settings),
        )
        // Tasks
        .route("/tasks", axum::routing::get(tasks::list))
        .route("/tasks/{id}", axum::routing::get(tasks::detail))
        .route("/tasks/{id}/cancel", axum::routing::post(tasks::cancel))
        .route("/tasks/{id}/resume", axum::routing::post(tasks::resume))
        .route("/tasks/{id}/trace", axum::routing::get(tasks::trace_page))
        .route(
            "/tasks/{id}/snapshots",
            axum::routing::get(tasks::snapshots),
        )
        .route(
            "/tasks/{id}/logs/stream",
            axum::routing::get(tasks::log_stream),
        )
        .route(
            "/api/tasks/{id}/trace",
            axum::routing::get(tasks::trace_json),
        )
        .route(
            "/api/tasks/{id}/context/{idx}/raw",
            axum::routing::get(tasks::context_raw),
        )
        // Tools
        .route(
            "/tools",
            axum::routing::get(tools::list).post(tools::install),
        )
        .route("/tools/{name}", axum::routing::delete(tools::remove))
        // Marketplace
        .route("/marketplace", axum::routing::get(marketplace::list))
        .route(
            "/marketplace/{name}",
            axum::routing::get(marketplace::detail),
        )
        .route(
            "/marketplace/{name}/review",
            axum::routing::post(marketplace::submit_review),
        )
        // Secrets
        .route(
            "/secrets",
            axum::routing::get(secrets::list).post(secrets::create),
        )
        .route("/secrets/{name}", axum::routing::delete(secrets::revoke))
        // Connectors & OAuth
        .route(
            "/connectors",
            axum::routing::get(connectors::list_connectors),
        )
        .route(
            "/connectors/{connector_id}/disconnect",
            axum::routing::post(connectors::disconnect_connector),
        )
        .route(
            "/api/connectors",
            axum::routing::get(connectors::list_connectors_json),
        )
        .route(
            "/auth/{connector_id}/start",
            axum::routing::get(oauth::start_oauth),
        )
        .route(
            "/auth/{connector_id}/callback",
            axum::routing::get(oauth::oauth_callback),
        )
        // Pipelines
        .route("/pipelines", axum::routing::get(pipeline_ui::list))
        .route(
            "/pipelines/new",
            axum::routing::get(pipeline_ui::new_builder),
        )
        .route(
            "/pipelines/{name}/edit",
            axum::routing::get(pipeline_ui::edit_builder),
        )
        .route(
            "/pipelines/{name}/clone",
            axum::routing::post(pipeline_ui::clone_pipeline),
        )
        .route(
            "/pipelines/{name}/delete",
            axum::routing::post(pipeline_ui::delete_pipeline),
        )
        .route("/pipelines/run", axum::routing::post(pipelines::run))
        .route(
            "/api/pipelines",
            axum::routing::post(pipeline_ui::save_pipeline),
        )
        .route(
            "/api/pipelines/import",
            axum::routing::post(pipeline_ui::import_yaml),
        )
        .route(
            "/api/pipelines/export",
            axum::routing::post(pipeline_ui::export_yaml),
        )
        .route(
            "/api/pipelines/run",
            axum::routing::post(pipeline_ui::run_pipeline),
        )
        .route(
            "/api/pipelines/runs/{run_id}/events",
            axum::routing::get(pipeline_ui::run_events),
        )
        // Dashboard partials
        .route(
            "/dashboard-stats",
            axum::routing::get(dashboard::stats_partial),
        )
        .route(
            "/dashboard-agents",
            axum::routing::get(dashboard::agents_partial),
        )
        .route(
            "/dashboard-tasks",
            axum::routing::get(dashboard::tasks_partial),
        )
        .route(
            "/dashboard-recent-audit",
            axum::routing::get(dashboard::recent_audit_partial),
        )
        // Agent-to-Agent Conversations
        .route("/agent-chat", axum::routing::get(agent_convo::list))
        .route(
            "/agent-chat/new",
            axum::routing::post(agent_convo::new_convo),
        )
        .route("/agent-chat/{id}", axum::routing::get(agent_convo::detail))
        .route(
            "/agent-chat/{id}/stop",
            axum::routing::post(agent_convo::stop),
        )
        .route(
            "/agent-chat/{id}/stream",
            axum::routing::get(agent_convo::stream),
        )
        // File upload and management
        .route("/files", axum::routing::get(files::list))
        .route(
            "/files/upload",
            axum::routing::post(files::upload)
                .layer(axum::extract::DefaultBodyLimit::max(101 * 1024 * 1024)),
        )
        .route("/files/{id}/delete", axum::routing::post(files::delete))
        .route("/files/{id}/download", axum::routing::get(files::download))
        .route(
            "/api/files/upload",
            axum::routing::post(files::upload_api)
                .layer(axum::extract::DefaultBodyLimit::max(101 * 1024 * 1024)),
        )
        .route("/api/files/search", axum::routing::get(files::search_api))
        // Chat (session-based, separate from the task system)
        .route("/chat", axum::routing::get(chat::list))
        .route("/chat/new", axum::routing::post(chat::new_session))
        .route("/chat/{session_id}", axum::routing::get(chat::conversation))
        .route(
            "/chat/{session_id}/rename",
            axum::routing::post(chat::rename_session),
        )
        .route(
            "/chat/{session_id}/delete",
            axum::routing::post(chat::delete_session),
        )
        .route(
            "/chat/{session_id}/fork",
            axum::routing::post(chat::fork_session),
        )
        .route(
            "/chat/{session_id}/export",
            axum::routing::get(chat::export_session),
        )
        .route("/chat/{session_id}/send", axum::routing::post(chat::send))
        .route("/chat/{session_id}/stop", axum::routing::post(chat::stop))
        .route(
            "/chat/{session_id}/stream",
            axum::routing::get(chat::message_stream),
        )
        // Notifications (UNIS Phase 2)
        .route("/notifications", axum::routing::get(notifications::inbox))
        .route(
            "/notifications/stream",
            axum::routing::get(notifications::notification_stream),
        )
        .route(
            "/notifications/unread-count",
            axum::routing::get(notifications::unread_count),
        )
        .route(
            "/notifications/read",
            axum::routing::delete(notifications::clear_read_notifications),
        )
        .route(
            "/notifications/{id}",
            axum::routing::get(notifications::get_notification)
                .delete(notifications::dismiss_notification),
        )
        .route(
            "/notifications/{id}/respond",
            axum::routing::post(notifications::respond_to_notification),
        )
        // Costs
        .route("/costs", axum::routing::get(costs::dashboard))
        .route(
            "/api/costs/summary",
            axum::routing::get(costs::summary_json),
        )
        // Audit
        .route("/audit", axum::routing::get(audit::list))
        .route("/audit/{trace_id}", axum::routing::get(audit::detail))
        // Dedicated management parity pages
        .route("/plugins", axum::routing::get(plugins::list))
        .route("/plugins/discover", axum::routing::post(plugins::discover))
        .route("/plugins/{id}", axum::routing::get(plugins::detail))
        .route("/plugins/{id}/enable", axum::routing::post(plugins::enable))
        .route(
            "/plugins/{id}/disable",
            axum::routing::post(plugins::disable),
        )
        .route("/channels", axum::routing::get(channels::list))
        .route(
            "/channels/{id}/disconnect",
            axum::routing::post(channels::disconnect),
        )
        .route(
            "/schedules",
            axum::routing::get(schedules::list).post(schedules::create),
        )
        .route(
            "/api/schedules/preview",
            axum::routing::post(schedules::preview),
        )
        .route(
            "/schedules/{id}/pause",
            axum::routing::post(schedules::pause),
        )
        .route(
            "/schedules/{id}/resume",
            axum::routing::post(schedules::resume),
        )
        .route(
            "/schedules/{id}/delete",
            axum::routing::post(schedules::delete),
        )
        .route(
            "/roles",
            axum::routing::get(roles::list).post(roles::create),
        )
        .route("/roles/{name}", axum::routing::get(roles::detail))
        .route("/roles/{name}/delete", axum::routing::post(roles::delete))
        .route("/config", axum::routing::get(config_page::page))
        .route("/escalations", axum::routing::get(escalations::list))
        .route("/prefs", axum::routing::get(prefs::list))
        .route("/prefs/accept", axum::routing::post(prefs::accept))
        .route("/prefs/reject", axum::routing::post(prefs::reject))
        .route("/profile", axum::routing::get(profile::list))
        .route("/profile/forget", axum::routing::post(profile::forget))
        .route("/profile/edit", axum::routing::post(profile::edit))
        .route(
            "/escalations/{id}/resolve",
            axum::routing::post(escalations::resolve),
        )
        .route("/mcp", axum::routing::get(mcp_page::list))
        .route("/mcp/{name}/detach", axum::routing::post(mcp_page::detach))
        .route(
            "/webhooks",
            axum::routing::get(webhooks_page::list).post(webhooks_page::create),
        )
        .route(
            "/webhooks/{id}/delete",
            axum::routing::post(webhooks_page::delete),
        )
        .route(
            "/webhooks/{id}/rotate",
            axum::routing::post(webhooks_page::rotate),
        )
        .route("/doctor", axum::routing::get(doctor::page))
        .route("/manual", axum::routing::get(manual_page::page))
        .route("/manual/view", axum::routing::get(manual_page::view))
        .route("/scratchpad", axum::routing::get(scratchpad::page))
        .route(
            "/agents/{name}/scratchpad",
            axum::routing::get(scratchpad::agent_page),
        )
        .route("/resources", axum::routing::get(resources_page::page))
        .route("/events", axum::routing::get(events_log::page))
        .route("/events-log", axum::routing::get(events_log::page))
        .route(
            "/events/subscribe",
            axum::routing::post(events_log::create_subscription),
        )
        .route(
            "/events/subscriptions/{id}/delete",
            axum::routing::post(events_log::delete_subscription),
        )
        .route(
            "/events/emit",
            axum::routing::post(events_log::emit_test_event),
        )
        .route("/logs", axum::routing::get(logs::page))
        .route("/hal", axum::routing::get(hal_page::page))
        .route("/teams", axum::routing::get(teams::page))
        .route("/teams/{id}", axum::routing::get(teams::detail))
        .route("/a2a", axum::routing::get(a2a::page))
        // Management + observability parity pages
        .route("/management", axum::routing::get(management::page))
        .route(
            "/management/plugins/{id}/enable",
            axum::routing::post(management::plugin_enable),
        )
        .route(
            "/management/plugins/{id}/disable",
            axum::routing::post(management::plugin_disable),
        )
        .route(
            "/management/schedules/{id}/pause",
            axum::routing::post(management::schedule_pause),
        )
        .route(
            "/management/schedules/{id}/resume",
            axum::routing::post(management::schedule_resume),
        )
        .route(
            "/management/schedules/{id}/delete",
            axum::routing::post(management::schedule_delete),
        )
        .route("/observability", axum::routing::get(observability::page))
        // SSE event streams
        .route(
            "/events/dashboard",
            axum::routing::get(events::dashboard_stream),
        )
        .route("/events/agents", axum::routing::get(events::agents_stream))
        .route("/events/tasks", axum::routing::get(events::tasks_stream))
        .route("/events/costs", axum::routing::get(events::costs_stream))
        // Static files (served without auth — bypassed inside require_auth)
        .nest_service(
            "/static",
            ServeDir::new(
                std::env::var("AGENTOS_STATIC_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| {
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static")
                    }),
            ),
        )
        .with_state(state.clone())
        // Execution order (Axum layers run outermost-first on requests):
        // GovernorLayer → CorsLayer → TraceLayer → CompressionLayer → add_security_headers
        //   → Extension(auth_token) → require_auth → csrf_middleware → handler
        // CSRF middleware runs after auth, so only authenticated sessions reach it.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::csrf::csrf_middleware,
        ))
        // Auth middleware — must be inside the Extension layer so the token is available.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_auth,
        ))
        // Extension layer — adds auth_token to every request before auth middleware runs.
        .layer(axum::Extension(auth_token))
        // Merge unauthenticated webhook routes — placed after auth layer so they
        // bypass auth/CSRF but still get security headers, compression, and rate limiting.
        .merge(webhook_routes)
        .merge(telegram_webhook_routes)
        // Security headers on all responses.
        .layer(axum::middleware::from_fn(add_security_headers))
        .layer(CompressionLayer::new().compress_when(
            DefaultPredicate::new().and(NotForContentType::new("text/event-stream")),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        // Rate limiting outermost — applied first on every incoming request.
        .layer(GovernorLayer::new(governor_conf)))
}
