---
title: "Phase 6: Container Runtime Core"
tags:
  - plan
  - real-world
  - compute
  - docker
  - isolation
  - phase-6
date: 2026-04-08
status: complete
effort: 3d
priority: medium
---

# Phase 6: Container Runtime Core

> A new `agentos-runtime` crate with a pluggable `ComputeRuntime` trait and a Docker backend (via `bollard`), allowing agents to spin up disposable containers for complex workloads.

---

## Why This Phase

The existing `agentos-sandbox` provides process-level isolation (seccomp-BPF, Landlock, rlimits) — excellent for running trusted tool binaries. But agents need heavier isolation for:

- **Data science scripts** — install pandas, matplotlib, numpy in a clean Python environment
- **Build systems** — compile code without polluting the host
- **Untrusted code** — user-submitted scripts that might be malicious
- **Dependency-heavy workloads** — different tasks need different runtimes (Python 3.11, Node 20, Rust nightly)

Process sandboxing can't provide dependency isolation or filesystem snapshots. Containers (Docker/Firecracker) solve this with:
- Image-based environments (clean slate each time)
- Network isolation by default
- cgroup-enforced resource limits
- Automatic cleanup on destruction

---

## Current State

- `agentos-sandbox` provides process-level isolation with seccomp/Landlock/rlimits
- `agentos-wasm` provides WASM tool execution with memory/CPU limits
- No container orchestration exists
- No Docker/containerd/Firecracker integration

## Target State

- New `agentos-runtime` crate
- `ComputeRuntime` trait with `provision`, `exec`, `logs`, `destroy` methods
- `DockerRuntime` implementation using `bollard` crate
- Volume mounting: host scratchpad dir ↔ container `/workspace`
- Network isolation: `none` by default, opt-in outbound only
- Container lifecycle tracking (ID, status, created_at, TTL)
- Kernel integration: `ContainerRegistry` for active container management

---

## Detailed Subtasks

### 1. Create `agentos-runtime` crate

**File:** `crates/agentos-runtime/Cargo.toml`

```toml
[package]
name = "agentos-runtime"
version = "0.1.0"
edition = "2021"

[dependencies]
agentos-types = { path = "../agentos-types" }
async-trait = "0.1"
bollard = "0.18"
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
uuid = { version = "1", features = ["v4"] }
```

Add to workspace `Cargo.toml` members.

### 2. Define `ComputeRuntime` trait

**File:** `crates/agentos-runtime/src/runtime.rs`

```rust
use async_trait::async_trait;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub image: String,                    // "python:3.11-slim", "node:20-alpine"
    pub memory_limit_bytes: u64,          // default: 1 GiB
    pub cpu_limit: f64,                   // default: 1.0 core
    pub pids_limit: i64,                  // default: 100
    pub ttl_seconds: u64,                 // default: 3600 (1 hour)
    pub network: NetworkMode,
    pub env_vars: HashMap<String, String>,
    pub workspace_mount: Option<PathBuf>, // host path → /workspace in container
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMode {
    None,                                 // no network (default)
    Outbound,                             // outbound only, no inbound
    Host,                                 // full host networking (requires approval)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: String,                       // container/VM ID
    pub image: String,
    pub status: ContainerStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub agent_id: AgentID,
    pub task_id: Option<TaskID>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ContainerStatus {
    Creating,
    Running,
    Stopped,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

#[async_trait]
pub trait ComputeRuntime: Send + Sync {
    /// Provision a new container/VM from the spec
    async fn provision(
        &self,
        spec: ContainerSpec,
        agent_id: AgentID,
        task_id: Option<TaskID>,
    ) -> Result<ContainerInfo, AgentOSError>;

    /// Execute a command inside a running container
    async fn exec(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout_ms: u64,
    ) -> Result<ExecResult, AgentOSError>;

    /// Read stdout/stderr logs from a container
    async fn logs(
        &self,
        container_id: &str,
        tail: usize,
    ) -> Result<String, AgentOSError>;

    /// Destroy a container (force-kill if running)
    async fn destroy(&self, container_id: &str) -> Result<(), AgentOSError>;

    /// List all managed containers
    async fn list(&self) -> Result<Vec<ContainerInfo>, AgentOSError>;

    /// Health check the runtime backend
    async fn health_check(&self) -> Result<bool, AgentOSError>;
}
```

### 3. Docker backend implementation

**File:** `crates/agentos-runtime/src/docker.rs`

```rust
use bollard::Docker;
use bollard::container::{Config, CreateContainerOptions, StartContainerOptions};
use bollard::exec::{CreateExecOptions, StartExecResults};

pub struct DockerRuntime {
    client: Docker,
    containers: RwLock<HashMap<String, ContainerInfo>>,
    label_prefix: String,              // "agentos" — for labeling managed containers
}

impl DockerRuntime {
    /// Connect to Docker daemon (socket or HTTP)
    pub async fn new() -> Result<Self, AgentOSError> {
        let client = Docker::connect_with_socket_defaults()?;
        // Verify connection
        client.ping().await?;
        Ok(Self {
            client,
            containers: RwLock::new(HashMap::new()),
            label_prefix: "agentos".into(),
        })
    }
}

#[async_trait]
impl ComputeRuntime for DockerRuntime {
    async fn provision(&self, spec: ContainerSpec, agent_id: AgentID, task_id: Option<TaskID>) -> Result<ContainerInfo, AgentOSError> {
        // 1. Pull image if not present (with timeout)
        // 2. Create container with:
        //    - Memory limit (spec.memory_limit_bytes)
        //    - CPU quota (spec.cpu_limit * 100000)
        //    - PID limit (spec.pids_limit)
        //    - Network mode (none/bridge)
        //    - Volume bind: workspace_mount → /workspace
        //    - Labels: agentos.agent_id, agentos.task_id, agentos.expires_at
        //    - Read-only root filesystem (except /workspace and /tmp)
        //    - No capabilities (drop all)
        //    - Security opt: no-new-privileges
        // 3. Start container
        // 4. Track in self.containers
        // 5. Return ContainerInfo
    }

    async fn exec(&self, container_id: &str, command: Vec<String>, timeout_ms: u64) -> Result<ExecResult, AgentOSError> {
        // 1. Create exec instance
        // 2. Start exec with stdout/stderr attached
        // 3. Collect output with timeout
        // 4. Return ExecResult with exit code + captured output
        // 5. Truncate stdout/stderr to 1MB each
    }

    async fn logs(&self, container_id: &str, tail: usize) -> Result<String, AgentOSError> {
        // Use bollard's logs API with tail parameter
    }

    async fn destroy(&self, container_id: &str) -> Result<(), AgentOSError> {
        // 1. Stop container (SIGTERM, 10s grace)
        // 2. If still running, force kill (SIGKILL)
        // 3. Remove container
        // 4. Clean up workspace directory
        // 5. Remove from self.containers
    }
}
```

### 4. Container reaper (TTL enforcement)

**File:** `crates/agentos-runtime/src/reaper.rs`

```rust
pub struct ContainerReaper {
    runtime: Arc<dyn ComputeRuntime>,
    cancel: CancellationToken,
}

impl ContainerReaper {
    /// Spawn a background task that checks every 60s for expired containers
    pub fn start(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = self.cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {
                        self.sweep_expired().await;
                    }
                }
            }
        })
    }

    async fn sweep_expired(&self) {
        let containers = self.runtime.list().await.unwrap_or_default();
        let now = Utc::now();
        for c in containers {
            if c.expires_at <= now {
                tracing::warn!(id = %c.id, "Reaping expired container");
                let _ = self.runtime.destroy(&c.id).await;
            }
        }
    }
}
```

### 5. Image allowlist

**File:** `crates/agentos-runtime/src/allowlist.rs`

Operators must pre-approve Docker images. Agents cannot pull arbitrary images.

```rust
pub struct ImageAllowlist {
    allowed: HashSet<String>,     // "python:3.11-slim", "node:20-alpine", etc.
    allow_all: bool,              // dangerous: only for development
}

impl ImageAllowlist {
    pub fn from_config(images: Vec<String>) -> Self;
    pub fn is_allowed(&self, image: &str) -> bool;
}
```

**File:** `config/default.toml`

```toml
[runtime]
backend = "docker"                           # "docker" | "firecracker" (future)
default_memory_limit_mb = 1024
default_cpu_limit = 1.0
default_pids_limit = 100
default_ttl_seconds = 3600
max_concurrent_containers = 10
workspace_base_dir = "/tmp/agentos/sandboxes"

[runtime.allowed_images]
images = [
    "python:3.11-slim",
    "python:3.12-slim",
    "node:20-alpine",
    "node:22-alpine",
    "ubuntu:22.04",
    "ubuntu:24.04",
    "rust:1.78-slim",
    "alpine:3.19",
]
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-runtime/Cargo.toml` | **New** — crate manifest |
| `crates/agentos-runtime/src/lib.rs` | **New** — re-exports |
| `crates/agentos-runtime/src/runtime.rs` | **New** — `ComputeRuntime` trait, spec/info types |
| `crates/agentos-runtime/src/docker.rs` | **New** — `DockerRuntime` (bollard-based) |
| `crates/agentos-runtime/src/reaper.rs` | **New** — TTL enforcement background task |
| `crates/agentos-runtime/src/allowlist.rs` | **New** — Image allowlist |
| `Cargo.toml` (workspace) | Add `agentos-runtime` to members |
| `config/default.toml` | Add `[runtime]` configuration section |

---

## Dependencies

- **Requires:** None (independent track)
- **Blocks:** Phase 7 (Container Tools & Quotas)

---

## Test Plan

1. **Unit: ContainerSpec defaults** — Verify default memory, CPU, PID limits
2. **Unit: image allowlist** — Verify allowed images pass, disallowed are rejected
3. **Unit: network mode mapping** — Verify `None` → Docker `none`, `Outbound` → bridge with no inbound
4. **Integration: provision + exec + destroy** (requires Docker)
   - Provision `alpine:3.19` container
   - Exec `echo hello` → verify stdout contains "hello"
   - Exec `cat /etc/os-release` → verify alpine
   - Destroy → verify container removed
5. **Integration: resource limits**
   - Provision with 64MB memory limit
   - Exec a script that allocates 128MB → verify OOM kill
6. **Integration: TTL reaper**
   - Provision container with 2s TTL
   - Wait 5s, run reaper sweep
   - Verify container is destroyed
7. **Integration: workspace mount**
   - Create temp dir, write a file
   - Provision container with workspace mount
   - Exec `cat /workspace/file` → verify file contents
8. **Security: no capabilities**
   - Exec `mount` → verify permission denied
   - Exec network command with `NetworkMode::None` → verify no connectivity

---

## Verification

```bash
cargo build -p agentos-runtime
cargo test -p agentos-runtime  # Note: integration tests require Docker daemon
cargo clippy -p agentos-runtime -- -D warnings
cargo fmt -p agentos-runtime -- --check
```
