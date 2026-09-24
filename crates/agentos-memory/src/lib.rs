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

/// Compact an fts5 index after rows were deleted from its content table.
///
/// External-content fts5 tables do not shrink on delete: the `AFTER DELETE`
/// trigger issues the `'delete'` command, which *appends a tombstone*. Nothing
/// reclaims those segments, so a store under retention accumulates dead index
/// forever — observed on a live install as 240 live rows against 26,478 index
/// rows occupying 100 MB, which `VACUUM` cannot touch because the dead segments
/// are live rows of the fts shadow tables.
///
/// Call only when rows were actually deleted: `'optimize'` is a full segment
/// merge, and the retention sweep ticks every 10 minutes while real deletions
/// are rare.
///
/// `table` is interpolated because SQLite cannot bind an identifier. Callers
/// must pass a string literal naming a table this crate created — never a value
/// derived from input.
pub(crate) fn compact_fts_index(
    conn: &rusqlite::Connection,
    table: &str,
) -> Result<(), agentos_types::AgentOSError> {
    conn.execute(
        &format!("INSERT INTO {table}({table}) VALUES('optimize')"),
        [],
    )
    .map_err(|e| {
        agentos_types::AgentOSError::StorageError(format!(
            "Failed to compact fts index {table}: {e}"
        ))
    })?;
    Ok(())
}

pub use embedder::Embedder;
pub use episodic::{EpisodeRecordInput, EpisodicStore};
pub use procedural::{CurateReport, ProceduralStore};
pub use semantic::SemanticStore;
pub use types::{
    EpisodeType, EpisodicEntry, MemoryChunk, MemoryEntry, MemoryStatus, Procedure, ProcedureInput,
    ProcedureSearchResult, ProcedureStep, RecallQuery, RecallResult,
};
