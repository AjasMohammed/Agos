use crate::handlers::{agents, audit, dashboard, pipelines, secrets, tasks, tools};
use crate::state::AppState;
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", axum::routing::get(dashboard::index))
        // Agents
        .route("/agents", axum::routing::get(agents::list).post(agents::connect))
        .route("/agents/{name}", axum::routing::delete(agents::disconnect))
        // Tasks
        .route("/tasks", axum::routing::get(tasks::list))
        .route("/tasks/{id}", axum::routing::get(tasks::detail))
        .route("/tasks/{id}/logs/stream", axum::routing::get(tasks::log_stream))
        // Tools
        .route("/tools", axum::routing::get(tools::list).post(tools::install))
        .route("/tools/{name}", axum::routing::delete(tools::remove))
        // Secrets
        .route("/secrets", axum::routing::get(secrets::list).post(secrets::create))
        .route("/secrets/{name}", axum::routing::delete(secrets::revoke))
        // Pipelines
        .route("/pipelines", axum::routing::get(pipelines::list))
        .route("/pipelines/run", axum::routing::post(pipelines::run))
        // Audit
        .route("/audit", axum::routing::get(audit::list))
        // Static files
        .nest_service("/static", ServeDir::new("crates/agentos-web/static"))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
}
