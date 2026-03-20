---
title: 30-05 Agent Discovery Tool
tags:
  - tools
  - coordination
  - next-steps
  - subtask
date: 2026-03-18
status: planned
effort: 6h
priority: high
---

# 30-05 — Agent Discovery Tool

> Add `agent-list` so agents can discover available peers without hardcoding names. Requires extending `ToolExecutionContext` with an `AgentRegistryQuery` trait and implementing it on the kernel's `AgentRegistry`.

---

## Why This Phase

`agent-message` requires knowing the target agent's name. Without `agent-list`, an agent either hardcodes peer names (brittle) or uses `shell-exec` to query the kernel (bypasses capability model). This is a fundamental gap in multi-agent coordination.

---

## Current → Target State

| Capability | Current | Target |
|-----------|---------|--------|
| Discover available agents | none | `agent-list` tool |
| ToolExecutionContext | no agent registry access | `agent_registry: Option<Arc<dyn AgentRegistryQuery>>` |
| AgentRegistryQuery trait | does not exist | defined in `agentos-types`, implemented by `AgentRegistry` |

---

## What to Do

This subtask touches three crates: `agentos-types`, `agentos-tools`, `agentos-kernel`.

### Step 1 — Define `AgentRegistryQuery` in `agentos-types`

Read `crates/agentos-types/src/lib.rs` to find the right module to add to. Create or add to `crates/agentos-types/src/registry_query.rs`:

```rust
use crate::{AgentID, TaskID};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lightweight agent summary returned by the agent-list tool.
/// Intentionally does not expose internal kernel state (no Arc references).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id: AgentID,
    pub name: String,
    pub status: String,             // "idle" | "running" | "paused" | "stopped"
    pub capabilities: Vec<String>,  // permission strings the agent holds
    pub registered_at: DateTime<Utc>,
}

/// Thin query interface for the agent registry.
/// Defined in agentos-types so agentos-tools can reference it
/// without creating a circular dependency on agentos-kernel.
pub trait AgentRegistryQuery: Send + Sync {
    /// Return all registered agents as lightweight summaries.
    fn list_agents(&self) -> Vec<AgentSummary>;

    /// Return a single agent by ID, or None if not found.
    fn get_agent(&self, id: &AgentID) -> Option<AgentSummary>;
}
```

Re-export from `crates/agentos-types/src/lib.rs`:
```rust
pub mod registry_query;
pub use registry_query::{AgentRegistryQuery, AgentSummary};
```

### Step 2 — Extend `ToolExecutionContext` in `crates/agentos-tools/src/traits.rs`

Read the file first. Add two new optional fields:

```rust
use agentos_types::{AgentRegistryQuery, TaskQuery};  // TaskQuery added in subtask 30-06

pub struct ToolExecutionContext {
    pub data_dir: PathBuf,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub trace_id: TraceID,
    pub permissions: PermissionSet,
    pub vault: Option<std::sync::Arc<agentos_vault::ProxyVault>>,
    pub hal: Option<std::sync::Arc<agentos_hal::HardwareAbstractionLayer>>,
    pub file_lock_registry: Option<std::sync::Arc<crate::file_lock::FileLockRegistry>>,
    // --- NEW ---
    pub agent_registry: Option<std::sync::Arc<dyn AgentRegistryQuery>>,
    pub task_registry: Option<std::sync::Arc<dyn TaskQuery>>,  // added in 30-06, add None stub here
}
```

**BREAKING CHANGE:** Every struct literal `ToolExecutionContext { ... }` must be updated with both new fields set to `None`. Find all usages:
```bash
grep -rn "ToolExecutionContext {" crates/ --include="*.rs"
```
Update every site by adding:
```rust
agent_registry: None,
task_registry: None,
```

### Step 3 — Implement `AgentRegistryQuery` on `AgentRegistry` in `agentos-kernel`

Read `crates/agentos-kernel/src/agent_registry.rs` to understand the current `AgentRegistry` struct and its agent map.

Add the implementation:
```rust
use agentos_types::{AgentRegistryQuery, AgentSummary};

impl AgentRegistryQuery for AgentRegistry {
    fn list_agents(&self) -> Vec<AgentSummary> {
        // Read the internal agent map (likely an Arc<RwLock<HashMap<AgentID, RegisteredAgent>>>)
        // and convert each entry to AgentSummary.
        // Adapt field names to match the actual AgentRegistry struct fields.
        self.agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|agent| AgentSummary {
                id: agent.id.clone(),
                name: agent.name.clone(),
                status: format!("{:?}", agent.status).to_lowercase(),
                capabilities: agent.permissions
                    .entries()
                    .map(|e| e.to_string())
                    .collect(),
                registered_at: agent.registered_at,
            })
            .collect()
    }

    fn get_agent(&self, id: &AgentID) -> Option<AgentSummary> {
        self.agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|agent| AgentSummary {
                id: agent.id.clone(),
                name: agent.name.clone(),
                status: format!("{:?}", agent.status).to_lowercase(),
                capabilities: vec![],
                registered_at: agent.registered_at,
            })
    }
}
```

**Important:** Read the actual `AgentRegistry` struct fields before writing this. Field names may differ from the above sketch. The key concern is avoiding deadlocks — do NOT call any method that acquires the same lock recursively.

### Step 4 — Inject `agent_registry` in the kernel's ToolExecutionContext builder

Find where `ToolExecutionContext` is constructed in `crates/agentos-kernel/src/task_executor.rs` (or wherever the kernel creates the context before calling `ToolRunner::execute`). Inject the registry:

```rust
let context = ToolExecutionContext {
    // ... existing fields ...
    agent_registry: Some(Arc::clone(&self.agent_registry) as Arc<dyn AgentRegistryQuery>),
    task_registry: None, // filled in by subtask 30-06
};
```

### Step 5 — Create `crates/agentos-tools/src/agent_list.rs`

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct AgentListTool;

impl AgentListTool {
    pub fn new() -> Self { Self }
}

impl Default for AgentListTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl AgentTool for AgentListTool {
    fn name(&self) -> &str { "agent-list" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.registry".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("agent.registry", PermissionOp::Read) {
            return Err(AgentOSError::PermissionDenied {
                resource: "agent.registry".to_string(),
                operation: "Read".to_string(),
            });
        }

        let registry = context.agent_registry.ok_or_else(|| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "agent-list".into(),
                reason: "Agent registry not available in this execution context".into(),
            }
        })?;

        let status_filter = payload
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());

        let agents = registry.list_agents();
        let filtered: Vec<_> = agents
            .into_iter()
            .filter(|a| {
                status_filter
                    .as_ref()
                    .map(|f| a.status.contains(f.as_str()))
                    .unwrap_or(true)
            })
            .map(|a| serde_json::json!({
                "id": a.id.to_string(),
                "name": a.name,
                "status": a.status,
                "capabilities": a.capabilities,
                "registered_at": a.registered_at.to_rfc3339(),
            }))
            .collect();

        Ok(serde_json::json!({
            "count": filtered.len(),
            "agents": filtered,
        }))
    }
}
```

### Step 6 — Register in `lib.rs` and `runner.rs`

`lib.rs`:
```rust
pub mod agent_list;
pub use agent_list::AgentListTool;
```

`runner.rs`:
```rust
use crate::agent_list::AgentListTool;
// In registration:
runner.register(Box::new(AgentListTool::new()));
```

### Step 7 — Create `tools/core/agent-list.toml`

```toml
[manifest]
name        = "agent-list"
version     = "1.0.0"
description = "List all registered agents with their status and capabilities. Optional 'status' filter (idle/running/paused/stopped)."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["agent.registry:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "AgentListQuery"
output = "AgentListResult"

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
| `crates/agentos-types/src/registry_query.rs` | Create — `AgentRegistryQuery`, `AgentSummary` traits/structs |
| `crates/agentos-types/src/lib.rs` | Add `pub mod registry_query;` and re-exports |
| `crates/agentos-tools/src/traits.rs` | Add `agent_registry` and `task_registry` fields to `ToolExecutionContext` |
| `crates/agentos-tools/src/agent_list.rs` | Create |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod agent_list;` and re-export |
| `crates/agentos-tools/src/runner.rs` | Register `AgentListTool` |
| `crates/agentos-kernel/src/agent_registry.rs` | Implement `AgentRegistryQuery` |
| `crates/agentos-kernel/src/task_executor.rs` | Inject `agent_registry` into `ToolExecutionContext` |
| `tools/core/agent-list.toml` | Create |
| All files with `ToolExecutionContext {` literal | Add `agent_registry: None, task_registry: None` |

---

## Prerequisites

None — this is the first phase that modifies `ToolExecutionContext`. Subtask 30-06 extends this work.

## Verification

```bash
# All struct literal sites compile
cargo build --workspace
cargo test --workspace
# Confirm agent-list visible in tool list
cargo test -p agentos-kernel -- agent_list
```

Check that the `ToolExecutionContext` struct literal grep finds and patches ALL sites before building.
