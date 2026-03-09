use crate::router::build_router;
use crate::state::AppState;
use crate::templates::build_template_engine;
use agentos_kernel::Kernel;
use std::net::SocketAddr;
use std::sync::Arc;

pub struct WebServer {
    bind_addr: SocketAddr,
    state: AppState,
}

impl WebServer {
    pub fn new(bind_addr: SocketAddr, kernel: Arc<Kernel>) -> Self {
        let templates = Arc::new(build_template_engine());
        let state = AppState { kernel, templates };
        Self { bind_addr, state }
    }

    pub async fn start(self) -> Result<(), anyhow::Error> {
        let app = build_router(self.state);
        let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
        tracing::info!("Web UI listening on http://{}", self.bind_addr);
        axum::serve(listener, app).await?;
        Ok(())
    }
}
