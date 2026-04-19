---
title: "Phase 4: Managed Processes"
tags:
  - kernel
  - capabilities
  - v4
  - phase-4
date: 2026-04-12
status: planned
effort: 2.5d
priority: high
---

# Phase 4: Managed Processes (`proc.*`)

> Allow agents to spawn, monitor, signal, and manage long-running processes — with per-agent process tables, cgroup v2 resource limits, and binary allowlists.

---

## Why This Phase

Agents often need to run processes that outlive a single tool call: dev servers (`python -m http.server`), file watchers (`cargo watch`), database instances, test runners. Currently, shell-exec runs a command and waits for it to finish — there's no way to start a background process, check its status, or stop it later.

OpenHands handles this by giving agents a persistent shell session. KMC handles it by making the kernel a process manager — agents request process lifecycle operations, the kernel executes them with resource controls and per-agent isolation.

---

## Current State

- HAL process driver can list and kill system processes (`crates/agentos-hal/src/drivers/process.rs`)
- Shell-exec runs commands synchronously via bwrap (`crates/agentos-tools/src/shell_exec.rs`)
- No concept of agent-owned managed processes
- No cgroup integration for resource limits (only rlimits in sandbox executor)
- `process_manager` tool exists but only wraps HAL for monitoring

## Target State

- `ProcessProvider` implements `CapabilityProvider` for domain `"proc"`
- Per-agent process table tracking spawned processes
- cgroup v2 integration for memory, CPU, PID limits per process
- Binary allowlist per trust tier
- Process output streaming via event bus
- Automatic cleanup on task completion or agent disconnect

---

## Detailed Subtasks

### 1. Define managed process model

**File:** `crates/agentos-kernel/src/managed_process.rs` (new)

```rust
use agentos_types::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A process managed by the kernel on behalf of an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedProcess {
    pub process_id: String,
    pub agent_id: AgentID,
    pub task_id: TaskID,
    pub binary: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    pub resource_limits: ProcessLimits,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub exited_at: Option<chrono::DateTime<chrono::Utc>>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ProcessStatus {
    Starting,
    Running,
    Stopped,
    Failed,
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessLimits {
    pub memory_max_bytes: u64,      // cgroup memory.max
    pub cpu_quota_us: u64,          // cgroup cpu.max (quota)
    pub cpu_period_us: u64,         // cgroup cpu.max (period)
    pub pids_max: u32,              // cgroup pids.max
    pub timeout_secs: Option<u64>,  // wall-clock timeout
}
```

### 2. Per-agent process table

**File:** `crates/agentos-kernel/src/managed_process.rs`

```rust
/// Tracks all managed processes, keyed by agent.
pub struct ProcessTable {
    /// agent_id -> process_id -> ManagedProcess
    processes: HashMap<AgentID, HashMap<String, ManagedProcess>>,
    /// Maximum processes per agent
    max_per_agent: usize,
}
```

Key operations:
- `spawn()` — create process, add to table, start cgroup
- `signal(process_id, signal)` — send signal to agent's own process only
- `output(process_id, limit)` — read recent stdout/stderr (ring buffer)
- `list()` — list agent's own processes only
- `cleanup(agent_id)` — kill all processes for agent (on disconnect/task end)

### 3. cgroup v2 integration

**File:** `crates/agentos-sandbox/src/cgroup.rs` (new)

```rust
/// Create a cgroup v2 scope for a managed process.
///
/// Requires cgroup v2 delegation to the agentos user (typically via systemd).
/// Falls back to rlimits if cgroup creation fails.
pub fn create_cgroup(
    agent_id: &AgentID,
    process_id: &str,
    limits: &ProcessLimits,
) -> Result<CgroupHandle, AgentOSError> {
    let cgroup_path = format!(
        "/sys/fs/cgroup/agentos/{}/{}",
        agent_id, process_id
    );
    // 1. mkdir -p cgroup_path
    // 2. Write memory.max
    // 3. Write cpu.max as "quota period"
    // 4. Write pids.max
    // Return handle for cleanup
}

/// Move a PID into a cgroup scope.
pub fn attach_pid(handle: &CgroupHandle, pid: u32) -> Result<(), AgentOSError>;

/// Remove a cgroup scope (after all processes exit).
pub fn cleanup_cgroup(handle: &CgroupHandle) -> Result<(), AgentOSError>;
```

Fallback: If `/sys/fs/cgroup/agentos/` doesn't exist or is not writable (no delegation), log a warning and use rlimits instead. The system should work either way, just with weaker isolation.

### 4. Binary allowlist

**File:** `config/default.toml` (add section)

```toml
[capabilities.proc]
# Maximum managed processes per agent
max_processes_per_agent = 8

# Default resource limits per process
default_memory_max_bytes = 536_870_912  # 512 MB
default_cpu_quota_us = 100_000          # 100ms per 100ms period (1 core)
default_pids_max = 64
default_timeout_secs = 3600             # 1 hour

# Binary allowlist — agents can only spawn these binaries.
# "*" means allow all (not recommended for production).
allowed_binaries = [
    "python", "python3", "pip", "pip3",
    "node", "npm", "npx",
    "cargo", "rustc",
    "git", "make", "cmake",
    "sh", "bash",
    "curl", "wget",
    "cat", "ls", "grep", "find", "wc", "sort", "head", "tail",
]

# Binaries that are NEVER allowed (takes precedence over allowlist).
denied_binaries = [
    "sudo", "su", "passwd", "chown", "chmod",
    "mount", "umount", "fdisk", "mkfs",
    "systemctl", "journalctl", "init",
    "iptables", "nft",
    "rm",  # use file-delete tool instead (audited)
]
```

### 5. Implement `ProcessProvider`

Actions:
- **`spawn`** — Start a managed process:
  1. Validate binary against allowlist
  2. Check `proc.spawn:x` permission
  3. Check per-agent process limit
  4. Create cgroup (or rlimit fallback)
  5. Spawn process via `tokio::process::Command`
  6. Attach PID to cgroup
  7. Start output capture (ring buffer, 1MB)
  8. Register in process table
  9. Audit: `ProcessSpawned`

- **`signal`** — Send signal to process:
  1. Verify process belongs to requesting agent
  2. Send signal (SIGTERM, SIGKILL, SIGHUP)
  3. Audit: `ProcessSignaled`

- **`output`** — Read recent output:
  1. Verify process belongs to requesting agent
  2. Return last N lines from ring buffer

- **`list`** — List agent's processes with status and resource usage

- **`wait`** — Block until process exits (with timeout)

### 6. Automatic cleanup

Wire process cleanup into existing lifecycle hooks:
- `HookEvent::TaskEnd` — kill all processes spawned by that task
- Agent disconnect — kill all processes for that agent
- Kernel shutdown — kill all managed processes

### 7. Convenience tools and manifests

- `proc-spawn` — `{ "binary": "python", "args": ["-m", "http.server", "8080"], "working_dir": "/workspace" }`
- `proc-signal` — `{ "process_id": "...", "signal": "SIGTERM" }`
- `proc-output` — `{ "process_id": "...", "lines": 50 }`
- `proc-list` — List agent's processes
- `proc-wait` — `{ "process_id": "...", "timeout_secs": 30 }`

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/managed_process.rs` | NEW — `ManagedProcess`, `ProcessTable`, `ProcessProvider` |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod managed_process;` |
| `crates/agentos-kernel/src/kernel.rs` | Register `ProcessProvider`, wire cleanup to shutdown |
| `crates/agentos-sandbox/src/cgroup.rs` | NEW — cgroup v2 create/attach/cleanup |
| `crates/agentos-sandbox/src/lib.rs` | Add `pub mod cgroup;` |
| `crates/agentos-tools/src/process_tools.rs` | NEW — 5 convenience tools |
| `crates/agentos-tools/src/factory.rs` | Register process tools |
| `config/default.toml` | Add `[capabilities.proc]` section |
| `tools/core/proc-*.toml` | NEW — 5 manifests |

---

## Dependencies

- **Requires:** Phase 1 (capability provider trait)
- **Blocks:** Nothing directly (Phase 6 benefits but doesn't require)

---

## Test Plan

- [ ] `proc.spawn` creates process with correct cgroup limits
- [ ] `proc.spawn` rejects binaries not on allowlist
- [ ] `proc.spawn` rejects denied binaries even if on allowlist
- [ ] `proc.spawn` enforces per-agent process limit
- [ ] `proc.signal` only works on agent's own processes
- [ ] `proc.signal` with SIGTERM → process stops gracefully
- [ ] `proc.output` returns correct recent output
- [ ] `proc.list` shows only the requesting agent's processes
- [ ] `proc.wait` returns exit code when process finishes
- [ ] `proc.wait` returns timeout error after deadline
- [ ] cgroup memory limit kills OOM process
- [ ] cgroup PID limit prevents fork bombs
- [ ] Fallback to rlimits when cgroups unavailable
- [ ] Task end cleanup kills all task processes
- [ ] Agent disconnect cleanup kills all agent processes
- [ ] Audit events: `ProcessSpawned`, `ProcessSignaled`, `ProcessTerminated`

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- managed_process
cargo test -p agentos-sandbox -- cgroup
cargo test -p agentos-tools -- process_tools
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Related

- [[01-capability-provider-trait]] — prerequisite
- [[Kernel Mediated Capabilities Plan]]
