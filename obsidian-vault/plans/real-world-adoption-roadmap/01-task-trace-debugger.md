---
title: Task Trace Debugger
tags:
  - kernel
  - web-ui
  - observability
  - plan
  - v3
date: 2026-03-25
status: complete
effort: 4d
priority: critical
---

# Phase 1 — Task Trace Debugger

> Build a task execution timeline viewer with time-travel debug capability, solving the #1 production pain point in the 2025-2026 agentic ecosystem: developers cannot determine which step of a multi-step agent task failed or why.

---

## Why This Phase

Ecosystem research (NotebookLM, 2026-03-25) identifies **"Attribution Difficulty"** as the leading cause of agent framework abandonment in production:

> "Traditional debugging fails in multi-agent systems. Practitioners face 'Attribution Difficulty,' where it is nearly impossible to pinpoint which specific agent or step caused a failure because stack traces are unhelpful and logs are scattered across different actors."

AgentOS already has two pieces of the solution:
1. An append-only audit log with 83+ event types (`agentos-audit`)
2. A snapshot system with `agentctl snapshot create/restore` (`SnapshotManager` in kernel)

What's missing is a **UI** that joins these two systems and lets developers interactively inspect, rewind, and replay task execution.

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Task audit events | Written to SQLite audit log | Exposed via structured trace API |
| Snapshots | CLI-only: `agentctl snapshot create/restore` | Linked to trace events; accessible from web UI |
| Task debug | None | Timeline viewer: each LLM iteration → tool calls → results as a tree |
| Time-travel | None | "Rewind to step N" button: restores snapshot, re-opens context for manual inspection |
| Tool failure detail | Error string in audit log | Full tool input, output, permission check result, and stack in trace view |
| Context at failure | Inaccessible | Inline context window viewer showing exact messages sent to LLM at each iteration |

---

## Detailed Subtasks

### Subtask 1.1 — Add `TaskTrace` type and trace event emission (agentos-kernel)

**File:** `crates/agentos-kernel/src/task_trace.rs` (new)

Create a `TaskTrace` struct that aggregates audit events for a single task into a structured tree:

```rust
use serde::{Deserialize, Serialize};
use crate::types::{TaskID, AgentID};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTrace {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: String,           // Pending / Running / Completed / Failed
    pub iterations: Vec<IterationTrace>,
    pub snapshot_ids: Vec<String>, // snapshots taken during this task
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationTrace {
    pub iteration: u32,
    pub started_at: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub stop_reason: String,      // tool_use / end_turn / max_tokens
    pub tool_calls: Vec<ToolCallTrace>,
    pub context_snapshot: Option<ContextSnapshot>, // messages sent to LLM
    pub snapshot_id: Option<String>,               // kernel snapshot at this point
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallTrace {
    pub tool_name: String,
    pub input_json: serde_json::Value,
    pub output_json: Option<serde_json::Value>,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub permission_check: PermissionCheckTrace,
    pub injection_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionCheckTrace {
    pub granted: bool,
    pub required_scope: String,
    pub held_scopes: Vec<String>,
    pub deny_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub messages: Vec<ContextMessage>,
    pub total_tokens: u64,
    pub overflow_strategy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMessage {
    pub role: String,   // system / user / assistant / tool
    pub content: String,
    pub tokens: u32,
}
```

**File:** `crates/agentos-kernel/src/task_executor.rs`

In the main execution loop, emit trace events:
- After each LLM inference call: record `IterationTrace` with input/output tokens, model, stop reason
- After each tool call: record `ToolCallTrace` with input, output, duration, permission result
- Before each LLM call: capture the context window state into `ContextSnapshot`

Wire these into the `TraceCollector` (see 1.2) rather than writing directly to audit log.

**File:** `crates/agentos-kernel/src/lib.rs`

Add `pub mod task_trace;` to module exports.

---

### Subtask 1.2 — TraceCollector: in-memory accumulator + SQLite persistence

**File:** `crates/agentos-kernel/src/trace_collector.rs` (new)

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::task_trace::TaskTrace;
use crate::types::TaskID;

pub struct TraceCollector {
    active_traces: Arc<RwLock<HashMap<TaskID, TaskTrace>>>,
    db: Arc<TraceDatabase>,
}

impl TraceCollector {
    pub async fn start_trace(&self, task_id: TaskID, agent_id: AgentID, ...) { ... }
    pub async fn record_iteration(&self, task_id: &TaskID, iter: IterationTrace) { ... }
    pub async fn finish_trace(&self, task_id: &TaskID, status: &str) { ... }

    /// Query completed traces from SQLite
    pub async fn get_trace(&self, task_id: &TaskID) -> Result<Option<TaskTrace>>;
    pub async fn list_traces(&self, agent_id: Option<&AgentID>, limit: u32) -> Result<Vec<TaskTraceSummary>>;
}

struct TraceDatabase {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl TraceDatabase {
    fn new(path: &Path) -> Result<Self> {
        // Schema: traces(task_id TEXT PK, agent_id TEXT, started_at TEXT,
        //                finished_at TEXT, status TEXT, trace_json TEXT)
        // Index on agent_id, started_at
    }
    fn upsert_trace(&self, trace: &TaskTrace) -> Result<()> { ... }
    fn get_trace(&self, task_id: &str) -> Result<Option<TaskTrace>> { ... }
    fn list_recent(&self, agent_id: Option<&str>, limit: u32) -> Result<Vec<TaskTraceSummary>> { ... }
}
```

Store complete `TaskTrace` as JSON blob in `traces` table (same pattern as audit log). Persist to `{data_dir}/traces.db`.

Add `trace_collector: Arc<TraceCollector>` to `KernelContext` (`crates/agentos-kernel/src/context.rs`).

---

### Subtask 1.3 — Kernel command: `TaskTrace` and `ListTraces`

**File:** `crates/agentos-kernel/src/commands/task.rs`

Add two new command handlers:
```rust
KernelCommand::TaskGetTrace { task_id } => {
    let trace = ctx.trace_collector.get_trace(&task_id).await?;
    respond(KernelResponse::TaskTrace(trace))
}
KernelCommand::TaskListTraces { agent_id, limit } => {
    let summaries = ctx.trace_collector.list_traces(agent_id.as_ref(), limit).await?;
    respond(KernelResponse::TaskTraces(summaries))
}
```

**File:** `crates/agentos-bus/src/message.rs`

Add to `KernelCommand`:
```rust
TaskGetTrace { task_id: TaskID },
TaskListTraces { agent_id: Option<AgentID>, limit: u32 },
```

Add to `KernelResponse`:
```rust
TaskTrace(Option<TaskTrace>),
TaskTraces(Vec<TaskTraceSummary>),
```

---

### Subtask 1.4 — CLI command: `agentctl task trace <task-id>`

**File:** `crates/agentos-cli/src/commands/task.rs`

Add `trace` subcommand:
```bash
agentctl task trace <TASK_ID>           # Print full trace as formatted table
agentctl task trace <TASK_ID> --json    # Dump raw JSON
agentctl task trace <TASK_ID> --iter 3  # Show only iteration 3
```

Output format (text):
```
Task abc123 — FAILED (3 iterations, 2,451 tokens, $0.0031)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Iter 1  [claude-sonnet-4-6]  847 in / 312 out  → tool_use
  └── file-reader  /home/user/data.csv  → OK  (12ms)
  └── memory-write "user preferences"   → OK  (3ms)

Iter 2  [claude-sonnet-4-6]  1,159 in / 198 out → tool_use
  └── shell-exec  "rm -rf /tmp/work"    → DENIED  [no execute perm]  ← FAILURE

Iter 3  [claude-sonnet-4-6]  1,204 in / 87 out  → end_turn
  └── (no tool calls)
```

---

### Subtask 1.5 — Web API: GET /api/tasks/:id/trace

**File:** `crates/agentos-web/src/handlers/task.rs`

Add handler:
```rust
pub async fn get_task_trace(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskTrace>, AppError> {
    let trace = state.kernel_client.get_task_trace(&task_id).await?;
    Ok(Json(trace.ok_or(AppError::NotFound)?))
}
```

Add route in `crates/agentos-web/src/router.rs`:
```rust
.route("/api/tasks/:id/trace", get(handlers::task::get_task_trace))
```

---

### Subtask 1.6 — Web UI: Task timeline view

**File:** `crates/agentos-web/src/templates/tasks/trace.html` (new)

Build HTMX-powered timeline page:
- Task header: status badge, total tokens, total cost, duration
- Iteration accordion: click to expand each iteration
- Per-iteration: model name, token counts, stop reason, list of tool calls
- Per-tool-call: name, input/output (truncated), duration, permission check result (green check or red X)
- "Context at this point" expandable section: shows messages sent to LLM (truncated to 500 chars each)
- "Rewind to here" button: calls `/api/tasks/:id/trace/rewind?iter=N`

**File:** `crates/agentos-web/src/handlers/task.rs`

Add `rewind_to_iteration` handler:
```rust
// POST /api/tasks/:id/trace/rewind?iter=N
// - Looks up snapshot_id for iteration N from trace
// - Calls kernel SnapshotRestore command
// - Returns JSON with restored context summary
```

**File:** `crates/agentos-web/src/router.rs`

```rust
.route("/tasks/:id/trace", get(handlers::task::task_trace_page))
.route("/api/tasks/:id/trace", get(handlers::task::get_task_trace))
.route("/api/tasks/:id/trace/rewind", post(handlers::task::rewind_to_iteration))
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_trace.rs` | New — TaskTrace, IterationTrace, ToolCallTrace types |
| `crates/agentos-kernel/src/trace_collector.rs` | New — TraceCollector with SQLite persistence |
| `crates/agentos-kernel/src/task_executor.rs` | Modified — emit traces at each iteration and tool call |
| `crates/agentos-kernel/src/context.rs` | Modified — add `trace_collector: Arc<TraceCollector>` |
| `crates/agentos-kernel/src/commands/task.rs` | Modified — add TaskGetTrace and TaskListTraces handlers |
| `crates/agentos-kernel/src/lib.rs` | Modified — add pub mod for new modules |
| `crates/agentos-bus/src/message.rs` | Modified — add TaskGetTrace/TaskListTraces commands and responses |
| `crates/agentos-cli/src/commands/task.rs` | Modified — add `trace` subcommand |
| `crates/agentos-web/src/handlers/task.rs` | Modified — add trace API handlers |
| `crates/agentos-web/src/templates/tasks/trace.html` | New — timeline UI template |
| `crates/agentos-web/src/router.rs` | Modified — add trace routes |

---

## Dependencies

- No other phases required
- Requires kernel snapshot system to be functional (already complete)
- Requires audit log to be functional (already complete)

---

## Test Plan

1. **Unit: TraceCollector persistence**
   - Start a mock task, emit 3 iterations with 2 tool calls each
   - Call `finish_trace`, then `get_trace`
   - Assert: all 3 iterations present, all 6 tool calls present, token counts match

2. **Unit: PermissionCheckTrace on denied tool**
   - Execute a tool call that fails permission check
   - Assert: `ToolCallTrace.permission_check.granted = false`, `deny_reason` is non-empty

3. **Integration: CLI `agentctl task trace <id>`**
   - Run a task with a deliberate tool failure
   - Run `agentctl task trace <id>`
   - Assert: output contains the failing tool name and `DENIED` label

4. **Integration: Web API GET /api/tasks/:id/trace**
   - Run a task, query trace endpoint
   - Assert: JSON contains `iterations` array, each with `tool_calls`

5. **Integration: Time-travel rewind**
   - Run a 3-iteration task with snapshots enabled
   - Call rewind to iteration 2
   - Assert: kernel returns `SnapshotRestored` event, context summary matches iteration 2

---

## Verification

```bash
# Build and test
cargo build -p agentos-kernel -p agentos-cli -p agentos-web
cargo test -p agentos-kernel -- trace
cargo test -p agentos-web -- trace

# Manual: run a task and check trace
agentctl task run --agent myagent "List files in /tmp"
agentctl task list   # get task ID
agentctl task trace <TASK_ID>
agentctl task trace <TASK_ID> --json | jq '.iterations | length'

# Web: verify trace page loads
curl http://localhost:8080/tasks/<TASK_ID>/trace
```

---

## Related

- [[Real World Adoption Roadmap Plan]] — parent plan
- [[02-web-ui-completion]] — uses trace viewer built here
- [[06-opentelemetry-export]] — exports these trace structures as OTLP spans
