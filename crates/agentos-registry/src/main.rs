mod db;
mod handlers;
mod models;

use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use axum::Router;
use clap::Parser;
use std::path::PathBuf;
use tower_http::cors::CorsLayer;

#[derive(Parser)]
#[command(name = "agentos-registry")]
#[command(about = "AgentOS tool marketplace registry server")]
struct Args {
    /// Port to listen on.
    #[arg(long, default_value_t = 8090)]
    port: u16,

    /// Bind address.
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,

    /// Path to the SQLite database file.
    #[arg(long, default_value = "registry.db")]
    db: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let db = db::RegistryDb::open(&args.db)?;

    tracing::info!(port = args.port, db = %args.db.display(), "Starting registry server");

    let app = Router::new()
        .route(
            "/v1/tools",
            get(handlers::list_tools).post(handlers::publish_tool),
        )
        .route("/v1/tools/{name}", get(handlers::get_tool))
        .route("/v1/tools/{name}/versions", get(handlers::list_versions))
        .route(
            "/v1/tools/{name}/{version}",
            get(handlers::get_tool_version),
        )
        .route(
            "/v1/tools/{name}/{version}/dl",
            get(handlers::download_tool),
        )
        .layer(DefaultBodyLimit::max(1024 * 1024)) // 1 MiB
        .layer(CorsLayer::permissive())
        .with_state(db);

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", args.bind, args.port)).await?;
    tracing::info!("Listening on {}:{}", args.bind, args.port);
    axum::serve(listener, app).await?;
    Ok(())
}
