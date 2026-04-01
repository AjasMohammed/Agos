//! API server entry point — binds to a socket and serves the REST API + WebSocket.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::api_key::ApiKeyStore;
use crate::router::build_router;
use crate::service::KernelService;
use crate::ws::broadcaster::WsBroadcaster;

/// Start the API server on the given address.
///
/// This function blocks until the server shuts down. It should be spawned
/// onto the Tokio runtime alongside the kernel's main loop.
///
/// # Arguments
/// * `service` — The `KernelService` implementation.
/// * `key_store` — API key store for authentication.
/// * `broadcaster` — WebSocket event broadcaster (wire to kernel events externally).
/// * `addr` — Socket address to bind to (e.g. `0.0.0.0:8080`).
pub async fn run_api_server(
    service: Arc<dyn KernelService>,
    key_store: ApiKeyStore,
    broadcaster: WsBroadcaster,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = build_router(service, key_store, broadcaster, addr)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;

    tracing::info!("AgentOS API server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
