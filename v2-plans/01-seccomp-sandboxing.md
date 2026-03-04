# Plan 01 — seccomp Sandboxing (`agentos-sandbox` crate)

## Goal

Implement process-level tool sandboxing using Linux seccomp-BPF (Berkeley Packet Filter) syscall filtering. In V1, tools run in-process (same binary). In V2, each tool execution is forked into a **child process** with a restrictive seccomp profile derived from the tool's manifest.

## Dependencies

- `agentos-types`
- `agentos-tools` (for `ToolManifest` sandbox config)
- `seccompiler` — AWS Firecracker's seccomp library (pure Rust, well-maintained)
- `nix` — Unix system call wrappers
- `tokio` — async child process management

## New Dependency

```toml
# Add to workspace Cargo.toml
seccompiler = "0.4"
nix = { version = "0.29", features = ["process", "signal"] }
```

## Architecture

```
Kernel receives tool execution request
    │
    ▼
ToolSandbox::spawn(manifest, payload, context)
    │
    ├── 1. Fork child process
    ├── 2. Apply seccomp-BPF filter based on manifest [sandbox] section
    ├── 3. Drop capabilities (no_new_privs)
    ├── 4. Set resource limits (memory, CPU time)
    ├── 5. Execute tool logic in isolated process
    ├── 6. Capture stdout as JSON result
    ├── 7. Kill on timeout
    └── 8. Return result to kernel
```

## Sandbox Profile Derivation

The tool manifest's `[sandbox]` section drives the seccomp profile:

```toml
[sandbox]
network       = false    # → block socket(), connect(), sendto(), recvfrom()
fs_write      = false    # → block write() to files outside data_dir
gpu           = false    # → block ioctl() for GPU devices
max_memory_mb = 64       # → setrlimit(RLIMIT_AS)
max_cpu_ms    = 5000     # → setrlimit(RLIMIT_CPU) + wall-clock timeout
syscalls      = ["read", "write", "mmap", "close", "exit_group"]  # explicit allowlist
```

## Core Struct: `ToolSandbox`

```rust
use std::path::PathBuf;
use std::time::Duration;

pub struct ToolSandbox {
    /// Working directory for tool execution (data_dir).
    data_dir: PathBuf,
}

pub struct SandboxConfig {
    pub allow_network: bool,
    pub allow_fs_write: bool,
    pub allow_gpu: bool,
    pub max_memory_bytes: u64,
    pub max_cpu_ms: u64,
    pub allowed_syscalls: Vec<String>,
}

pub struct SandboxResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub wall_time_ms: u64,
    pub was_killed: bool,
}

impl ToolSandbox {
    pub fn new(data_dir: PathBuf) -> Self;

    /// Derive a SandboxConfig from a tool manifest.
    pub fn config_from_manifest(manifest: &ToolManifest) -> SandboxConfig;

    /// Build a seccomp-BPF filter from a SandboxConfig.
    fn build_seccomp_filter(config: &SandboxConfig) -> Result<BpfProgram, AgentOSError>;

    /// Spawn a tool in an isolated process with seccomp applied.
    /// Returns the tool's JSON output or an error.
    pub async fn spawn(
        &self,
        tool_name: &str,
        payload: serde_json::Value,
        config: &SandboxConfig,
        timeout: Duration,
    ) -> Result<serde_json::Value, AgentOSError>;
}
```

## Default Syscall Allowlist

The base allowlist for ALL tools (minimum required for a Rust binary to function):

```
read, write, close, fstat, mmap, mprotect, munmap, brk,
rt_sigaction, rt_sigprocmask, exit_group, arch_prctl,
clock_gettime, nanosleep, getrandom, futex, sched_yield
```

Tools requesting `network = true` additionally get: `socket, connect, sendto, recvfrom, bind, listen, accept, setsockopt, getsockopt`

## Integration with Kernel

The kernel's `execute_tool_call` method is updated to use `ToolSandbox` instead of calling tools in-process:

```rust
// Before (V1 — in-process):
let result = self.tool_runner.execute(tool_name, payload, context).await?;

// After (V2 — sandboxed):
let sandbox_config = ToolSandbox::config_from_manifest(&manifest);
let result = self.sandbox.spawn(tool_name, payload, &sandbox_config, timeout).await?;
```

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_manifest_network_blocked() {
        let manifest = /* manifest with network = false */;
        let config = ToolSandbox::config_from_manifest(&manifest);
        assert!(!config.allow_network);
    }

    #[test]
    fn test_default_syscall_allowlist_includes_basics() {
        let config = SandboxConfig::default();
        let filter = ToolSandbox::build_seccomp_filter(&config).unwrap();
        // filter should allow read, write, close, exit_group
    }

    #[tokio::test]
    async fn test_sandboxed_tool_returns_result() {
        let sandbox = ToolSandbox::new(temp_dir());
        let result = sandbox.spawn(
            "data-parser",
            json!({"data": "{\"a\":1}", "format": "json"}),
            &SandboxConfig::default(),
            Duration::from_secs(5),
        ).await.unwrap();
        assert!(result.get("parsed").is_some());
    }

    #[tokio::test]
    async fn test_sandboxed_tool_killed_on_timeout() {
        // spawn a tool that sleeps forever
        // verify it's killed after timeout
    }
}
```

## Verification

```bash
cargo test -p agentos-sandbox
cargo test -p agentos-kernel  # verify integration
```

> [!NOTE]
> seccomp-BPF is Linux-only. On macOS/Windows development machines, the sandbox gracefully falls back to process isolation without seccomp filtering. The fallback is logged as a warning.
