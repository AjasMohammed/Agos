//! Managed Processes capability provider (`proc.*`).
//!
//! Allows agents to spawn, monitor, signal, and manage long-running processes
//! with per-agent process tables, resource limits, and binary allowlists.
//!
//! Processes are tracked per-agent and automatically cleaned up on task
//! completion or agent disconnect.

use crate::capability_provider::{CapabilityContext, CapabilityProvider, CapabilityResult};
use crate::managed_env::{activated_env, WorkspaceInfo, WorkspaceResolver};
use agentos_types::{AgentID, AgentOSError, PermissionOp, TaskID};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Status of a managed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Stopped,
    Failed,
    Killed,
}

/// Resource limits for a managed process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessLimits {
    /// Maximum memory in bytes (applied via rlimit).
    pub memory_max_bytes: u64,
    /// Maximum number of child processes.
    pub pids_max: u32,
    /// Wall-clock timeout in seconds (None = no timeout).
    pub timeout_secs: Option<u64>,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            memory_max_bytes: 536_870_912, // 512 MB
            pids_max: 64,
            timeout_secs: Some(3600), // 1 hour
        }
    }
}

/// A process managed by the kernel on behalf of an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedProcessInfo {
    pub process_id: String,
    pub agent_id: AgentID,
    pub task_id: TaskID,
    pub binary: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    pub limits: ProcessLimits,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub exited_at: Option<chrono::DateTime<chrono::Utc>>,
    pub exit_code: Option<i32>,
}

/// Internal handle for a running process (not serializable).
struct RunningProcess {
    info: ManagedProcessInfo,
    /// Ring buffer for captured output (stdout + stderr merged).
    output_buffer: std::collections::VecDeque<String>,
    /// Max lines to keep in the ring buffer.
    output_max_lines: usize,
}

impl RunningProcess {
    fn append_output(&mut self, line: String) {
        if self.output_buffer.len() >= self.output_max_lines {
            self.output_buffer.pop_front(); // O(1) unlike Vec::remove(0)
        }
        self.output_buffer.push_back(line);
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the managed process capability.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcessConfig {
    /// Maximum managed processes per agent.
    #[serde(default = "default_max_procs")]
    pub max_processes_per_agent: usize,
    /// Default resource limits per process.
    #[serde(default)]
    pub default_limits: ProcessLimits,
    /// Binary allowlist — agents can only spawn these. Empty = allow all.
    #[serde(default = "default_allowed_binaries")]
    pub allowed_binaries: Vec<String>,
    /// Denied binaries — NEVER allowed, takes precedence.
    #[serde(default = "default_denied_binaries")]
    pub denied_binaries: Vec<String>,
    /// Max lines in output ring buffer per process.
    #[serde(default = "default_output_lines")]
    pub output_buffer_lines: usize,
}

fn default_max_procs() -> usize {
    8
}

fn default_allowed_binaries() -> Vec<String> {
    vec![
        "python", "python3", "pip", "pip3", "node", "npm", "npx", "cargo", "rustc", "git", "make",
        "cmake", "sh", "bash", "curl", "wget", "cat", "ls", "grep", "find", "wc", "sort", "head",
        "tail",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn default_denied_binaries() -> Vec<String> {
    vec![
        "sudo",
        "su",
        "passwd",
        "chown",
        "chmod",
        "mount",
        "umount",
        "fdisk",
        "mkfs",
        "systemctl",
        "journalctl",
        "init",
        "iptables",
        "nft",
        "rm",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn default_output_lines() -> usize {
    500
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            max_processes_per_agent: default_max_procs(),
            default_limits: ProcessLimits::default(),
            allowed_binaries: default_allowed_binaries(),
            denied_binaries: default_denied_binaries(),
            output_buffer_lines: default_output_lines(),
        }
    }
}

// ---------------------------------------------------------------------------
// Binary validation
// ---------------------------------------------------------------------------

fn validate_binary(
    binary: &str,
    allowed: &[String],
    denied: &[String],
) -> Result<(), AgentOSError> {
    // Extract the base name from the binary path
    let base = std::path::Path::new(binary)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(binary);

    // SECURITY: deny list takes absolute precedence.
    if denied.iter().any(|d| d == base || d == binary) {
        return Err(AgentOSError::PermissionDenied {
            resource: "proc.spawn".into(),
            operation: format!("binary '{binary}' is on the denied list"),
        });
    }

    // If allowlist is non-empty, binary must be on it.
    if !allowed.is_empty() && !allowed.iter().any(|a| a == base || a == binary) {
        return Err(AgentOSError::PermissionDenied {
            resource: "proc.spawn".into(),
            operation: format!(
                "binary '{binary}' is not on the allowed list; \
                 allowed: {}",
                allowed.join(", ")
            ),
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Process table
// ---------------------------------------------------------------------------

/// Callback invoked when a managed process exits abnormally
/// (status [`ProcessStatus::Failed`] or [`ProcessStatus::Killed`]).
///
/// Receives a snapshot of the process info at exit time. The kernel uses this
/// to emit a `ProcessCrashed` event without `managed_process` having to depend
/// on `EventBus`. Callbacks must be non-blocking — long work should be
/// `tokio::spawn`ed inside the callback.
pub type ProcessCrashCallback = Arc<dyn Fn(ManagedProcessInfo) + Send + Sync>;

/// Shared process table for all agents.
#[derive(Clone)]
pub struct ProcessTable {
    inner: Arc<RwLock<ProcessTableInner>>,
    config: ProcessConfig,
}

struct ProcessTableInner {
    /// process_id -> RunningProcess
    processes: HashMap<String, RunningProcess>,
    next_id: u64,
    /// Set via [`ProcessTable::set_crash_callback`]. Fired from
    /// [`ProcessTable::mark_exited`] when a process exits with
    /// `Failed` or `Killed` status.
    crash_callback: Option<ProcessCrashCallback>,
}

impl ProcessTable {
    pub fn new(config: ProcessConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ProcessTableInner {
                processes: HashMap::new(),
                next_id: 1,
                crash_callback: None,
            })),
            config,
        }
    }

    /// Install a callback fired whenever a managed process exits with
    /// abnormal status. Replaces any previously installed callback.
    pub async fn set_crash_callback(&self, cb: ProcessCrashCallback) {
        let mut inner = self.inner.write().await;
        inner.crash_callback = Some(cb);
    }

    async fn get_info(&self, process_id: &str, agent_id: &AgentID) -> Option<ManagedProcessInfo> {
        let inner = self.inner.read().await;
        inner
            .processes
            .get(process_id)
            .filter(|p| p.info.agent_id == *agent_id)
            .map(|p| p.info.clone())
    }

    async fn list_for_agent(&self, agent_id: &AgentID) -> Vec<ManagedProcessInfo> {
        let inner = self.inner.read().await;
        inner
            .processes
            .values()
            .filter(|p| p.info.agent_id == *agent_id)
            .map(|p| p.info.clone())
            .collect()
    }

    async fn get_output(
        &self,
        process_id: &str,
        agent_id: &AgentID,
        lines: usize,
    ) -> Option<Vec<String>> {
        let inner = self.inner.read().await;
        inner
            .processes
            .get(process_id)
            .filter(|p| p.info.agent_id == *agent_id)
            .map(|p| {
                let buf = &p.output_buffer;
                let skip = buf.len().saturating_sub(lines);
                buf.iter().skip(skip).cloned().collect()
            })
    }

    async fn mark_exited(&self, process_id: &str, exit_code: i32, status: ProcessStatus) {
        // Update under the write lock; collect the crash-callback inputs while
        // we still hold it so we can fire after releasing — keeping callback
        // execution out of the critical section.
        let crash_fire = {
            let mut inner = self.inner.write().await;
            let info_snapshot = inner.processes.get_mut(process_id).map(|proc| {
                proc.info.status = status;
                proc.info.exit_code = Some(exit_code);
                proc.info.exited_at = Some(chrono::Utc::now());
                proc.info.clone()
            });
            match (
                info_snapshot,
                matches!(status, ProcessStatus::Failed | ProcessStatus::Killed),
            ) {
                (Some(info), true) => inner
                    .crash_callback
                    .as_ref()
                    .map(|cb| (Arc::clone(cb), info)),
                _ => None,
            }
        };
        if let Some((cb, info)) = crash_fire {
            cb(info);
        }
    }

    async fn append_output(&self, process_id: &str, line: String) {
        let mut inner = self.inner.write().await;
        if let Some(proc) = inner.processes.get_mut(process_id) {
            proc.append_output(line);
        }
    }

    /// Kill all processes for an agent. Returns the count killed.
    ///
    /// Uses write lock to both send signals and update process status atomically,
    /// preventing stale `Running` status from blocking subsequent spawn attempts.
    pub async fn cleanup_agent(&self, agent_id: &AgentID) -> usize {
        let mut inner = self.inner.write().await;
        let mut killed = 0;
        for proc in inner.processes.values_mut() {
            if proc.info.agent_id == *agent_id && proc.info.status == ProcessStatus::Running {
                if let Some(pid) = proc.info.pid {
                    #[cfg(unix)]
                    {
                        // Safety: libc::kill with a valid PID and signal.
                        // PID reuse risk is accepted — there is no way to avoid
                        // it without storing the Child handle (future improvement).
                        unsafe {
                            libc::kill(pid as i32, libc::SIGKILL);
                        }
                    }
                    proc.info.status = ProcessStatus::Killed;
                    proc.info.exited_at = Some(chrono::Utc::now());
                    killed += 1;
                }
            }
        }
        killed
    }

    /// Remove exited process entries older than `max_age`. Returns count removed.
    ///
    /// Prevents unbounded growth of the process table from long-running kernels.
    pub async fn sweep_exited(&self, max_age: chrono::Duration) -> usize {
        let mut inner = self.inner.write().await;
        let cutoff = chrono::Utc::now() - max_age;
        let before = inner.processes.len();
        inner.processes.retain(|_, p| {
            p.info.status == ProcessStatus::Running
                || p.info.exited_at.map(|e| e > cutoff).unwrap_or(true)
        });
        before - inner.processes.len()
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new(ProcessConfig::default())
    }
}

// ---------------------------------------------------------------------------
// ProcessProvider
// ---------------------------------------------------------------------------

/// Managed processes capability provider.
pub struct ProcessProvider {
    table: ProcessTable,
    /// Optional resolver for `workspace` parameters on `proc-spawn`. When set,
    /// the spawn flow looks up the workspace, resolves binaries inside it
    /// (venv/bin → node_modules/.bin → bin → system PATH), and activates
    /// the workspace env vars before exec.
    workspace_resolver: Option<Arc<dyn WorkspaceResolver>>,
}

impl ProcessProvider {
    pub fn new(table: ProcessTable) -> Self {
        Self {
            table,
            workspace_resolver: None,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(ProcessTable::default())
    }

    /// Attach a workspace resolver. Wired at kernel boot to the shared
    /// `EnvProvider`.
    pub fn with_resolver(
        table: ProcessTable,
        workspace_resolver: Arc<dyn WorkspaceResolver>,
    ) -> Self {
        Self {
            table,
            workspace_resolver: Some(workspace_resolver),
        }
    }

    /// Set a workspace resolver post-construction. Used at kernel boot where
    /// the ProcessTable is constructed before the EnvProvider.
    pub fn set_workspace_resolver(&mut self, resolver: Arc<dyn WorkspaceResolver>) {
        self.workspace_resolver = Some(resolver);
    }

    /// Get a reference to the process table for sharing.
    pub fn table(&self) -> &ProcessTable {
        &self.table
    }
}

/// Resolve a binary name against the workspace's local bin dirs.
///
/// Order matches `activated_env`'s `PATH`: venv/bin → node_modules/.bin →
/// bin. Returns `None` if no match is an executable regular file confined
/// to the workspace tree.
///
/// SECURITY: the lookup is bounded to the workspace in two layers:
/// 1. `binary` must not contain path separators or `..` segments. This blocks
///    inputs like `"../../../bin/sh"` that would otherwise traverse out via
///    `PathBuf::join`.
/// 2. The resolved path is canonicalized and its canonical form must start
///    with the canonical workspace root. This blocks symlinks that a
///    package install (pip/npm post-install scripts) might drop to point
///    out of the workspace at e.g. `/bin/rm`.
fn resolve_workspace_binary(ws: &WorkspaceInfo, binary: &str) -> Option<PathBuf> {
    // Layer 1: reject any binary name that could traverse via the join.
    if binary.is_empty()
        || binary.contains('/')
        || binary.contains('\\')
        || binary.contains('\0')
        || binary.split(['/', '\\']).any(|seg| seg == "..")
    {
        return None;
    }

    let canonical_root = std::fs::canonicalize(&ws.root).ok()?;

    for sub in ["venv/bin", "node_modules/.bin", "bin"] {
        let p = ws.root.join(sub).join(binary);
        if !p.is_file() || !is_executable(&p) {
            continue;
        }
        // Layer 2: canonicalize-and-prefix-check defeats symlink escapes.
        let Ok(canonical) = std::fs::canonicalize(&p) else {
            continue;
        };
        if !canonical.starts_with(&canonical_root) {
            tracing::warn!(
                workspace = %ws.root.display(),
                binary = binary,
                resolved = %canonical.display(),
                "rejected workspace binary that resolved outside the workspace tree (symlink escape attempt?)"
            );
            continue;
        }
        return Some(canonical);
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.exists()
}

impl ProcessProvider {
    async fn action_spawn(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let binary = params["binary"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'binary' field".into()))?;

        let args: Vec<String> = params["args"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Optional workspace name. When set, the binary is resolved against
        // {ws}/venv/bin → {ws}/node_modules/.bin → {ws}/bin, the workspace
        // env (VIRTUAL_ENV + PATH prepends) is activated for the child, and
        // working_dir defaults to the workspace root.
        let workspace_name = params["workspace"].as_str();
        let workspace_info = match (workspace_name, self.workspace_resolver.as_ref()) {
            (Some(name), Some(resolver)) => resolver.resolve(context.agent_id, name).await,
            _ => None,
        };
        if workspace_name.is_some() && workspace_info.is_none() {
            return Err(AgentOSError::KernelError {
                reason: format!(
                    "workspace '{}' not found for this agent; create it with env-create first",
                    workspace_name.unwrap_or("")
                ),
            });
        }

        let working_dir = params["working_dir"]
            .as_str()
            .map(PathBuf::from)
            .or_else(|| workspace_info.as_ref().map(|w| w.root.clone()))
            .unwrap_or_else(|| context.data_dir.clone());

        // SECURITY: validate working_dir is within agent's scope.
        if !working_dir.starts_with(&context.data_dir)
            && !context
                .workspace_paths
                .iter()
                .any(|wp| working_dir.starts_with(wp))
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "proc.spawn".into(),
                operation: format!(
                    "working_dir '{}' is outside agent scope",
                    working_dir.display()
                ),
            });
        }

        // Resolve the binary. With a workspace, prefer the workspace's local
        // bin dirs and bypass the global binary allowlist (the workspace is
        // the sandbox). Without a workspace, keep the original "no path-based
        // binaries + allowlist" rules.
        let (resolved_binary, in_workspace) = if let Some(ws) = workspace_info.as_ref() {
            match resolve_workspace_binary(ws, binary) {
                Some(path) => (path.to_string_lossy().into_owned(), true),
                None => {
                    // Fall back to PATH lookup, but still forbid path-based binaries.
                    if binary.contains('/') || binary.contains('\\') {
                        return Err(AgentOSError::PermissionDenied {
                            resource: "proc.spawn".into(),
                            operation: format!(
                                "binary '{}' not found in workspace '{}' and absolute paths are not allowed",
                                binary,
                                ws.root.display()
                            ),
                        });
                    }
                    (binary.to_string(), false)
                }
            }
        } else {
            if binary.contains('/') || binary.contains('\\') {
                return Err(AgentOSError::PermissionDenied {
                    resource: "proc.spawn".into(),
                    operation:
                        "binary must be a bare name (no paths); system PATH is used for resolution"
                            .into(),
                });
            }
            (binary.to_string(), false)
        };

        // SECURITY: skip the global allowlist only when the binary was
        // resolved inside the workspace tree (fail-closed everywhere else).
        if !in_workspace {
            validate_binary(
                binary,
                &self.table.config.allowed_binaries,
                &self.table.config.denied_binaries,
            )?;
        }

        let limits = self.table.config.default_limits.clone();

        // Spawn the process first (we need the PID for the table entry).
        let mut cmd = tokio::process::Command::new(&resolved_binary);
        cmd.args(&args)
            .current_dir(&working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(ws) = workspace_info.as_ref() {
            cmd.env_clear();
            for (k, v) in activated_env(ws) {
                cmd.env(k, v);
            }
        }

        let mut child = cmd.spawn().map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "proc-spawn".into(),
            reason: format!("failed to spawn '{resolved_binary}': {e}"),
        })?;

        let pid = child.id();

        // Atomically check process limit, generate ID, and insert under a
        // single write lock — prevents TOCTOU race on the limit check.
        // If the limit is exceeded, kill the just-spawned child.
        let process_id = {
            let mut inner = self.table.inner.write().await;
            let count = inner
                .processes
                .values()
                .filter(|p| {
                    p.info.agent_id == context.agent_id && p.info.status == ProcessStatus::Running
                })
                .count();
            if count >= self.table.config.max_processes_per_agent {
                // Kill the child we just spawned — it exceeds the limit.
                let _ = child.kill().await;
                return Err(AgentOSError::KernelError {
                    reason: format!(
                        "agent has reached the maximum of {} managed processes",
                        self.table.config.max_processes_per_agent
                    ),
                });
            }
            let proc_id = format!("proc-{}", inner.next_id);
            inner.next_id += 1;

            let info = ManagedProcessInfo {
                process_id: proc_id.clone(),
                agent_id: context.agent_id,
                task_id: context.task_id,
                binary: resolved_binary.clone(),
                args: args.clone(),
                working_dir: working_dir.clone(),
                pid,
                status: ProcessStatus::Running,
                limits: limits.clone(),
                started_at: chrono::Utc::now(),
                exited_at: None,
                exit_code: None,
            };

            let running = RunningProcess {
                info,
                output_buffer: std::collections::VecDeque::new(),
                output_max_lines: self.table.config.output_buffer_lines,
            };

            inner.processes.insert(proc_id.clone(), running);
            proc_id
        };

        // Spawn background tasks to capture output and detect exit.
        let table_clone = self.table.clone();
        let proc_id_clone = process_id.clone();
        let timeout = limits.timeout_secs;

        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            // Capture stdout
            if let Some(stdout) = stdout {
                let reader = tokio::io::BufReader::new(stdout);
                let mut lines = reader.lines();
                let t = table_clone.clone();
                let pid = proc_id_clone.clone();
                tokio::spawn(async move {
                    while let Ok(Some(line)) = lines.next_line().await {
                        t.append_output(&pid, line).await;
                    }
                });
            }

            // Capture stderr
            if let Some(stderr) = stderr {
                let reader = tokio::io::BufReader::new(stderr);
                let mut lines = reader.lines();
                let t = table_clone.clone();
                let pid = proc_id_clone.clone();
                tokio::spawn(async move {
                    while let Ok(Some(line)) = lines.next_line().await {
                        t.append_output(&pid, format!("[stderr] {line}")).await;
                    }
                });
            }

            // Wait for process exit with optional timeout.
            let exit_result = if let Some(timeout_secs) = timeout {
                let duration = std::time::Duration::from_secs(timeout_secs);
                match tokio::time::timeout(duration, child.wait()).await {
                    Ok(Ok(status)) => {
                        let code = status.code().unwrap_or(-1);
                        let proc_status = if status.success() {
                            ProcessStatus::Stopped
                        } else {
                            ProcessStatus::Failed
                        };
                        (code, proc_status)
                    }
                    Ok(Err(_)) => (-1, ProcessStatus::Failed),
                    Err(_) => {
                        // Timeout — kill the process.
                        let _ = child.kill().await;
                        (-9, ProcessStatus::Killed)
                    }
                }
            } else {
                match child.wait().await {
                    Ok(status) => {
                        let code = status.code().unwrap_or(-1);
                        let proc_status = if status.success() {
                            ProcessStatus::Stopped
                        } else {
                            ProcessStatus::Failed
                        };
                        (code, proc_status)
                    }
                    Err(_) => (-1, ProcessStatus::Failed),
                }
            };

            table_clone
                .mark_exited(&proc_id_clone, exit_result.0, exit_result.1)
                .await;
        });

        Ok(CapabilityResult {
            output: json!({
                "process_id": process_id,
                "binary": resolved_binary,
                "args": args,
                "pid": pid,
                "status": "running",
                "workspace": workspace_name,
                "resolved_via": if in_workspace { "workspace" } else { "path" },
            }),
            audit_metadata: json!({
                "event": "ManagedProcessSpawned",
                "process_id": process_id,
                "binary": resolved_binary,
                "pid": pid,
                "agent_id": context.agent_id.to_string(),
                "workspace": workspace_name,
                "resolved_via": if in_workspace { "workspace" } else { "path" },
            }),
        })
    }

    async fn action_signal(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let process_id = params["process_id"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'process_id' field".into()))?;

        let signal_name = params["signal"].as_str().unwrap_or("SIGTERM");

        let info = self
            .table
            .get_info(process_id, &context.agent_id)
            .await
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("process '{process_id}' not found or not owned by this agent"),
            })?;

        let pid = info.pid.ok_or_else(|| AgentOSError::KernelError {
            reason: "process has no PID (never started)".into(),
        })?;

        #[cfg(unix)]
        {
            let sig = match signal_name.to_ascii_uppercase().as_str() {
                "SIGTERM" | "TERM" | "15" => libc::SIGTERM,
                "SIGKILL" | "KILL" | "9" => libc::SIGKILL,
                "SIGHUP" | "HUP" | "1" => libc::SIGHUP,
                "SIGINT" | "INT" | "2" => libc::SIGINT,
                other => {
                    return Err(AgentOSError::SchemaValidation(format!(
                        "unsupported signal '{other}': use SIGTERM, SIGKILL, SIGHUP, or SIGINT"
                    )));
                }
            };

            let result = unsafe { libc::kill(pid as i32, sig) };
            if result != 0 {
                let errno = std::io::Error::last_os_error();
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "proc-signal".into(),
                    reason: format!("kill({pid}, {signal_name}) failed: {errno}"),
                });
            }
        }

        #[cfg(not(unix))]
        {
            return Err(AgentOSError::KernelError {
                reason: "process signaling is only supported on Unix platforms".into(),
            });
        }

        Ok(CapabilityResult {
            output: json!({
                "signaled": process_id,
                "signal": signal_name,
                "pid": pid,
            }),
            audit_metadata: json!({
                "event": "ManagedProcessSignaled",
                "process_id": process_id,
                "signal": signal_name,
                "pid": pid,
            }),
        })
    }

    async fn action_output(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let process_id = params["process_id"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'process_id' field".into()))?;

        let lines = params["lines"].as_u64().unwrap_or(50) as usize;

        let output = self
            .table
            .get_output(process_id, &context.agent_id, lines)
            .await
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("process '{process_id}' not found or not owned by this agent"),
            })?;

        Ok(CapabilityResult {
            output: json!({
                "process_id": process_id,
                "lines": output,
                "count": output.len(),
            }),
            audit_metadata: json!({
                "action": "output",
                "process_id": process_id,
            }),
        })
    }

    async fn action_list(
        &self,
        _params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let processes = self.table.list_for_agent(&context.agent_id).await;
        let entries: Vec<Value> = processes
            .iter()
            .map(|p| {
                json!({
                    "process_id": p.process_id,
                    "binary": p.binary,
                    "args": p.args,
                    "pid": p.pid,
                    "status": serde_json::to_value(p.status).unwrap_or(Value::Null),
                    "started_at": p.started_at.to_rfc3339(),
                    "exit_code": p.exit_code,
                })
            })
            .collect();

        Ok(CapabilityResult {
            output: json!({
                "processes": entries,
                "count": entries.len(),
            }),
            audit_metadata: json!({
                "action": "list",
                "count": entries.len(),
            }),
        })
    }

    async fn action_wait(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let process_id = params["process_id"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'process_id' field".into()))?;

        let timeout_secs = params["timeout_secs"].as_u64().unwrap_or(30);

        // Poll for process exit.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        loop {
            let info = self
                .table
                .get_info(process_id, &context.agent_id)
                .await
                .ok_or_else(|| AgentOSError::KernelError {
                    reason: format!("process '{process_id}' not found or not owned by this agent"),
                })?;

            if info.status != ProcessStatus::Running {
                return Ok(CapabilityResult {
                    output: json!({
                        "process_id": process_id,
                        "status": serde_json::to_value(info.status).unwrap_or(Value::Null),
                        "exit_code": info.exit_code,
                    }),
                    audit_metadata: json!({
                        "action": "wait",
                        "process_id": process_id,
                        "exit_code": info.exit_code,
                    }),
                });
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "proc-wait".into(),
                    reason: format!("process '{process_id}' did not exit within {timeout_secs}s"),
                });
            }

            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}

#[async_trait]
impl CapabilityProvider for ProcessProvider {
    fn domain(&self) -> &str {
        "proc"
    }

    fn supported_actions(&self) -> &[&str] {
        &["spawn", "signal", "output", "list", "wait"]
    }

    fn required_permissions(&self, action: &str) -> Option<Vec<(String, PermissionOp)>> {
        match action {
            "spawn" => Some(vec![("proc.spawn".to_string(), PermissionOp::Execute)]),
            "signal" => Some(vec![("proc.signal".to_string(), PermissionOp::Execute)]),
            "output" => Some(vec![("proc.output".to_string(), PermissionOp::Read)]),
            "list" => Some(vec![("proc.list".to_string(), PermissionOp::Read)]),
            "wait" => Some(vec![("proc.wait".to_string(), PermissionOp::Read)]),
            _ => None,
        }
    }

    async fn execute(
        &self,
        action: &str,
        params: Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        match action {
            "spawn" => self.action_spawn(&params, context).await,
            "signal" => self.action_signal(&params, context).await,
            "output" => self.action_output(&params, context).await,
            "list" => self.action_list(&params, context).await,
            "wait" => self.action_wait(&params, context).await,
            other => Err(AgentOSError::KernelError {
                reason: format!("unknown proc action '{other}'"),
            }),
        }
    }

    fn description(&self) -> &str {
        "Spawn, monitor, signal, and manage agent processes"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, TaskID, TraceID};

    fn make_config() -> ProcessConfig {
        ProcessConfig {
            max_processes_per_agent: 3,
            // Use both bare names and full paths so validation works for either form.
            allowed_binaries: vec![
                "echo".into(),
                "sleep".into(),
                "sh".into(),
                "cat".into(),
                "true".into(),
                "false".into(),
            ],
            denied_binaries: vec!["sudo".into(), "rm".into()],
            ..Default::default()
        }
    }

    fn make_provider() -> ProcessProvider {
        ProcessProvider::new(ProcessTable::new(make_config()))
    }

    fn make_context() -> CapabilityContext {
        CapabilityContext {
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
            // Use /tmp which always exists as working_dir for test processes.
            data_dir: PathBuf::from("/tmp"),
            permissions: agentos_types::PermissionSet::default(),
            workspace_paths: vec![],
        }
    }

    #[test]
    fn provider_metadata() {
        let p = make_provider();
        assert_eq!(p.domain(), "proc");
        assert_eq!(
            p.supported_actions(),
            &["spawn", "signal", "output", "list", "wait"]
        );
        assert!(p.required_permissions("spawn").is_some());
        assert!(p.required_permissions("unknown").is_none());
    }

    #[test]
    fn validate_binary_allowed() {
        let allowed = vec!["python".into(), "node".into()];
        let denied = vec!["sudo".into()];

        assert!(validate_binary("python", &allowed, &denied).is_ok());
        assert!(validate_binary("node", &allowed, &denied).is_ok());
    }

    #[test]
    fn resolve_workspace_binary_prefers_venv() {
        let tmp = tempfile::TempDir::new().unwrap();
        let venv_bin = tmp.path().join("venv").join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        let bin_path = venv_bin.join("pytest");
        std::fs::write(&bin_path, "#!/bin/sh\necho hi").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let ws = crate::managed_env::WorkspaceInfo {
            root: tmp.path().to_path_buf(),
            ecosystem: crate::managed_env::Ecosystem::Python,
        };
        let resolved = resolve_workspace_binary(&ws, "pytest").unwrap();
        assert!(resolved.ends_with("venv/bin/pytest"));
    }

    #[test]
    fn resolve_workspace_binary_returns_none_for_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = crate::managed_env::WorkspaceInfo {
            root: tmp.path().to_path_buf(),
            ecosystem: crate::managed_env::Ecosystem::NodeJs,
        };
        assert!(resolve_workspace_binary(&ws, "nope").is_none());
    }

    #[test]
    fn resolve_workspace_binary_rejects_path_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("venv").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let ws = crate::managed_env::WorkspaceInfo {
            root: tmp.path().to_path_buf(),
            ecosystem: crate::managed_env::Ecosystem::Python,
        };
        // All of these must return None — they could otherwise reach /bin/sh
        // or similar via the OS resolving `..` segments.
        assert!(resolve_workspace_binary(&ws, "../../../bin/sh").is_none());
        assert!(resolve_workspace_binary(&ws, "../sh").is_none());
        assert!(resolve_workspace_binary(&ws, "foo/bar").is_none());
        assert!(resolve_workspace_binary(&ws, "\\foo").is_none());
        assert!(resolve_workspace_binary(&ws, "").is_none());
        assert!(resolve_workspace_binary(&ws, "foo\0bar").is_none());
        // Plain `..` as the whole input is rejected; a name *containing* the
        // characters `..` (like `pip3.10`) must still be accepted as a name.
        assert!(resolve_workspace_binary(&ws, "..").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_workspace_binary_rejects_symlink_escape() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let venv_bin = tmp.path().join("venv").join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();

        // Drop a target outside the workspace so canonicalize succeeds.
        let outside = tempfile::TempDir::new().unwrap();
        let escape_target = outside.path().join("malicious");
        std::fs::write(&escape_target, "#!/bin/sh\necho pwned").unwrap();
        std::fs::set_permissions(&escape_target, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Symlink {ws}/venv/bin/innocuous -> /tmp/outside/malicious
        let link = venv_bin.join("innocuous");
        std::os::unix::fs::symlink(&escape_target, &link).unwrap();

        let ws = crate::managed_env::WorkspaceInfo {
            root: tmp.path().to_path_buf(),
            ecosystem: crate::managed_env::Ecosystem::Python,
        };
        // The symlink itself is_file() and executable, but its canonical
        // target lives outside the workspace — must be rejected.
        assert!(resolve_workspace_binary(&ws, "innocuous").is_none());
    }

    #[test]
    fn resolve_workspace_binary_skips_non_executable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let file = bin.join("plain");
        std::fs::write(&file, "not exec").unwrap();
        // Permissions deliberately not set executable on unix.
        let ws = crate::managed_env::WorkspaceInfo {
            root: tmp.path().to_path_buf(),
            ecosystem: crate::managed_env::Ecosystem::Generic,
        };
        #[cfg(unix)]
        {
            assert!(resolve_workspace_binary(&ws, "plain").is_none());
        }
        #[cfg(not(unix))]
        {
            let _ = (&ws, &file);
        }
    }

    #[test]
    fn validate_binary_denied_takes_precedence() {
        let allowed = vec!["sudo".into()]; // Even on allowlist
        let denied = vec!["sudo".into()];

        assert!(validate_binary("sudo", &allowed, &denied).is_err());
    }

    #[test]
    fn validate_binary_not_on_allowlist() {
        let allowed = vec!["python".into()];
        let denied = vec![];

        let err = validate_binary("unknown-binary", &allowed, &denied).unwrap_err();
        assert!(format!("{err}").contains("not on the allowed list"));
    }

    #[test]
    fn validate_binary_empty_allowlist_allows_all() {
        let allowed = vec![];
        let denied = vec!["sudo".into()];

        assert!(validate_binary("anything", &allowed, &denied).is_ok());
        assert!(validate_binary("sudo", &allowed, &denied).is_err());
    }

    #[test]
    fn validate_binary_path_extracts_basename() {
        // validate_binary still allows paths (basename extraction) — the path
        // rejection happens at the action level in action_spawn.
        let allowed = vec!["python3".into()];
        let denied = vec![];
        assert!(validate_binary("/usr/bin/python3", &allowed, &denied).is_ok());
    }

    #[tokio::test]
    async fn spawn_rejects_path_based_binary() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "spawn",
                json!({"binary": "/usr/bin/echo", "args": ["test"]}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("bare name"));
    }

    #[tokio::test]
    async fn spawn_echo_process() {
        let p = make_provider();
        let ctx = make_context();

        let result = p
            .execute(
                "spawn",
                json!({"binary": "echo", "args": ["hello", "world"]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.output["process_id"].is_string());
        assert!(result.output["binary"].as_str().unwrap().contains("echo"));
        assert_eq!(result.output["status"], "running");
    }

    #[tokio::test]
    async fn spawn_denied_binary_fails() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute("spawn", json!({"binary": "sudo", "args": ["-i"]}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("denied list"));
    }

    #[tokio::test]
    async fn spawn_disallowed_binary_fails() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute("spawn", json!({"binary": "unknown-binary"}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not on the allowed list"));
    }

    #[tokio::test]
    async fn max_processes_enforced() {
        let p = make_provider();
        let ctx = make_context();

        // Spawn 3 processes (the max)
        for _ in 0..3 {
            p.execute("spawn", json!({"binary": "sleep", "args": ["10"]}), &ctx)
                .await
                .unwrap();
        }

        // 4th should fail
        let err = p
            .execute("spawn", json!({"binary": "sleep", "args": ["10"]}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("maximum of 3"));
    }

    #[tokio::test]
    async fn list_processes() {
        let p = make_provider();
        let ctx = make_context();

        p.execute("spawn", json!({"binary": "sleep", "args": ["10"]}), &ctx)
            .await
            .unwrap();

        let result = p.execute("list", json!({}), &ctx).await.unwrap();
        assert_eq!(result.output["count"], 1);
    }

    #[tokio::test]
    async fn agent_isolation() {
        let p = make_provider();
        let ctx_a = make_context();
        let ctx_b = CapabilityContext {
            agent_id: AgentID::new(),
            ..make_context()
        };

        let spawn_result = p
            .execute("spawn", json!({"binary": "sleep", "args": ["10"]}), &ctx_a)
            .await
            .unwrap();

        let proc_id = spawn_result.output["process_id"].as_str().unwrap();

        // Agent B can't see Agent A's processes
        let list_b = p.execute("list", json!({}), &ctx_b).await.unwrap();
        assert_eq!(list_b.output["count"], 0);

        // Agent B can't signal Agent A's processes
        let err = p
            .execute(
                "signal",
                json!({"process_id": proc_id, "signal": "SIGTERM"}),
                &ctx_b,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not found"));
    }

    #[tokio::test]
    async fn wait_for_fast_process() {
        let p = make_provider();
        let ctx = make_context();

        let spawn_result = p
            .execute("spawn", json!({"binary": "true"}), &ctx)
            .await
            .unwrap();

        let proc_id = spawn_result.output["process_id"].as_str().unwrap();

        // Give it a moment to exit
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let wait_result = p
            .execute(
                "wait",
                json!({"process_id": proc_id, "timeout_secs": 5}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(wait_result.output["exit_code"], 0);
    }

    #[tokio::test]
    async fn output_capture() {
        let p = make_provider();
        let ctx = make_context();

        let spawn_result = p
            .execute(
                "spawn",
                json!({"binary": "echo", "args": ["hello from managed process"]}),
                &ctx,
            )
            .await
            .unwrap();

        let proc_id = spawn_result.output["process_id"].as_str().unwrap();

        // Wait for process to finish and output to be captured
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let output_result = p
            .execute("output", json!({"process_id": proc_id, "lines": 10}), &ctx)
            .await
            .unwrap();

        let lines = output_result.output["lines"].as_array().unwrap();
        assert!(!lines.is_empty());
        assert!(lines[0]
            .as_str()
            .unwrap()
            .contains("hello from managed process"));
    }

    #[tokio::test]
    async fn unknown_action_fails() {
        let p = make_provider();
        let ctx = make_context();

        let err = p.execute("restart", json!({}), &ctx).await.unwrap_err();
        assert!(format!("{err}").contains("unknown proc action"));
    }
}
