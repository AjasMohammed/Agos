use std::net::SocketAddr;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::auth::AuthToken;
use crate::router::build_router;
use crate::state::AppState;
use crate::templates::build_template_engine;
use agentos_kernel::notification_router::SseDeliveryAdapter;
use agentos_kernel::Kernel;

pub struct WebServer {
    bind_addr: SocketAddr,
    state: AppState,
}

impl WebServer {
    pub async fn new(
        bind_addr: SocketAddr,
        kernel: Arc<Kernel>,
        allowed_tool_dirs: Arc<Vec<std::path::PathBuf>>,
    ) -> Result<Self, anyhow::Error> {
        let templates = Arc::new(build_template_engine()?);

        let chat_db_path = kernel.data_dir().join("chat.db");
        let chat_store = Arc::new(
            crate::chat_store::ChatStore::open(&chat_db_path)
                .map_err(|e| anyhow::anyhow!("Failed to open chat store: {}", e))?,
        );

        // Create the notification broadcast channel and register the SSE adapter
        // with the kernel's NotificationRouter so it receives real-time pushes.
        let (notification_tx, _) = tokio::sync::broadcast::channel(256);
        let sse_adapter = SseDeliveryAdapter::new(notification_tx.clone());
        kernel
            .notification_router
            .register_adapter(Arc::new(sse_adapter))
            .await;

        let state = AppState {
            kernel,
            templates,
            csrf_tokens: Arc::new(dashmap::DashMap::<String, (String, std::time::Instant)>::new()),
            allowed_tool_dirs,
            chat_store,
            notification_tx,
        };
        Ok(Self { bind_addr, state })
    }

    pub async fn start(self) -> Result<(), anyhow::Error> {
        let auth_token = self.make_auth_token();
        let app = build_router(self.state, self.bind_addr, auth_token)?;
        let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
        tracing::info!("Web UI listening on http://{}", self.bind_addr);
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
        Ok(())
    }

    pub async fn start_with_shutdown(
        self,
        shutdown: CancellationToken,
    ) -> Result<(), anyhow::Error> {
        let auth_token = self.make_auth_token();

        // Periodically evict expired CSRF tokens to prevent unbounded map growth.
        // Tokens older than 2× TOKEN_TTL are safe to remove.
        let csrf_tokens = Arc::clone(&self.state.csrf_tokens);
        let sweep_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let sweep_interval = tokio::time::Duration::from_secs(30 * 60); // every 30 min
            let max_age = crate::csrf::TOKEN_TTL * 2;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(sweep_interval) => {
                        let cutoff = std::time::Instant::now() - max_age;
                        csrf_tokens.retain(|_, (_, issued_at)| *issued_at > cutoff);
                    }
                    _ = sweep_shutdown.cancelled() => break,
                }
            }
        });

        let app = build_router(self.state, self.bind_addr, auth_token)?;
        let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
        tracing::info!("Web UI listening on http://{}", self.bind_addr);
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
        Ok(())
    }

    fn make_auth_token(&self) -> AuthToken {
        let token = generate_auth_token();
        // Write to stderr so the token is not captured by stdout log aggregators.
        eprintln!("=== AgentOS Web UI ===");
        eprintln!("Auth token: {}", token.as_str());
        eprintln!(
            "Open http://{}/login and paste the token above to access the UI.",
            self.bind_addr
        );
        AuthToken(Arc::new(token))
    }
}

/// Generates a 32-byte cryptographically random token, returned as `Zeroizing<String>`
/// so the plaintext is cleared from memory when the value is dropped.
fn generate_auth_token() -> zeroize::Zeroizing<String> {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    zeroize::Zeroizing::new(hex::encode(bytes))
}
