use crate::chat_inflight::InFlightChat;
use crate::chat_store::ChatStore;
use crate::convo_inflight::InFlightConvos;
use crate::convo_store::ConvoStore;
use crate::file_store::FileStore;
use agentos_api::KernelService;
use agentos_kernel::notification_router::NotificationSsePayload;
use agentos_kernel::Kernel;
use dashmap::DashMap;
use minijinja::Environment;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    /// Direct kernel handle — use only for handlers not yet migrated to `service`.
    /// New handler code must use `service` instead. This field is being phased out as
    /// part of the KernelService migration (see plans/api-layer/).
    pub kernel: Arc<Kernel>,
    /// Service abstraction over the kernel — required for all new handler code.
    pub service: Arc<dyn KernelService>,
    pub templates: Arc<Environment<'static>>,
    /// Per-session CSRF tokens: SHA-256(session_cookie) -> (csrf_token, issued_at).
    ///
    /// - Key is the SHA-256 hash of the raw session cookie value so the auth credential is
    ///   never stored as a plain String in this map.
    /// - Value is (64-char hex CSRF token, Instant it was issued).
    /// - Tokens are regenerated after 8 h (matching the session cookie max-age).
    pub csrf_tokens: Arc<DashMap<String, (String, Instant)>>,
    /// Browser UI sessions after login: SHA-256(opaque_session_cookie) -> issued_at.
    /// The cookie value is random per login (not the shared auth token) so file ownership
    /// can distinguish concurrent users who share one deployment token.
    pub browser_sessions: Arc<DashMap<String, Instant>>,
    /// Pre-canonicalized directories from which tool manifest files may be loaded.
    /// Paths are resolved at startup so handler comparisons are O(1) in-memory.
    pub allowed_tool_dirs: Arc<Vec<PathBuf>>,
    /// Persistent chat session store (separate from the task scheduler).
    pub chat_store: Arc<ChatStore>,
    /// In-memory registry of running chat inferences. Lets SSE subscribers reconnect to
    /// an inference that the server is still streaming after a browser refresh.
    pub inflight_chat: Arc<InFlightChat>,
    /// Persistent multi-agent conversation store.
    pub convo_store: Arc<ConvoStore>,
    /// In-memory registry of running multi-agent conversation orchestrators.
    pub inflight_convos: Arc<InFlightConvos>,
    /// Persistent file upload store.
    pub file_store: Arc<FileStore>,
    /// Broadcast channel for real-time notification push to browser SSE subscribers.
    /// The `SseDeliveryAdapter` in the kernel publishes to this sender.
    pub notification_tx: broadcast::Sender<NotificationSsePayload>,
    /// Whether to set the `Secure` flag on session cookies.
    /// True when the server is bound to a non-loopback address (production, behind TLS proxy).
    pub secure_cookies: bool,
}
