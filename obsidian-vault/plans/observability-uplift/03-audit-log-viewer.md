---
title: "Phase 3: Audit Log Viewer"
tags:
  - web
  - audit
  - plan
date: 2026-04-03
status: planned
effort: 2d
priority: high
---

# Phase 3: Audit Log Viewer

> Add an `/audit` page to the web UI with a paginated, filterable table of audit events, full-text search via SQLite FTS5, CSV export, and HMAC chain verification status per row.

---

## Why This Phase

AgentOS has 83+ audit event types stored in a SQLite database with HMAC chain integrity. The only way to view these is `agentos audit list` in the CLI, which returns raw JSON. There is no way to:

- Filter events by task, agent, event type, or date range
- Search event details with full-text queries
- Verify the HMAC chain integrity visually
- Export events for external analysis

This phase adds a comprehensive audit viewer to the web UI, making the audit system useful for operational debugging and compliance.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Audit access | CLI only (`agentos audit list`) | Web UI at `/audit` with full table view |
| Filtering | None in CLI | Filter by task_id, agent_id, event_type, date range |
| Search | None | FTS5 full-text search on event details |
| Pagination | CLI returns all (up to limit) | 50 rows per page with next/prev navigation |
| HMAC verification | CLI can export chain | Per-row HMAC status indicator (valid/broken) |
| Export | `agentos audit export` CLI | CSV download button in UI |

## What to Do

### 1. Add FTS5 index to audit log (if not already present)

Open `crates/agentos-audit/src/log.rs`. Check if an FTS5 virtual table exists for audit events. If not, add to the schema initialization:

```rust
conn.execute_batch(
    "CREATE VIRTUAL TABLE IF NOT EXISTS audit_fts USING fts5(
        details,
        content='audit_log',
        content_rowid='rowid'
    );
    CREATE TRIGGER IF NOT EXISTS audit_fts_insert AFTER INSERT ON audit_log BEGIN
        INSERT INTO audit_fts(rowid, details) VALUES (new.rowid, new.details);
    END;",
)?;
```

### 2. Add paginated query methods to `AuditLog`

Open `crates/agentos-audit/src/log.rs`. Add:

```rust
/// Query audit entries with filtering and pagination.
pub fn query_filtered(
    &self,
    filters: &AuditQueryFilters,
    offset: u64,
    limit: u64,
) -> Result<(Vec<AuditEntry>, u64), AgentOSError> {
    let conn = self.db.lock().map_err(|e| AgentOSError::KernelError {
        reason: format!("AuditLog: lock failed: {e}"),
    })?;

    let mut where_clauses = Vec::new();
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(ref task_id) = filters.task_id {
        where_clauses.push("task_id = ?");
        params_vec.push(Box::new(task_id.to_string()));
    }
    if let Some(ref agent_id) = filters.agent_id {
        where_clauses.push("agent_id = ?");
        params_vec.push(Box::new(agent_id.to_string()));
    }
    if let Some(ref event_type) = filters.event_type {
        where_clauses.push("event_type = ?");
        params_vec.push(Box::new(format!("{:?}", event_type)));
    }
    if let Some(ref since) = filters.since {
        where_clauses.push("timestamp >= ?");
        params_vec.push(Box::new(since.to_rfc3339()));
    }
    if let Some(ref until) = filters.until {
        where_clauses.push("timestamp <= ?");
        params_vec.push(Box::new(until.to_rfc3339()));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    // Count total matching entries.
    let count_sql = format!("SELECT COUNT(*) FROM audit_log {}", where_sql);
    let total: u64 = conn.query_row(&count_sql, rusqlite::params_from_iter(&params_vec), |r| r.get(0))
        .unwrap_or(0);

    // Fetch page.
    let select_sql = format!(
        "SELECT rowid, timestamp, trace_id, event_type, agent_id, task_id, \
                tool_id, details, severity, reversible, rollback_ref \
         FROM audit_log {} ORDER BY timestamp DESC LIMIT ? OFFSET ?",
        where_sql
    );
    params_vec.push(Box::new(limit as i64));
    params_vec.push(Box::new(offset as i64));

    let mut stmt = conn.prepare(&select_sql).map_err(|e| AgentOSError::KernelError {
        reason: format!("AuditLog: prepare failed: {e}"),
    })?;

    // ... map rows to AuditEntry ...

    Ok((entries, total))
}

/// Full-text search on audit event details.
pub fn search_fts(
    &self,
    query: &str,
    limit: u64,
) -> Result<Vec<AuditEntry>, AgentOSError> {
    let conn = self.db.lock().map_err(|e| AgentOSError::KernelError {
        reason: format!("AuditLog: lock failed: {e}"),
    })?;

    let sql = "SELECT a.rowid, a.timestamp, a.trace_id, a.event_type, a.agent_id, \
               a.task_id, a.tool_id, a.details, a.severity, a.reversible, a.rollback_ref \
               FROM audit_log a \
               JOIN audit_fts f ON a.rowid = f.rowid \
               WHERE audit_fts MATCH ?1 \
               ORDER BY rank LIMIT ?2";

    let mut stmt = conn.prepare(sql).map_err(|e| AgentOSError::KernelError {
        reason: format!("AuditLog: FTS prepare failed: {e}"),
    })?;

    // ... map rows to AuditEntry ...

    Ok(entries)
}
```

### 3. Define `AuditQueryFilters`

Add to `crates/agentos-audit/src/log.rs`:

```rust
/// Filter parameters for paginated audit queries.
#[derive(Debug, Default)]
pub struct AuditQueryFilters {
    pub task_id: Option<TaskID>,
    pub agent_id: Option<AgentID>,
    pub event_type: Option<AuditEventType>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub search_text: Option<String>,
}
```

### 4. Create the audit handler

Create `crates/agentos-web/src/handlers/audit_viewer.rs`:

```rust
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse};
use crate::state::AppState;
use std::collections::HashMap;

/// Paginated, filterable audit log viewer.
pub async fn audit_view(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let page: u64 = params.get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let per_page: u64 = 50;
    let offset = (page - 1) * per_page;

    let filters = AuditQueryFilters {
        task_id: params.get("task_id").and_then(|s| s.parse().ok()),
        agent_id: params.get("agent_id").and_then(|s| s.parse().ok()),
        event_type: params.get("event_type").and_then(|s| parse_event_type(s)),
        since: params.get("since").and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()
            .map(|d| d.with_timezone(&chrono::Utc))),
        until: params.get("until").and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()
            .map(|d| d.with_timezone(&chrono::Utc))),
        search_text: params.get("q").cloned(),
    };

    let (entries, total) = if let Some(ref q) = filters.search_text {
        let results = state.kernel.audit.search_fts(q, per_page).unwrap_or_default();
        let total = results.len() as u64;
        (results, total)
    } else {
        state.kernel.audit.query_filtered(&filters, offset, per_page)
            .unwrap_or_else(|_| (vec![], 0))
    };

    let total_pages = (total + per_page - 1) / per_page;

    let ctx = minijinja::context! {
        title => "Audit Log",
        entries => entries.iter().map(|e| audit_entry_to_json(e)).collect::<Vec<_>>(),
        page => page,
        total_pages => total_pages,
        total => total,
        filters => &params,
    };

    let html = state.templates.get_template("audit_viewer.html")
        .and_then(|t| t.render(ctx).map_err(Into::into))
        .unwrap_or_else(|e| format!("<p>Template error: {}</p>", e));

    Html(html)
}

/// CSV export of audit entries matching the current filters.
pub async fn audit_export_csv(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let filters = AuditQueryFilters {
        task_id: params.get("task_id").and_then(|s| s.parse().ok()),
        agent_id: params.get("agent_id").and_then(|s| s.parse().ok()),
        // ... same filter parsing ...
        ..Default::default()
    };

    let (entries, _) = state.kernel.audit.query_filtered(&filters, 0, 10_000)
        .unwrap_or_else(|_| (vec![], 0));

    let mut csv = String::from("timestamp,trace_id,event_type,agent_id,task_id,severity,details\n");
    for entry in &entries {
        csv.push_str(&format!(
            "{},{},{:?},{},{},{:?},{}\n",
            entry.timestamp.to_rfc3339(),
            entry.trace_id,
            entry.event_type,
            entry.agent_id.map(|id| id.to_string()).unwrap_or_default(),
            entry.task_id.map(|id| id.to_string()).unwrap_or_default(),
            entry.severity,
            entry.details.to_string().replace(',', ";"), // escape commas
        ));
    }

    (
        [
            (axum::http::header::CONTENT_TYPE, "text/csv"),
            (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"audit_log.csv\""),
        ],
        csv,
    )
}
```

### 5. Create the audit template

Create `crates/agentos-web/templates/audit_viewer.html`:

A table with columns: Timestamp, Trace ID, Event Type, Agent, Task, Severity, Details (truncated, expandable). Filter controls at the top (dropdowns for event type, text inputs for IDs, date pickers). Pagination at the bottom. CSV export button.

### 6. Register routes

Open `crates/agentos-web/src/router.rs`. Add:

```rust
.route("/audit", axum::routing::get(audit_viewer::audit_view))
.route("/audit/export", axum::routing::get(audit_viewer::audit_export_csv))
```

Open `crates/agentos-web/src/handlers/mod.rs`. Add:

```rust
pub mod audit_viewer;
```

Note: There is already `pub mod audit;` -- the new module is `audit_viewer` to avoid conflicts with the existing audit handler module.

### 7. Add HMAC chain verification indicator

In the query results, check each entry's HMAC against the chain. The `AuditLog` already has chain verification methods. Add a `hmac_valid: bool` field to the template context for each row.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-audit/src/log.rs` | Add FTS5 index, `query_filtered()`, `search_fts()`, `AuditQueryFilters` |
| `crates/agentos-web/src/handlers/audit_viewer.rs` | New file: `audit_view`, `audit_export_csv` handlers |
| `crates/agentos-web/src/handlers/mod.rs` | Add `pub mod audit_viewer;` |
| `crates/agentos-web/src/router.rs` | Add `/audit` and `/audit/export` routes |
| `crates/agentos-web/templates/audit_viewer.html` | New template: paginated audit table with filters |

## Prerequisites

None -- this phase is independent of Phases 1 and 2 and can be developed in parallel.

## Test Plan

- **Unit test `test_query_filtered_by_task_id`:** Insert 10 audit entries for 2 different task IDs. Query with `task_id` filter. Assert only entries for the filtered task are returned.
- **Unit test `test_query_filtered_by_date_range`:** Insert entries at different timestamps. Query with `since` and `until`. Assert only entries in range are returned.
- **Unit test `test_fts_search`:** Insert an entry with details containing "memory consolidation failed". Search for "consolidation". Assert the entry is found.
- **Unit test `test_pagination`:** Insert 120 entries. Query page 1 (limit 50). Assert 50 entries returned and total == 120. Query page 3. Assert 20 entries returned.
- **Unit test `test_csv_export_format`:** Generate CSV from 5 entries. Assert the output starts with the header row and contains 5 data rows.
- **Unit test `test_empty_filters_returns_all`:** Query with no filters. Assert all entries returned (up to limit).

## Verification

```bash
cargo build -p agentos-audit -p agentos-web
cargo test -p agentos-audit -- --nocapture
cargo test -p agentos-web -- --nocapture
cargo clippy -p agentos-audit -p agentos-web -- -D warnings
cargo fmt --all -- --check
```
