---
title: "Phase 07 -- SSE Stream Fix and Pipeline Error Handling"
tags:
  - webui
  - correctness
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 5h
priority: high
---

# Phase 07 -- SSE Stream Fix and Pipeline Error Handling

> Fix the SSE live log stream to use ID-based tracking instead of count-based tracking (preventing event loss and freezes), and improve the pipeline `run` handler's error handling.

---

## Why This Phase

Two correctness bugs: (I1) The SSE stream in `tasks.rs:91-138` uses `query_recent(50)` and tracks events by count (`last_count`). When there are more than 50 relevant entries total, the count wraps around and the stream either re-emits old events or freezes permanently. When entries are deleted, the count drops and the stream re-emits previously sent events. (I2) The pipeline `run` handler in `pipelines.rs:52-66` calls `state.kernel.run_pipeline()` which properly delegates to `cmd_run_pipeline`, but the error path returns a raw `String` without context, and the handler does not validate required fields before calling the kernel.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| SSE tracking (`tasks.rs:99`) | `stream::unfold(0u32, ...)` -- tracks by `last_count` (count of relevant entries seen so far) | `stream::unfold(0i64, ...)` -- tracks by `last_seen_id` (maximum audit entry SQLite row ID seen) |
| SSE query method | `audit.query_recent(50)` -- returns 50 most recent entries across ALL tasks, then filters by task_id in the handler | New method `audit.query_since_for_task(task_id, after_id, limit)` -- returns entries for a specific task with rowid > after_id |
| Event loss | When >50 total entries exist, earlier task-specific entries fall off the `query_recent(50)` window; if new `count < last_count`, stream freezes | ID-based tracking never loses events; each poll returns only new entries since `last_seen_id` |
| Freeze on delete | If audit entries are deleted, `relevant.len()` drops below `last_count` and `count > last_count` never triggers | Row IDs always increase monotonically; deletion does not affect delivery of new events |
| SSE task_id validation | No validation -- invalid UUID string causes filter to silently match nothing | Parse task_id before creating stream; return error SSE event for invalid UUIDs |
| Pipeline run errors | `pipelines.rs:64` returns `(StatusCode::BAD_REQUEST, e)` where `e` is a raw `String` from `kernel.run_pipeline()` | Return structured error with `format!("Pipeline execution failed: {}", message)` |
| Pipeline form validation | Handler calls `kernel.run_pipeline()` without checking if `agent_name` is provided; kernel returns a generic error | Handler validates required fields before calling kernel; returns descriptive 400 error |

---

## Subtasks

### 1. Add ID-based audit query method

**File:** `crates/agentos-audit/src/log.rs`

The existing `query_recent(limit: u32)` method (line 468) returns the N most recent entries with no task filtering and no row ID tracking. Add a new method that returns entries for a specific task with a row ID greater than a given value:

```rust
/// Query audit entries for a specific task inserted after the given row ID.
/// Returns entries ordered by row ID ascending (oldest first), limited to `limit` rows.
/// Each entry is paired with its SQLite row ID for monotonic tracking.
pub fn query_since_for_task(
    &self,
    task_id: &TaskID,
    after_row_id: i64,
    limit: u32,
) -> Result<Vec<(i64, AuditEntry)>, AgentOSError> {
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    let mut stmt = conn
        .prepare(
            "SELECT rowid, timestamp, trace_id, event_type, agent_id, task_id, \
             tool_id, details, severity, reversible, rollback_ref \
             FROM audit_log \
             WHERE task_id = ?1 AND rowid > ?2 \
             ORDER BY rowid ASC \
             LIMIT ?3",
        )
        .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

    let task_id_str = task_id.to_string();
    let rows = stmt
        .query_map(
            rusqlite::params![task_id_str, after_row_id, limit],
            |row| {
                let rowid: i64 = row.get(0)?;
                // Parse AuditEntry from columns 1-10 using the same logic as query_recent
                let entry = Self::parse_audit_row_offset(row, 1)?;
                Ok((rowid, entry))
            },
        )
        .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentOSError::VaultError(e.to_string()))
}
```

**Note:** The existing `query_recent` method (line 468) has row-to-`AuditEntry` parsing logic inline. To avoid duplication, extract the row parsing into a helper function:

```rust
/// Parse an AuditEntry from a rusqlite Row, reading columns starting at `offset`.
fn parse_audit_row_offset(row: &rusqlite::Row, offset: usize) -> rusqlite::Result<AuditEntry> {
    // timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity, reversible, rollback_ref
    // These are the 10 columns selected in both query_recent and query_since_for_task
    // ... same parsing logic as the existing closure in query_recent ...
}
```

Then update `query_recent` to use `Self::parse_audit_row_offset(row, 0)` and the new method to use `Self::parse_audit_row_offset(row, 1)` (offset by 1 because column 0 is `rowid`).

### 2. Rewrite SSE stream to use ID-based tracking

**File:** `crates/agentos-web/src/handlers/tasks.rs`

Replace the `log_stream` function (lines 91-138):

```rust
/// SSE endpoint for live task log streaming.
/// Streams audit events related to the given task using monotonic ID-based tracking.
pub async fn log_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    // Parse task ID upfront for early error handling
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => {
            // Return a stream with a single error event
            let stream = stream::once(async {
                Ok(Event::default().data("Error: invalid task ID"))
            });
            return Sse::new(stream).keep_alive(KeepAlive::default());
        }
    };

    let audit = state.kernel.audit.clone();

    // Poll audit log every second, tracking by monotonic row ID.
    let stream = stream::unfold(0i64, move |last_seen_id| {
        let audit = audit.clone();
        let task_id = task_id.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(1)).await;

            match audit.query_since_for_task(&task_id, last_seen_id, 100) {
                Ok(entries) if !entries.is_empty() => {
                    let max_id = entries.last().map(|(id, _)| *id).unwrap_or(last_seen_id);
                    let data: Vec<String> = entries
                        .iter()
                        .map(|(_, e)| {
                            format!(
                                "[{}] {:?} - {}",
                                e.timestamp.format("%H:%M:%S"),
                                e.event_type,
                                e.details
                            )
                        })
                        .collect();
                    let event_data = data.join("\n");
                    Some((
                        Ok(Event::default()
                            .data(event_data)
                            .id(max_id.to_string())),
                        max_id,
                    ))
                }
                Ok(_) => {
                    // No new entries -- send keepalive comment
                    Some((Ok(Event::default().comment("keepalive")), last_seen_id))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "SSE audit query error");
                    Some((Ok(Event::default().comment("query error")), last_seen_id))
                }
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

Key changes from current implementation:
- `unfold(0i64, ...)` instead of `unfold(0u32, ...)`
- Uses `query_since_for_task(task_id, last_seen_id, 100)` instead of `query_recent(50)` + filter
- Tracks `max_id` (row ID) instead of `count` (number of entries)
- Sets SSE event `id` field for client reconnection support (browser can send `Last-Event-ID` on reconnect)
- Parses task_id before creating the stream
- Error handling for query failures (logs warning and continues)

### 3. Improve pipeline run handler error handling

**File:** `crates/agentos-web/src/handlers/pipelines.rs`

The current handler (lines 52-66) calls `state.kernel.run_pipeline()` which returns `Result<serde_json::Value, String>`. The error handling returns the raw string as the response body. Improve with better error messages and validate the agent_name field:

```rust
pub async fn run(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<RunForm>,
) -> Response {
    // Pipeline execution requires an agent name for permission enforcement.
    // The kernel will return a generic error if agent_name is None,
    // but we can provide a better user-facing error here.
    if form.agent_name.as_ref().map(|n| n.trim().is_empty()).unwrap_or(true) {
        return (
            StatusCode::BAD_REQUEST,
            "Pipeline execution requires an agent name. Specify the governing agent in the 'Agent Name' field.",
        )
            .into_response();
    }

    match state
        .kernel
        .run_pipeline(form.pipeline_name.clone(), form.input.clone(), true, form.agent_name.clone())
        .await
    {
        Ok(data) => {
            if let Some(run_id) = data.get("id").and_then(|v| v.as_str()) {
                tracing::info!(run_id = %run_id, pipeline = %form.pipeline_name, "Pipeline started from web UI");
            }
            axum::response::Redirect::to("/pipelines").into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Pipeline execution failed: {}", e),
        )
            .into_response(),
    }
}
```

The `RunForm` struct already has `agent_name: Option<String>` in the current code (line 49). No change needed to the struct.

### 4. Update pipeline form template

**File:** `crates/agentos-web/src/templates/pipelines.html`

Ensure the pipeline run form includes an `agent_name` field. If the current template does not have it, add:

```html
<label for="agent_name">Agent Name (required)</label>
<input name="agent_name" id="agent_name" required
       placeholder="Name of the agent to execute the pipeline">
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-audit/src/log.rs` | Add `query_since_for_task(task_id, after_row_id, limit)` method; extract row parsing helper |
| `crates/agentos-web/src/handlers/tasks.rs` | Rewrite `log_stream` to use ID-based tracking via `query_since_for_task` |
| `crates/agentos-web/src/handlers/pipelines.rs` | Add agent_name validation; improve error message formatting |
| `crates/agentos-web/src/templates/pipelines.html` | Add `agent_name` input field to run form (if missing) |

---

## Dependencies

None -- this phase can be done independently. The `query_since_for_task` change in `agentos-audit` is self-contained and does not affect other audit consumers.

---

## Test Plan

### SSE Stream Tests

1. **ID-based tracking basic test:** Create 5 audit entries for a task. Connect to the SSE stream. Verify all 5 entries are emitted. Create 3 more entries. Verify only the 3 new entries are emitted on the next poll (not the original 5).

2. **No event loss with >50 entries:** Create 100 audit entries for a task. Connect to the SSE stream. Verify all 100 entries are eventually emitted across multiple 1-second poll cycles (the method returns up to 100 per call, so this should complete in 1 cycle).

3. **Deletion resilience:** Create 10 entries for a task. Connect to SSE (entries emitted, `last_seen_id = 10`). Delete entries 1-5 from the audit DB. Create 5 more entries (IDs 11-15). Verify the stream emits only entries 11-15 (not re-emitting old ones or freezing).

4. **Invalid task ID:** Connect to `/tasks/not-a-uuid/logs/stream`. Verify the stream emits a single event with data "Error: invalid task ID" and does not panic.

5. **SSE event ID field:** Verify each emitted SSE event has an `id` field containing the max row ID (enables client reconnection via `Last-Event-ID` header).

### Pipeline Error Handling Tests

6. **Missing agent name:** POST `/pipelines/run` with `agent_name` empty or absent. Verify 400 response with message mentioning "agent name".

7. **Pipeline execution success:** POST `/pipelines/run` with valid pipeline and agent. Verify 302 redirect to `/pipelines`.

8. **Non-existent pipeline:** POST `/pipelines/run` with `pipeline_name=nonexistent`. Verify 400 error with descriptive message.

### Audit Method Unit Tests

9. **`query_since_for_task` unit test:** Insert 10 audit entries for task A and 5 for task B. Call `query_since_for_task(task_A, 0, 100)`. Verify exactly 10 entries returned. Call again with `after_row_id = max_id` from first call. Verify 0 entries returned. Insert 2 more for task A. Call again. Verify 2 entries returned.

10. **Row ID monotonicity:** Insert entries, call `query_since_for_task`, verify row IDs are strictly increasing in the returned results.

---

## Verification

```bash
# Must compile
cargo build -p agentos-audit -p agentos-web

# All tests pass
cargo test -p agentos-audit -p agentos-web

# Verify new audit method exists
grep -n "query_since_for_task" crates/agentos-audit/src/log.rs
# Expected: at least 1 match

# Verify SSE uses ID-based tracking
grep -n "last_seen_id" crates/agentos-web/src/handlers/tasks.rs
# Expected: multiple matches in the unfold closure

# Verify old count-based tracking is removed
grep -c "last_count" crates/agentos-web/src/handlers/tasks.rs
# Expected: 0

# Verify pipeline handler validates agent_name
grep -n "agent_name" crates/agentos-web/src/handlers/pipelines.rs
```

---

## Related

- [[WebUI Security Fixes Plan]] -- Master plan
- [[WebUI Security Fixes Data Flow]] -- SSE stream before/after diagram
