//! Pluggable sink for persisting inbound channel media (Telegram photos,
//! documents, voice notes, …).
//!
//! The kernel downloads media bytes (it has the bot token via the vault) and
//! hands them to an `AttachmentSink`, which persists them and returns an opaque
//! file id the agent can later resolve. The `agentos-web` server backs this
//! with its `FileStore`; CLI/test builds use [`NoopAttachmentSink`], which
//! declines — media then surfaces to the agent as a text note only.
//!
//! This mirrors the `agentos_llm::ImageResolver` injection pattern: the kernel
//! holds the sink behind a lock and the web layer swaps in a real impl at boot.

use async_trait::async_trait;

/// Persists downloaded inbound media and returns a stable file id.
#[async_trait]
pub trait AttachmentSink: Send + Sync {
    /// Persist `bytes` (already size- and type-validated by the caller).
    ///
    /// `original_name` is a display/download name; `mime` is the detected
    /// content type. Returns a stable opaque file id, or an error string.
    async fn store(
        &self,
        original_name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<String, String>;
}

/// Default sink for builds without a file store (CLI, tests). Declines to store
/// so callers fall back to a text-only media note.
#[derive(Debug, Default)]
pub struct NoopAttachmentSink;

#[async_trait]
impl AttachmentSink for NoopAttachmentSink {
    async fn store(
        &self,
        _original_name: &str,
        _mime: &str,
        _bytes: Vec<u8>,
    ) -> Result<String, String> {
        Err("attachment sink not configured".to_string())
    }
}
