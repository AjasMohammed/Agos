---
title: "Phase 7: Container Tools & Quotas"
tags:
  - plan
  - real-world
  - compute
  - tools
  - phase-7
date: 2026-04-08
status: complete
effort: 2d
priority: medium
---

# Phase 7: Container Tools & Quotas

> Expose the container runtime to agents via `container.*` tools, add per-agent quota enforcement, kernel command wiring, and a Web UI compute tab.

---

## Why This Phase

Phase 6 built the runtime backend. This phase makes it usable by agents and operators:

- **Agent tools** — `container.create`, `container.exec`, `container.logs`, `container.destroy`, `container.list`
- **Quota enforcement** — per-agent limits on concurrent containers and total memory
- **Kernel integration** — KernelCommands, run_loop dispatch, CLI subcommands
- **Web UI** — live view of active containers with logs and kill buttons

---

## Current State

- Phase 6 provides `ComputeRuntime` trait, `DockerRuntime`, `ContainerReaper`, `ImageAllowlist`
- `agentos-kernel` has `Kernel` struct with many subsystem fields
- Tool manifests in `tools/core/` define agent-facing tools
- Web UI has handler pattern for subsystem pages (tasks, tools, agents, etc.)
- No container-related kernel commands or tools exist

## Target State

- 5 agent tools: `container.create`, `container.exec`, `container.logs`, `container.destroy`, `container.list`
- `ContainerQuota` per-agent enforcement (max containers, max memory, max CPU)
- KernelCommands: `ContainerCreate`, `ContainerExec`, `ContainerLogs`, `ContainerDestroy`, `ContainerList`
- CLI: `agentos container create/exec/logs/destroy/list`
- Web UI: `/compute` page with active container table, log viewer, destroy button
- Audit events: `ContainerProvisioned`, `ContainerExecRun`, `ContainerDestroyed`, `ContainerQuotaExceeded`

---

## Detailed Subtasks

### 1. Container quota system

**File:** `crates/agentos-runtime/src/quota.rs` (new)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerQuota {
    pub max_containers: usize,         // per agent, default: 3
    pub max_total_memory_bytes: u64,   // per agent, default: 4 GiB
    pub max_total_cpu: f64,            // per agent, default: 4.0 cores
}

pub struct QuotaEnforcer {
    quotas: RwLock<HashMap<AgentID, ContainerQuota>>,
    default_quota: ContainerQuota,
}

impl QuotaEnforcer {
    pub fn new(default_quota: ContainerQuota) -> Self;

    /// Check if an agent can provision a container with the given spec.
    /// Returns Ok(()) or Err(AgentOSError::QuotaExceeded { ... })
    pub fn check(
        &self,
        agent_id: &AgentID,
        spec: &ContainerSpec,
        current_containers: &[ContainerInfo],
    ) -> Result<(), AgentOSError>;

    /// Set custom quota for an agent
    pub fn set_quota(&self, agent_id: AgentID, quota: ContainerQuota);
}
```

### 2. Tool manifests

**File:** `tools/core/container-create.toml` (new)

```toml
[manifest]
name = "container-create"
version = "1.0.0"
description = "Create an isolated container for running scripts or workloads"
author = "agentos-core"
trust_tier = "core"
tags = ["compute", "sandbox", "container"]

[capabilities_required]
permissions = ["container.create:x"]

[intent_schema]
type = "execute"
target_tool = "container-create"

[sandbox]
network = false
fs_write = false
max_memory_mb = 32
max_cpu_ms = 30000
```

Similarly for: `container-exec.toml`, `container-logs.toml`, `container-destroy.toml`, `container-list.toml`.

### 3. Tool implementations

**File:** `crates/agentos-tools/src/container.rs` (new)

```rust
/// container-create: provisions a new container
/// Input: { "image": "python:3.11-slim", "memory_mb": 512, "network": "none" }
/// Output: { "container_id": "...", "workspace": "/workspace", "expires_at": "..." }
pub async fn container_create(
    input: Value,
    context: &ToolExecutionContext,
) -> Result<Value, AgentOSError>;

/// container-exec: runs a command in an existing container
/// Input: { "container_id": "...", "command": ["python", "-c", "print('hello')"], "timeout_ms": 30000 }
/// Output: { "exit_code": 0, "stdout": "hello\n", "stderr": "", "duration_ms": 150 }
pub async fn container_exec(
    input: Value,
    context: &ToolExecutionContext,
) -> Result<Value, AgentOSError>;

/// container-logs: read recent logs from a container
/// Input: { "container_id": "...", "tail": 100 }
/// Output: { "logs": "..." }
pub async fn container_logs(
    input: Value,
    context: &ToolExecutionContext,
) -> Result<Value, AgentOSError>;

/// container-destroy: tear down a container
/// Input: { "container_id": "..." }
/// Output: { "destroyed": true }
pub async fn container_destroy(
    input: Value,
    context: &ToolExecutionContext,
) -> Result<Value, AgentOSError>;

/// container-list: list agent's active containers
/// Input: {}
/// Output: { "containers": [...] }
pub async fn container_list(
    input: Value,
    context: &ToolExecutionContext,
) -> Result<Value, AgentOSError>;
```

### 4. KernelCommand wiring

**File:** `crates/agentos-bus/src/message.rs`

Add variants:
```rust
ContainerCreate { agent_id: AgentID, image: String, memory_mb: u64, network: String },
ContainerExec { container_id: String, command: Vec<String>, timeout_ms: u64 },
ContainerLogs { container_id: String, tail: usize },
ContainerDestroy { container_id: String },
ContainerList { agent_id: Option<AgentID> },
```

**File:** `crates/agentos-kernel/src/commands/container.rs` (new)

Handler implementations delegating to `ComputeRuntime` + `QuotaEnforcer`.

**File:** `crates/agentos-kernel/src/run_loop.rs`

Add dispatch arms for container commands.

### 5. CLI subcommands

**File:** `crates/agentos-cli/src/commands/container.rs` (new)

```
agentos container create --image python:3.11-slim --memory 512 --agent <id>
agentos container exec <container_id> -- python -c "print('hello')"
agentos container logs <container_id> --tail 50
agentos container destroy <container_id>
agentos container list [--agent <id>]
```

### 6. Web UI compute page

**File:** `crates/agentos-web/src/handlers/compute.rs` (new)

```rust
/// GET /compute — list active containers
pub async fn compute_dashboard(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError>;

/// GET /compute/:id/logs — SSE stream of container logs
pub async fn container_log_stream(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = ...>>, AppError>;

/// POST /compute/:id/destroy — kill a container (with CSRF)
pub async fn destroy_container(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Redirect, AppError>;
```

**Template:** `crates/agentos-web/templates/compute.html`

Table columns: Container ID, Image, Agent, Status, Memory, CPU, Created, Expires, Actions (logs / destroy).

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-runtime/src/quota.rs` | **New** — Per-agent quota enforcement |
| `crates/agentos-runtime/src/lib.rs` | Re-export quota module |
| `tools/core/container-create.toml` | **New** — Tool manifest |
| `tools/core/container-exec.toml` | **New** — Tool manifest |
| `tools/core/container-logs.toml` | **New** — Tool manifest |
| `tools/core/container-destroy.toml` | **New** — Tool manifest |
| `tools/core/container-list.toml` | **New** — Tool manifest |
| `crates/agentos-tools/src/container.rs` | **New** — Tool implementations |
| `crates/agentos-bus/src/message.rs` | Add 5 container command variants |
| `crates/agentos-kernel/src/commands/container.rs` | **New** — Command handlers |
| `crates/agentos-kernel/src/kernel.rs` | Add `compute_runtime` and `quota_enforcer` fields |
| `crates/agentos-kernel/src/run_loop.rs` | Add dispatch arms |
| `crates/agentos-cli/src/commands/container.rs` | **New** — CLI subcommands |
| `crates/agentos-web/src/handlers/compute.rs` | **New** — Web UI handlers |
| `crates/agentos-web/templates/compute.html` | **New** — Compute dashboard template |
| `crates/agentos-web/src/router.rs` | Register `/compute` routes |
| `config/default.toml` | Add quota defaults to `[runtime]` section |

---

## Dependencies

- **Requires:** Phase 6 (Container Runtime Core)
- **Blocks:** None (end of Subsystem C)

---

## Test Plan

1. **Unit: quota enforcement** — Agent with 3 containers tries to create 4th → `QuotaExceeded`
2. **Unit: quota memory check** — Agent at 3.5 GiB tries 1 GiB container → rejected
3. **Unit: tool input validation** — Invalid image name, negative memory → proper error
4. **Integration: create → exec → destroy** (requires Docker)
   - Create python container
   - Exec `python -c "import sys; print(sys.version)"` → verify Python version in stdout
   - Destroy → verify clean
5. **Integration: CLI round-trip** — `agentos container create` → `exec` → `list` → `destroy`
6. **Integration: ownership** — Agent A creates container, Agent B tries to exec → rejected
7. **Security: image allowlist** — Create with disallowed image → rejected
8. **Security: no host network** — Create with `NetworkMode::Host` without approval → rejected

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-runtime
cargo test -p agentos-kernel
cargo test -p agentos-tools
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
