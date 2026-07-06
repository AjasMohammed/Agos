use crate::kernel::Kernel;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    uptime_seconds: u64,
    /// Per-channel health, populated when running as a gateway with channels
    /// connected. Empty (and omitted) for non-gateway deployments.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    channels: Vec<ChannelHealthEntry>,
}

#[derive(Serialize)]
struct ChannelHealthEntry {
    channel: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct ReadyResponse {
    status: &'static str,
    connected_agents: usize,
    active_tasks: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

async fn healthz(State(kernel): State<Arc<Kernel>>) -> Json<HealthResponse> {
    let uptime = (chrono::Utc::now() - kernel.started_at)
        .num_seconds()
        .max(0) as u64;

    let mut channels: Vec<ChannelHealthEntry> = kernel
        .channel_manager
        .health()
        .await
        .into_iter()
        .map(|(channel, health)| {
            let (status, detail) = match health {
                agentos_channels::ChannelHealth::Connected => ("ok", None),
                agentos_channels::ChannelHealth::Degraded(msg) => ("degraded", Some(msg)),
                agentos_channels::ChannelHealth::Disconnected(msg) => ("down", Some(msg)),
            };
            ChannelHealthEntry {
                channel,
                status,
                detail,
            }
        })
        .collect();
    channels.sort_by(|a, b| a.channel.cmp(&b.channel));

    Json(HealthResponse {
        status: "ok",
        uptime_seconds: uptime,
        channels,
    })
}

async fn readyz(State(kernel): State<Arc<Kernel>>) -> (StatusCode, Json<ReadyResponse>) {
    let agents = kernel.agent_registry.read().await.list_online().len();
    let tasks = kernel.scheduler.running_count().await;

    // The kernel is ready if the bus, scheduler, and subsystems are initialized.
    // Agent presence is informational — a kernel with zero agents can still
    // accept connections and is ready from an infrastructure standpoint.
    let reason = if agents == 0 {
        Some("no agents connected (kernel accepting connections)".to_string())
    } else {
        None
    };

    (
        StatusCode::OK,
        Json(ReadyResponse {
            status: "ready",
            connected_agents: agents,
            active_tasks: tasks,
            reason,
        }),
    )
}

async fn metrics_handler(State(handle): State<PrometheusHandle>) -> impl IntoResponse {
    handle.render()
}

pub fn health_router(kernel: Arc<Kernel>, prom_handle: PrometheusHandle) -> Router {
    // The /metrics endpoint uses its own state (PrometheusHandle), so we nest it separately
    let metrics_router = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(prom_handle);

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(kernel)
        .merge(metrics_router)
}

/// Install the Prometheus metrics recorder. Must be called once before any metrics are recorded.
/// Returns a handle that can render the metrics as text for the /metrics endpoint.
pub fn install_prometheus_recorder() -> Option<PrometheusHandle> {
    let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
    match builder.install_recorder() {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to install Prometheus metrics recorder (may already be installed)");
            None
        }
    }
}

/// Start the health HTTP server. Returns the actual bound address (useful when port is 0).
/// If health_port is 0 in config, this is a no-op.
pub async fn start_health_server(
    kernel: Arc<Kernel>,
    prom_handle: PrometheusHandle,
) -> Result<Option<std::net::SocketAddr>, anyhow::Error> {
    let port = kernel.config.kernel.health_port;
    if port == 0 {
        return Ok(None);
    }

    let bind: std::net::IpAddr = kernel.config.kernel.health_bind.parse().map_err(|e| {
        anyhow::anyhow!(
            "invalid [kernel] health_bind '{}': {e}",
            kernel.config.kernel.health_bind
        )
    })?;
    let addr = std::net::SocketAddr::new(bind, port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_addr = listener.local_addr()?;

    let router = health_router(kernel, prom_handle);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "Health server error");
        }
    });

    tracing::info!(addr = %actual_addr, "Health server started");
    Ok(Some(actual_addr))
}
