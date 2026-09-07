//! Shared lifecycle-column migration for the memory stores.
//!
//! Adds `last_used_at`, `use_count`, `confidence`, and `status` to a memory
//! table when missing, so pre-lifecycle databases upgrade in place on open.

use crate::types::MemoryStatus;
use agentos_types::AgentOSError;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::HashSet;

/// Columns added by the memory-lifecycle migration, with their DDL types.
const LIFECYCLE_COLUMNS: &[(&str, &str)] = &[
    ("last_used_at", "TEXT"),
    ("use_count", "INTEGER NOT NULL DEFAULT 0"),
    ("confidence", "REAL NOT NULL DEFAULT 0.6"),
    ("status", "TEXT NOT NULL DEFAULT 'active'"),
];

/// Add any missing lifecycle columns to `table` and index its `status` column.
/// Idempotent: existing columns are left untouched. `table` is always a
/// compile-time constant supplied by the stores, never user input.
pub(crate) fn migrate_lifecycle_columns(
    conn: &Connection,
    table: &str,
) -> Result<(), AgentOSError> {
    let existing: HashSet<String> = conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .map_err(|e| AgentOSError::StorageError(format!("Lifecycle migration probe: {e}")))?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| AgentOSError::StorageError(format!("Lifecycle migration probe: {e}")))?
        .collect::<Result<_, _>>()
        .map_err(|e| AgentOSError::StorageError(format!("Lifecycle migration probe: {e}")))?;

    for (col, ddl) in LIFECYCLE_COLUMNS {
        if !existing.contains(*col) {
            conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {ddl}"), [])
                .map_err(|e| {
                    AgentOSError::StorageError(format!(
                        "Lifecycle migration: failed to add {table}.{col}: {e}"
                    ))
                })?;
        }
    }

    conn.execute(
        &format!("CREATE INDEX IF NOT EXISTS idx_{table}_status ON {table}(status)"),
        [],
    )
    .map_err(|e| {
        AgentOSError::StorageError(format!("Lifecycle migration: status index on {table}: {e}"))
    })?;

    Ok(())
}

/// Read the four lifecycle columns starting at column index `idx`
/// (order: last_used_at, use_count, confidence, status).
pub(crate) fn lifecycle_from_row(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<(Option<DateTime<Utc>>, u32, f32, MemoryStatus)> {
    let last_used: Option<String> = row.get(idx)?;
    let use_count: u32 = row.get(idx + 1)?;
    let confidence: f64 = row.get(idx + 2)?;
    let status: String = row.get(idx + 3)?;
    Ok((
        last_used.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|t| t.with_timezone(&Utc))
        }),
        use_count,
        confidence as f32,
        MemoryStatus::parse(&status),
    ))
}
