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

        // Chat + convo stores now live on the kernel (shared with the REST API);
        // reuse those instances rather than opening second connections.
        let chat_store = kernel.chat_store.clone();
        let convo_store = kernel.convo_store.clone();

        // FileStore now lives on the kernel (shared with the REST API); reuse that
        // instance rather than opening a second connection to the same DB.
        let file_store = kernel.file_store.clone();

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
        let (token, source) = self.resolve_auth_token();
        eprintln!("=== AgentOS Web UI ===");
        match source {
            // Only print the secret when we just generated it (first boot). On
            // normal restarts the token is stable, so we never reprint it —
            // this keeps the token out of logs on every restart.
            TokenSource::GeneratedAndPersisted(ref path) => {
                eprintln!("Generated a new Web UI auth token (no env/config token set).");
                eprintln!("Auth token: {}", token.as_str());
                eprintln!("Persisted to {} (mode 0600) — reused on restart.", path);
                eprintln!(
                    "Open http://{}/login and paste the token above to access the UI.",
                    self.bind_addr
                );
            }
            TokenSource::Env => {
                eprintln!("Web UI auth token loaded from $AGENTOS_WEB_TOKEN.");
                eprintln!("Open http://{}/login to access the UI.", self.bind_addr);
            }
            TokenSource::Config => {
                eprintln!("Web UI auth token loaded from [web].auth_token config.");
                eprintln!("Open http://{}/login to access the UI.", self.bind_addr);
            }
            TokenSource::PersistedFile(ref path) => {
                eprintln!("Web UI auth token loaded from {path}.");
                eprintln!("Open http://{}/login to access the UI.", self.bind_addr);
            }
        }
        AuthToken(Arc::new(token))
    }

    /// Resolves the Web UI auth token from, in precedence order:
    /// 1. `$AGENTOS_WEB_TOKEN`
    /// 2. `[web].auth_token` in config
    /// 3. A token persisted to `{data_dir}/web_token` (generated on first boot)
    ///
    /// The token is *stable across restarts* — only the first-boot generation
    /// path produces a fresh value.
    fn resolve_auth_token(&self) -> (zeroize::Zeroizing<String>, TokenSource) {
        // 1. Environment variable — never persisted, ideal for systemd/Docker.
        if let Ok(env_token) = std::env::var("AGENTOS_WEB_TOKEN") {
            let trimmed = env_token.trim();
            if !trimmed.is_empty() {
                return (
                    zeroize::Zeroizing::new(trimmed.to_string()),
                    TokenSource::Env,
                );
            }
        }

        // 2. Explicit config value.
        if let Some(cfg_token) = self.state.kernel.config.web.auth_token.as_deref() {
            let trimmed = cfg_token.trim();
            if !trimmed.is_empty() {
                return (
                    zeroize::Zeroizing::new(trimmed.to_string()),
                    TokenSource::Config,
                );
            }
        }

        // 3. Persisted file in the kernel data dir.
        let path = self.state.kernel.data_dir().join("web_token");
        let path_display = path.display().to_string();
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let trimmed = existing.trim();
            if !trimmed.is_empty() {
                return (
                    zeroize::Zeroizing::new(trimmed.to_string()),
                    TokenSource::PersistedFile(path_display),
                );
            }
        }

        // First boot (or unreadable/empty file): generate and persist.
        let token = generate_auth_token();
        match persist_token(&path, token.as_str()) {
            Ok(()) => (token, TokenSource::GeneratedAndPersisted(path_display)),
            Err(e) => {
                // Could not persist — fall back to an ephemeral token for this
                // run and warn loudly. The token still works until restart.
                tracing::warn!(
                    error = %e,
                    path = %path_display,
                    "Could not persist Web UI auth token; using an ephemeral token for this run"
                );
                (token, TokenSource::GeneratedAndPersisted(path_display))
            }
        }
    }
}

/// Where the resolved auth token came from — drives the startup banner.
enum TokenSource {
    Env,
    Config,
    PersistedFile(String),
    GeneratedAndPersisted(String),
}

/// Generates a 32-byte cryptographically random token, returned as `Zeroizing<String>`
/// so the plaintext is cleared from memory when the value is dropped.
fn generate_auth_token() -> zeroize::Zeroizing<String> {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    zeroize::Zeroizing::new(hex::encode(bytes))
}

/// Writes the token to `path`, restricting permissions to `0600` on Unix so
/// only the owner can read the secret.
fn persist_token(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_is_64_hex_chars() {
        let token = generate_auth_token();
        assert_eq!(token.len(), 64, "32 random bytes => 64 hex chars");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        // Two generations should not collide.
        assert_ne!(token.as_str(), generate_auth_token().as_str());
    }

    #[test]
    fn persist_token_writes_value_and_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("web_token");
        persist_token(&path, "deadbeef").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deadbeef");
    }

    #[cfg(unix)]
    #[test]
    fn persist_token_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web_token");
        persist_token(&path, "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "token file must be owner read/write only"
        );
    }
}
