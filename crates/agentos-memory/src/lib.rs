pub mod embedder;
pub mod episodic;
mod lifecycle;
pub mod procedural;
pub mod semantic;
pub mod types;

/// Restrict a freshly-created SQLite DB file to owner read/write (`0600`).
///
/// Memory DBs hold raw tool-result and agent-reasoning content in plaintext;
/// without this they inherit the process umask (often world-readable `0644`),
/// exposing that content to other local users. Best-effort: logged, not fatal.
#[cfg(unix)]
pub(crate) fn restrict_db_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        eprintln!(
            "warning: failed to set 0600 on memory DB {}: {e}",
            path.display()
        );
    }
}

#[cfg(not(unix))]
pub(crate) fn restrict_db_permissions(_path: &std::path::Path) {}

pub use embedder::Embedder;
pub use episodic::{EpisodeRecordInput, EpisodicStore};
pub use procedural::ProceduralStore;
pub use semantic::SemanticStore;
pub use types::{
    EpisodeType, EpisodicEntry, MemoryChunk, MemoryEntry, MemoryStatus, Procedure,
    ProcedureSearchResult, ProcedureStep, RecallQuery, RecallResult,
};
