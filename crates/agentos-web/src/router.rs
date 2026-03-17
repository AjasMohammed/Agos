use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{HeaderValue, Method, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::auth::AuthToken;
use crate::handlers::{agents, audit, dashboard, pipelines, secrets, tasks, tools};
use crate::state::AppState;

/// Middleware that sets security headers on every response.
async fn add_security_headers(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        axum::http::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
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
        // Tasks
        .route("/tasks", axum::routing::get(tasks::list))
        .route("/tasks/{id}", axum::routing::get(tasks::detail))
        .route(
            "/tasks/{id}/logs/stream",
            axum::routing::get(tasks::log_stream),
        )
        // Tools
        .route(
            "/tools",
            axum::routing::get(tools::list).post(tools::install),
        )
        .route("/tools/{name}", axum::routing::delete(tools::remove))
        // Secrets
        .route(
            "/secrets",
            axum::routing::get(secrets::list).post(secrets::create),
        )
        .route("/secrets/{name}", axum::routing::delete(secrets::revoke))
        // Pipelines
        .route("/pipelines", axum::routing::get(pipelines::list))
        .route("/pipelines/run", axum::routing::post(pipelines::run))
        // Audit
        .route("/audit", axum::routing::get(audit::list))
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
            state,
            crate::csrf::csrf_middleware,
        ))
        // Auth middleware — must be inside the Extension layer so the token is available.
        .layer(axum::middleware::from_fn(crate::auth::require_auth))
        // Extension layer — adds auth_token to every request before auth middleware runs.
        .layer(axum::Extension(auth_token))
        // Security headers on all responses.
        .layer(axum::middleware::from_fn(add_security_headers))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        // Rate limiting outermost — applied first on every incoming request.
        .layer(GovernorLayer::new(governor_conf)))
}
