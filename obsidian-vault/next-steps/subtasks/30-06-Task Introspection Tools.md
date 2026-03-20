---
title: 30-06 Task Introspection Tools
tags:
  - tools
  - coordination
  - tasks
  - next-steps
  - subtask
date: 2026-03-18
status: planned
effort: 6h
priority: high
---

# 30-06 — Task Introspection Tools

> Add `task-status` and `task-list` so agents can check on tasks they've delegated and see their own task queue. Requires defining `TaskQuery` trait (similar to subtask 30-05's `AgentRegistryQuery`) and implementing it in the kernel scheduler.

---

## Why This Phase

After `task-delegate`, an agent has no way to know if the sub-task completed, failed, or is still running. This breaks the coordination loop: the delegating agent cannot react to outcomes. `task-status` and `task-list` close this gap.

---

## Current → Target State

| Capability | Current | Target |
|-----------|---------|--------|
| Check delegated task status | none | `task-status` tool |
| List agent's tasks | none | `task-list` tool |
| `TaskQuery` trait | does not exist | defined in `agentos-types`, implemented by kernel scheduler |

---

## Prerequisites

Subtask [[30-05-Agent Discovery Tool]] must be completed first — it adds `agent_registry` and `task_registry` fields to `ToolExecutionContext`. The `task_registry: None` stub placed in subtask 30-05 is populated here.

---

## What to Do

### Step 1 — Define `TaskQuery` in `agentos-types`

Add to `crates/agentos-types/src/registry_query.rs` (created in subtask 30-05):

```rust
/// Lightweight task summary returned by task introspection tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: TaskID,
    pub description: String,
    pub status: String,                     // "pending" | "running" | "completed" | "failed" | "cancelled"
    pub assigned_agent: Option<String>,     // agent name, if assigned
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result_preview: Option<String>,     // first 200 chars of result, if completed
    pub error: Option<String>,              // error message, if failed
}

/// Thin query interface for the task store / scheduler.
pub trait TaskQuery: Send + Sync {
    /// Return a single task by ID, or None if not found.
    fn get_task(&self, id: &TaskID) -> Option<TaskSummary>;

    /// Return tasks assigned to a specific agent, newest first.
    /// `limit` caps the result (default 20, max 100).
    fn list_tasks_for_agent(&self, agent_id: &AgentID, limit: usize) -> Vec<TaskSummary>;

    /// Return all active (pending + running) tasks across all agents.
    fn list_active_tasks(&self, limit: usize) -> Vec<TaskSummary>;
}
```

Re-export from `crates/agentos-types/src/lib.rs`:
```rust
pub use registry_query::{AgentRegistryQuery, AgentSummary, TaskQuery, TaskSummary};
```

### Step 2 — Implement `TaskQuery` on the kernel's task store

Read `crates/agentos-kernel/src/task_executor.rs` (or wherever `AgentTask` records are stored — may be `scheduler.rs`) to find the actual task storage structure.

The implementation must:
1. Acquire a read lock on the task map
2. Filter by agent_id or return a single task
3. Map internal task structs to `TaskSummary`
4. Never hold the lock while doing async work

```rust
// In crates/agentos-kernel/src/scheduler.rs or task_executor.rs
use agentos_types::{TaskQuery, TaskSummary, TaskID, AgentID};

impl TaskQuery for Scheduler {
    fn get_task(&self, id: &TaskID) -> Option<TaskSummary> {
        let tasks = self.tasks.read().unwrap_or_else(|e| e.into_inner());
        tasks.get(id).map(|t| TaskSummary {
            id: t.id.clone(),
            description: t.description.clone(),
            status: format!("{:?}", t.status).to_lowercase(),
            assigned_agent: t.assigned_agent.clone(),
            created_at: t.created_at,
            started_at: t.started_at,
            completed_at: t.completed_at,
            result_preview: t.result.as_deref().map(|r| {
                if r.len() > 200 { format!("{}...", &r[..200]) } else { r.to_string() }
            }),
            error: t.error.clone(),
        })
    }

    fn list_tasks_for_agent(&self, agent_id: &AgentID, limit: usize) -> Vec<TaskSummary> {
        let tasks = self.tasks.read().unwrap_or_else(|e| e.into_inner());
        let mut results: Vec<_> = tasks
            .values()
            .filter(|t| t.assigned_agent_id.as_ref() == Some(agent_id))
            .map(|t| TaskSummary { /* same mapping as above */ })
            .collect();
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit.min(100));
        results
    }

    fn list_active_tasks(&self, limit: usize) -> Vec<TaskSummary> {
        let tasks = self.tasks.read().unwrap_or_else(|e| e.into_inner());
        let mut results: Vec<_> = tasks
            .values()
            .filter(|t| matches!(t.status.as_str(), "pending" | "running"))
            .map(|t| TaskSummary { /* same mapping */ })
            .collect();
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit.min(100));
        results
    }
}
```

**Read the actual task struct in the kernel first.** Field names will differ — adapt accordingly.

### Step 3 — Inject `task_registry` in the kernel's ToolExecutionContext builder

In `crates/agentos-kernel/src/task_executor.rs`, update the `ToolExecutionContext` construction (where `agent_registry: None` was placed in subtask 30-05):

```rust
let context = ToolExecutionContext {
    // ... existing fields ...
    agent_registry: Some(Arc::clone(&self.agent_registry) as Arc<dyn AgentRegistryQuery>),
    task_registry: Some(Arc::clone(&self.scheduler) as Arc<dyn TaskQuery>),
};
```

### Step 4 — Create `crates/agentos-tools/src/task_status.rs`

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp, TaskID};
use async_trait::async_trait;

pub struct TaskStatusTool;

impl TaskStatusTool {
    pub fn new() -> Self { Self }
}

impl Default for TaskStatusTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl AgentTool for TaskStatusTool {
    fn name(&self) -> &str { "task-status" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("task.query".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("task.query", PermissionOp::Read) {
            return Err(AgentOSError::PermissionDenied {
                resource: "task.query".to_string(),
                operation: "Read".to_string(),
            });
        }

        let task_id_str = payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("task-status requires 'task_id' field".into())
            })?;

        let task_id: TaskID = task_id_str.parse().map_err(|_| {
            AgentOSError::SchemaValidation(format!("Invalid task_id UUID: {}", task_id_str))
        })?;

        let registry = context.task_registry.ok_or_else(|| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "task-status".into(),
                reason: "Task registry not available in this context".into(),
            }
        })?;

        match registry.get_task(&task_id) {
            Some(t) => Ok(serde_json::json!({
                "found": true,
                "id": t.id.to_string(),
                "description": t.description,
                "status": t.status,
                "assigned_agent": t.assigned_agent,
                "created_at": t.created_at.to_rfc3339(),
                "started_at": t.started_at.map(|dt| dt.to_rfc3339()),
                "completed_at": t.completed_at.map(|dt| dt.to_rfc3339()),
                "result_preview": t.result_preview,
                "error": t.error,
            })),
            None => Ok(serde_json::json!({
                "found": false,
                "task_id": task_id_str,
            })),
        }
    }
}
```

### Step 5 — Create `crates/agentos-tools/src/task_list.rs`

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct TaskListTool;

impl TaskListTool {
    pub fn new() -> Self { Self }
}

impl Default for TaskListTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl AgentTool for TaskListTool {
    fn name(&self) -> &str { "task-list" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("task.query".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("task.query", PermissionOp::Read) {
            return Err(AgentOSError::PermissionDenied {
                resource: "task.query".to_string(),
                operation: "Read".to_string(),
            });
        }

        let registry = context.task_registry.ok_or_else(|| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "task-list".into(),
                reason: "Task registry not available in this context".into(),
            }
        })?;

        let filter = payload.get("filter").and_then(|v| v.as_str()).unwrap_or("mine");
        let limit = payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        let tasks = match filter {
            "active" => registry.list_active_tasks(limit),
            _ => registry.list_tasks_for_agent(&context.agent_id, limit), // "mine" or default
        };

        let serialized: Vec<_> = tasks.into_iter().map(|t| serde_json::json!({
            "id": t.id.to_string(),
            "description": t.description,
            "status": t.status,
            "assigned_agent": t.assigned_agent,
            "created_at": t.created_at.to_rfc3339(),
            "completed_at": t.completed_at.map(|dt| dt.to_rfc3339()),
            "result_preview": t.result_preview,
        })).collect();

        Ok(serde_json::json!({
            "filter": filter,
            "count": serialized.len(),
            "tasks": serialized,
        }))
    }
}
```

### Step 6 — Register in `lib.rs` and `runner.rs`

`lib.rs`:
```rust
pub mod task_list;
pub mod task_status;
pub use task_list::TaskListTool;
pub use task_status::TaskStatusTool;
```

`runner.rs`:
```rust
use crate::task_list::TaskListTool;
use crate::task_status::TaskStatusTool;
runner.register(Box::new(TaskStatusTool::new()));
runner.register(Box::new(TaskListTool::new()));
```

### Step 7 — Create manifests

`tools/core/task-status.toml`:
```toml
[manifest]
name        = "task-status"
version     = "1.0.0"
description = "Query the status, result, and timestamps of a task by ID. Returns 'found: false' if the task does not exist."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["task.query:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "TaskStatusQuery"
output = "TaskStatusResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

`tools/core/task-list.toml`:
```toml
[manifest]
name        = "task-list"
version     = "1.0.0"
description = "List tasks for the calling agent (filter=mine, default) or all active tasks (filter=active). Newest first, max 100 results."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["task.query:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "TaskListQuery"
output = "TaskListResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/registry_query.rs` | Add `TaskQuery`, `TaskSummary` (extends subtask 30-05's file) |
| `crates/agentos-types/src/lib.rs` | Re-export `TaskQuery`, `TaskSummary` |
| `crates/agentos-kernel/src/scheduler.rs` (or `task_executor.rs`) | Implement `TaskQuery` |
| `crates/agentos-kernel/src/task_executor.rs` | Wire `task_registry` into `ToolExecutionContext` |
| `crates/agentos-tools/src/task_status.rs` | Create |
| `crates/agentos-tools/src/task_list.rs` | Create |
| `crates/agentos-tools/src/lib.rs` | Add modules and re-exports |
| `crates/agentos-tools/src/runner.rs` | Register both tools |
| `tools/core/task-status.toml` | Create |
| `tools/core/task-list.toml` | Create |

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo test -p agentos-kernel -- task_status task_list
```
