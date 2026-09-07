//! Opt-in resume support for the `claude_code` adapter.
//!
//! The `claude` CLI can resume a prior conversation with `--resume <session_id>`,
//! sending only the new turn instead of replaying the full flattened
//! `ContextWindow`. To stay decoupled from the kernel, the adapter depends only
//! on this small [`ClaudeSessionLookup`] trait; the kernel provides the concrete
//! implementation wrapping its SQLite-backed `ClaudeSessionStore`.
//!
//! The session is keyed by the context's stable `resume_key` (set by the kernel
//! to the TaskID — NOT the ephemeral compiled `ContextID`, which is regenerated
//! every turn), and is treated strictly as a **cache**: it is invalidated on
//! context compaction and deleted on task completion, so the kernel never cedes
//! authority over conversation state. A delta turn is only sent when a prefix
//! fingerprint confirms the CLI session still matches the current context;
//! otherwise the adapter falls back to a full send + fresh session.

use async_trait::async_trait;

/// A resolved resume entry for a context window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    /// The `claude` CLI session id to pass to `--resume`.
    pub session_id: String,
    /// High-water mark: how many active context entries had already been sent
    /// to the CLI the last time we recorded a session. Used to compute the
    /// delta turn to send on resume.
    pub last_sent_entry_count: usize,
    /// Fingerprint of `(system_prompt + the prefix entries already sent)` at the
    /// time the session was recorded. On the next turn the adapter recomputes
    /// this over the current prefix; the delta is only sent on an exact match,
    /// otherwise it falls back to a full send + fresh session. This makes resume
    /// safe against context recompilation (compaction, reordering, eviction,
    /// system-prompt changes), not just monotonic appends.
    pub fingerprint: u64,
}

/// Per-call resume resolution hook used by `ClaudeCodeCore`.
///
/// All methods are best-effort from the adapter's perspective: a `None` lookup
/// simply means "send the full context and start a fresh session", and
/// `record`/`invalidate` failures are swallowed by the implementation (the
/// session is a cache, never a source of truth).
#[async_trait]
pub trait ClaudeSessionLookup: Send + Sync {
    /// Look up the stored session for a context window, if any.
    async fn lookup(&self, context_id: &str) -> Option<SessionState>;

    /// Record (UPSERT) the session id, the number of entries sent so far, and
    /// the prefix fingerprint of what the CLI session now holds.
    async fn record(
        &self,
        context_id: &str,
        session_id: &str,
        sent_entry_count: usize,
        fingerprint: u64,
    );

    /// Invalidate (delete) the stored session for a context window.
    async fn invalidate(&self, context_id: &str);
}
