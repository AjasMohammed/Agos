use agentos_kernel::Kernel;
use dashmap::DashMap;
use minijinja::Environment;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Kernel>,
    pub templates: Arc<Environment<'static>>,
    /// Per-session CSRF tokens: SHA-256(session_cookie) -> (csrf_token, issued_at).
    ///
    /// - Key is the SHA-256 hash of the raw session cookie value so the auth credential is
    ///   never stored as a plain String in this map.
    /// - Value is (64-char hex CSRF token, Instant it was issued).
    /// - Tokens are regenerated after 8 h (matching the session cookie max-age).
    pub csrf_tokens: Arc<DashMap<String, (String, Instant)>>,
    /// Pre-canonicalized directories from which tool manifest files may be loaded.
    /// Paths are resolved at startup so handler comparisons are O(1) in-memory.
    pub allowed_tool_dirs: Arc<Vec<PathBuf>>,
}
