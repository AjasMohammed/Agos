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

        let convo_db_path = kernel.data_dir().join("agent_convos.db");
        let convo_store = Arc::new(
            crate::convo_store::ConvoStore::open(&convo_db_path)
                .map_err(|e| anyhow::anyhow!("Failed to open convo store: {}", e))?,
        );

        let file_store = Arc::new(
            crate::file_store::FileStore::open(kernel.data_dir())
                .map_err(|e| anyhow::anyhow!("Failed to open file store: {}", e))?,
        );

        let resolver: Arc<dyn agentos_llm::ImageResolver> =
            match crate::handlers::files::FileStoreImageResolver::new(Arc::clone(&file_store)) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Could not canonicalize uploads dir — FileRef images disabled"
                    );
                    Arc::new(agentos_llm::NoopImageResolver)
                }
            };
        kernel.set_image_resolver(resolver);

        // Persist inbound channel media (Telegram photos/docs/voice) into the
        // same FileStore so downloaded attachments get a resolvable file id.
        kernel.set_attachment_sink(Arc::new(
            crate::handlers::files::FileStoreAttachmentSink::new(Arc::clone(&file_store)),
        ));

        // Create the notification broadcast channel and register the SSE adapter
        // with the kernel's NotificationRouter so it receives real-time pushes.
        let (notification_tx, _) = tokio::sync::broadcast::channel(256);
        let sse_adapter = SseDeliveryAdapter::new(notification_tx.clone());
        kernel
            .notification_router
            .register_adapter(Arc::new(sse_adapter))
            .await;

        let service: Arc<dyn agentos_api::KernelService> =
            Arc::clone(&kernel) as Arc<dyn agentos_api::KernelService>;
        let state = AppState {
            kernel,
            service,
            templates,
            csrf_tokens: Arc::new(dashmap::DashMap::<String, (String, std::time::Instant)>::new()),
            browser_sessions: Arc::new(dashmap::DashMap::new()),
            allowed_tool_dirs,
            chat_store,
            inflight_chat: Arc::new(crate::chat_inflight::InFlightChat::new()),
            convo_store,
            inflight_convos: Arc::new(crate::convo_inflight::InFlightConvos::new()),
            file_store,
            notification_tx,
            secure_cookies: !bind_addr.ip().is_loopback(),
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
        let browser_sessions = Arc::clone(&self.state.browser_sessions);
        let sweep_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let sweep_interval = tokio::time::Duration::from_secs(30 * 60); // every 30 min
            let max_age = crate::csrf::TOKEN_TTL * 2;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(sweep_interval) => {
                        let cutoff = std::time::Instant::now() - max_age;
                        csrf_tokens.retain(|_, (_, issued_at)| *issued_at > cutoff);
                        browser_sessions.retain(|_, issued_at| *issued_at > cutoff);
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
