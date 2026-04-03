---
title: "Phase 01 — Sub-Agent Spawning"
tags:
  - kernel
  - agents
  - v4
  - plan
date: 2026-04-02
status: planned
effort: 2d
priority: critical
---

# Phase 01 — Sub-Agent Spawning

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> Add `SpawnSubAgent` and `AwaitSubAgents` kernel commands, parent-child task tracking in the scheduler, and capability scoping so a running task can spawn child tasks and wait for their results.

---

## Why This Phase

This is the foundation. Nothing in later phases works without a parent task being able to say "run this subtask under these constraints and give me back the result." The capability scoping and depth limit enforcement live here.

---

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `KernelCommand` | No spawn command | `SpawnSubAgent`, `AwaitSubAgents` variants added |
| `TaskScheduler` | Flat task list | Tasks have optional `parent_task_id`, `child_task_ids` |
| `AgentTask` (types) | No parent link | `parent_task_id: Option<TaskID>`, `spawn_depth: u8` |
| `CapabilityEngine` | Issues tokens independently | `scope_for_child()` enforces intersection |
| Depth limit | None | Kernel rejects spawn if `spawn_depth >= 5` |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/task.rs` | Add `parent_task_id`, `spawn_depth` fields to `AgentTask` |
| `crates/agentos-bus/src/message.rs` | Add `SpawnSubAgent`, `AwaitSubAgents` `KernelCommand` variants + `SubAgentSpawned` response |
| `crates/agentos-kernel/src/commands/sub_agent.rs` | New file — `cmd_spawn_sub_agent()`, `cmd_await_sub_agents()` |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch arms for the two new commands |
| `crates/agentos-capability/src/lib.rs` | `scope_for_child(parent: &CapabilityToken, requested: &PermissionSet) -> Result<CapabilityToken>` |
| `crates/agentos-kernel/src/scheduler.rs` | Track `child_task_ids` per task; cascade cancel |

---

## Detailed Tasks

### Task 1: Extend `AgentTask` with parent link and depth

**Files:**
- Modify: `crates/agentos-types/src/task.rs`

- [ ] **Step 1: Read the current `AgentTask` struct**

Read `crates/agentos-types/src/task.rs` to find the exact struct definition before editing.

- [ ] **Step 2: Add fields**

Find the `AgentTask` struct and add:

```rust
/// `Some(id)` when this task was spawned by another task.
#[serde(skip_serializing_if = "Option::is_none")]
pub parent_task_id: Option<TaskID>,

/// How many spawn hops from a root task (root = 0, child = 1, grandchild = 2, …).
#[serde(default)]
pub spawn_depth: u8,
```

- [ ] **Step 3: Write the failing test**

In `crates/agentos-types/src/task.rs` inside `#[cfg(test)]`:

```rust
#[test]
fn test_agent_task_parent_fields_default() {
    let task = AgentTask {
        id: TaskID::new(),
        agent_id: AgentID::new(),
        prompt: "test".to_string(),
        status: TaskStatus::Pending,
        parent_task_id: None,
        spawn_depth: 0,
        // fill other required fields with defaults
        ..Default::default()
    };
    assert!(task.parent_task_id.is_none());
    assert_eq!(task.spawn_depth, 0);
}
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p agentos-types test_agent_task_parent_fields_default
```

Expected: PASS

- [ ] **Step 5: Fix any compilation errors from the struct change**

If other crates construct `AgentTask` with struct literals, add `parent_task_id: None, spawn_depth: 0` to each. Search:

```bash
cargo build --workspace 2>&1 | grep "missing field"
```

Fix every missing field error.

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-types/src/task.rs
git commit -m "feat(types): add parent_task_id and spawn_depth to AgentTask"
```

---

### Task 2: Add `CapabilityEngine::scope_for_child()`

**Files:**
- Modify: `crates/agentos-capability/src/lib.rs`

- [ ] **Step 1: Write the failing test**

In `crates/agentos-capability/src/lib.rs` inside `#[cfg(test)]`:

```rust
#[test]
fn test_scope_for_child_intersects_permissions() {
    let engine = CapabilityEngine::new(b"test-secret-key-32-bytes-padding!");
    let agent = AgentID::new();
    let task = TaskID::new();

    // Parent has read + write + shell
    let parent_perms = PermissionSet::from_strings(&["read", "write", "shell"]);
    let parent_token = engine.issue(agent, task, parent_perms.clone()).unwrap();

    // Child requests read + shell + network (network not in parent)
    let child_requested = PermissionSet::from_strings(&["read", "shell", "network"]);
    let child_token = engine.scope_for_child(&parent_token, &child_requested).unwrap();

    // Child should only get read + shell (intersection)
    let child_perms = engine.validate(&child_token).unwrap();
    assert!(child_perms.allows("read"));
    assert!(child_perms.allows("shell"));
    assert!(!child_perms.allows("network")); // not in parent
    assert!(!child_perms.allows("write"));   // not requested
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
cargo test -p agentos-capability test_scope_for_child_intersects_permissions
```

Expected: FAIL — `scope_for_child` not found

- [ ] **Step 3: Implement `scope_for_child`**

In `crates/agentos-capability/src/lib.rs`, add to `CapabilityEngine`:

```rust
/// Issue a capability token for a child agent scoped to the intersection of
/// `parent_token`'s permissions and `requested`. Returns `Err` if the parent
/// token is invalid or if the intersection is empty.
pub fn scope_for_child(
    &self,
    parent_token: &CapabilityToken,
    requested: &PermissionSet,
) -> Result<CapabilityToken, AgentOSError> {
    let parent_perms = self.validate(parent_token)?;
    let intersection = parent_perms.intersect(requested);
    if intersection.is_empty() {
        return Err(AgentOSError::PermissionDenied(
            "child requested permissions not held by parent".to_string(),
        ));
    }
    self.issue(parent_token.agent_id, parent_token.task_id, intersection)
}
```

Also add `intersect()` to `PermissionSet` if it doesn't exist:

```rust
pub fn intersect(&self, other: &PermissionSet) -> PermissionSet {
    let entries: Vec<String> = self
        .entries
        .iter()
        .filter(|e| other.allows(e))
        .cloned()
        .collect();
    PermissionSet::from_strings(&entries.iter().map(|s| s.as_str()).collect::<Vec<_>>())
}

pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
}
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p agentos-capability test_scope_for_child_intersects_permissions
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-capability/src/lib.rs
git commit -m "feat(capability): add scope_for_child() for sub-agent capability inheritance"
```

---

### Task 3: Add `SpawnSubAgent` and `AwaitSubAgents` kernel commands

**Files:**
- Modify: `crates/agentos-bus/src/message.rs`

- [ ] **Step 1: Read the `KernelCommand` enum**

Read `crates/agentos-bus/src/message.rs` around line 29 to understand the existing variant format.

- [ ] **Step 2: Add command variants**

After the existing `RunTask` block, add:

```rust
/// Spawn a child task from within a running parent task.
/// The child inherits a scoped subset of the parent's capabilities.
SpawnSubAgent {
    /// The parent task that is spawning this child.
    parent_task_id: TaskID,
    /// Name of the agent to run the child task (must be registered).
    agent_name: String,
    /// The prompt / goal for the child task.
    prompt: String,
    /// Permissions requested for the child (intersected with parent's at spawn time).
    requested_permissions: Vec<String>,
},

/// Block the calling task until all listed child tasks complete.
/// Returns a list of (TaskID, output_summary) pairs.
AwaitSubAgents {
    /// The parent task waiting.
    parent_task_id: TaskID,
    /// Child task IDs to wait for.
    child_task_ids: Vec<TaskID>,
},
```

- [ ] **Step 3: Add `SubAgentSpawned` response variant**

In the `KernelResponse` enum, add:

```rust
SubAgentSpawned {
    child_task_id: TaskID,
},

SubAgentResults {
    /// (child_task_id, result_summary) pairs in completion order.
    results: Vec<(TaskID, String)>,
},
```

- [ ] **Step 4: Build to confirm no compile errors**

```bash
cargo build -p agentos-bus
```

Expected: clean build

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-bus/src/message.rs
git commit -m "feat(bus): add SpawnSubAgent, AwaitSubAgents commands and responses"
```

---

### Task 4: Implement the kernel command handlers

**Files:**
- Create: `crates/agentos-kernel/src/commands/sub_agent.rs`
- Modify: `crates/agentos-kernel/src/commands/mod.rs`
- Modify: `crates/agentos-kernel/src/run_loop.rs`

- [ ] **Step 1: Create the handler file**

Create `crates/agentos-kernel/src/commands/sub_agent.rs`:

```rust
use crate::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::{AgentID, TaskID};

impl Kernel {
    pub async fn cmd_spawn_sub_agent(
        &self,
        parent_task_id: TaskID,
        agent_name: &str,
        prompt: &str,
        requested_permissions: &[String],
    ) -> KernelResponse {
        // 1. Look up parent task to get its capability token and spawn_depth.
        let parent_task = {
            let scheduler = self.scheduler.read().await;
            match scheduler.get_task(parent_task_id) {
                Some(t) => t.clone(),
                None => {
                    return KernelResponse::Error(format!(
                        "parent task {} not found",
                        parent_task_id
                    ))
                }
            }
        };

        // 2. Enforce depth limit.
        if parent_task.spawn_depth >= 5 {
            return KernelResponse::Error(
                "spawn depth limit (5) exceeded — cannot spawn further sub-agents".to_string(),
            );
        }

        // 3. Look up the agent.
        let agent_id = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(agent_name) {
                Some(a) => a.id,
                None => {
                    return KernelResponse::Error(format!("agent '{}' not registered", agent_name))
                }
            }
        };

        // 4. Scope capabilities: child = parent_caps ∩ requested.
        let parent_cap = match &parent_task.capability_token {
            Some(t) => t.clone(),
            None => {
                return KernelResponse::Error("parent task has no capability token".to_string())
            }
        };
        let requested_perm_set =
            agentos_capability::PermissionSet::from_strings(requested_permissions);
        let child_cap = match self
            .capability_engine
            .scope_for_child(&parent_cap, &requested_perm_set)
        {
            Ok(c) => c,
            Err(e) => return KernelResponse::Error(e.to_string()),
        };

        // 5. Build the child AgentTask.
        let child_task_id = TaskID::new();
        let child_task = agentos_types::AgentTask {
            id: child_task_id,
            agent_id,
            prompt: prompt.to_string(),
            status: agentos_types::TaskStatus::Pending,
            parent_task_id: Some(parent_task_id),
            spawn_depth: parent_task.spawn_depth + 1,
            capability_token: Some(child_cap),
            ..Default::default()
        };

        // 6. Enqueue the child task.
        {
            let mut scheduler = self.scheduler.write().await;
            if let Err(e) = scheduler.enqueue(child_task) {
                return KernelResponse::Error(e.to_string());
            }
            // Register the child under the parent for cascade-cancel.
            scheduler.register_child(parent_task_id, child_task_id);
        }

        // 7. Audit.
        let _ = self.audit(
            agentos_audit::AuditEventType::TaskStarted,
            agentos_audit::AuditSeverity::Info,
            Some(agent_id),
            Some(child_task_id),
            serde_json::json!({
                "parent_task_id": parent_task_id.to_string(),
                "agent": agent_name,
                "spawn_depth": parent_task.spawn_depth + 1,
            }),
        );

        KernelResponse::SubAgentSpawned { child_task_id }
    }

    pub async fn cmd_await_sub_agents(
        &self,
        parent_task_id: TaskID,
        child_task_ids: &[TaskID],
    ) -> KernelResponse {
        // Poll until all children are in a terminal state (Completed | Failed | Cancelled).
        // In a real implementation this would use an async notification; here we use the
        // scheduler's completed task store which is already populated by task_completion.rs.
        let mut results = Vec::new();
        for &child_id in child_task_ids {
            let summary = {
                let scheduler = self.scheduler.read().await;
                scheduler
                    .get_task_result_summary(child_id)
                    .unwrap_or_else(|| format!("child task {} has no result", child_id))
            };
            results.push((child_id, summary));
        }
        KernelResponse::SubAgentResults { results }
    }
}
```

- [ ] **Step 2: Register in `commands/mod.rs`**

Add to the pub mod list in `crates/agentos-kernel/src/commands/mod.rs`:

```rust
pub mod sub_agent;
```

- [ ] **Step 3: Add dispatch arms in `run_loop.rs`**

Find the giant `match command` block in `run_loop.rs` and add:

```rust
KernelCommand::SpawnSubAgent {
    parent_task_id,
    agent_name,
    prompt,
    requested_permissions,
} => {
    self.cmd_spawn_sub_agent(
        parent_task_id,
        &agent_name,
        &prompt,
        &requested_permissions,
    )
    .await
}

KernelCommand::AwaitSubAgents {
    parent_task_id,
    child_task_ids,
} => {
    self.cmd_await_sub_agents(parent_task_id, &child_task_ids)
        .await
}
```

- [ ] **Step 4: Add `register_child` and `get_task_result_summary` to the scheduler**

In `crates/agentos-kernel/src/scheduler.rs`, add:

```rust
/// Track a child task under its parent for cascade-cancel.
pub fn register_child(&mut self, parent_id: TaskID, child_id: TaskID) {
    self.child_map
        .entry(parent_id)
        .or_default()
        .push(child_id);
}

/// Return a brief text summary of a completed task's output, or None if not found.
pub fn get_task_result_summary(&self, task_id: TaskID) -> Option<String> {
    self.completed_tasks
        .get(&task_id)
        .and_then(|t| t.result_summary.clone())
}
```

Add the field to the scheduler struct:

```rust
child_map: HashMap<TaskID, Vec<TaskID>>,
```

- [ ] **Step 5: Build the workspace**

```bash
cargo build --workspace 2>&1 | head -40
```

Fix any errors. Common: missing struct fields, unknown method names.

- [ ] **Step 6: Write an integration test**

In `crates/agentos-kernel/src/kernel.rs` `#[cfg(test)]` section:

```rust
#[tokio::test]
async fn test_spawn_sub_agent_depth_limit() {
    let kernel = setup_kernel().await;
    // Register an agent.
    let agent = register_mock_agent(&kernel, "worker").await;

    // Create a fake parent task at depth 5 (the limit).
    let parent_id = TaskID::new();
    {
        let mut sched = kernel.scheduler.write().await;
        sched.insert_task_for_test(AgentTask {
            id: parent_id,
            agent_id: agent,
            spawn_depth: 5,
            status: TaskStatus::Running,
            capability_token: Some(make_capability(&kernel, agent, parent_id)),
            ..Default::default()
        });
    }

    let resp = kernel
        .cmd_spawn_sub_agent(parent_id, "worker", "do something", &["read".to_string()])
        .await;

    assert!(
        matches!(resp, KernelResponse::Error(msg) if msg.contains("depth limit")),
        "expected depth limit error"
    );
}
```

- [ ] **Step 7: Run the test**

```bash
cargo test -p agentos-kernel test_spawn_sub_agent_depth_limit
```

Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/agentos-kernel/src/commands/sub_agent.rs \
        crates/agentos-kernel/src/commands/mod.rs \
        crates/agentos-kernel/src/run_loop.rs \
        crates/agentos-kernel/src/scheduler.rs
git commit -m "feat(kernel): add SpawnSubAgent and AwaitSubAgents command handlers"
```

---

### Task 5: Cascade cancel to children

**Files:**
- Modify: `crates/agentos-kernel/src/commands/task.rs` (or wherever `cmd_cancel_task` lives)

- [ ] **Step 1: Find `cmd_cancel_task`**

```bash
grep -rn "cmd_cancel_task" crates/agentos-kernel/src/
```

- [ ] **Step 2: Add child cascade**

In `cmd_cancel_task`, after cancelling the parent task, add:

```rust
// Cascade cancel to all registered children.
let children = {
    let scheduler = self.scheduler.read().await;
    scheduler.get_children(task_id)
};
for child_id in children {
    Box::pin(self.cmd_cancel_task(child_id)).await;
}
```

Add `get_children()` to the scheduler:

```rust
pub fn get_children(&self, task_id: TaskID) -> Vec<TaskID> {
    self.child_map.get(&task_id).cloned().unwrap_or_default()
}
```

- [ ] **Step 3: Write the failing test**

```rust
#[tokio::test]
async fn test_cancel_parent_cascades_to_children() {
    let kernel = setup_kernel().await;
    let agent = register_mock_agent(&kernel, "worker").await;

    // Spawn a parent and child task.
    let parent_id = TaskID::new();
    let child_id = TaskID::new();
    {
        let mut sched = kernel.scheduler.write().await;
        sched.insert_task_for_test(make_running_task(parent_id, agent, 0));
        sched.insert_task_for_test(make_running_task(child_id, agent, 1));
        sched.register_child(parent_id, child_id);
    }

    kernel.cmd_cancel_task(parent_id).await;

    let child_status = kernel.scheduler.read().await.get_task(child_id).unwrap().status.clone();
    assert_eq!(child_status, TaskStatus::Cancelled);
}
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p agentos-kernel test_cancel_parent_cascades_to_children
```

Expected: PASS

- [ ] **Step 5: Run full test suite**

```bash
cargo test --workspace
```

Expected: all pass

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-kernel/src/commands/ crates/agentos-kernel/src/scheduler.rs
git commit -m "feat(kernel): cascade task cancellation to child sub-agents"
```

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

All four must pass before this phase is marked complete.

## Dependencies

- Requires: None
- Blocks: [[02-context-handoff]], [[03-coordination-tools]]
