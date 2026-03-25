//! Sandbox executor — spawns tool logic in an isolated child process.
//!
//! On Linux, applies seccomp-BPF filters and resource limits.
//! On other platforms, applies resource limits only (no seccomp).

use crate::config::SandboxConfig;
use crate::request::SandboxExecRequest;
use crate::result::SandboxResult;
use agentos_types::AgentOSError;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Maximum bytes to read from sandbox child stdout or stderr.
/// Prevents child processes from exhausting host memory via large output.
const MAX_SANDBOX_OUTPUT_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB

fn write_request_file(
    temp_dir: &Path,
    tool_name: &str,
    request: &SandboxExecRequest,
) -> Result<PathBuf, AgentOSError> {
    std::fs::create_dir_all(temp_dir).map_err(|e| AgentOSError::SandboxSpawnFailed {
        reason: format!("Cannot create temp dir: {}", e),
    })?;
    restrict_temp_dir_permissions(temp_dir)?;

    let request_file = temp_dir.join(format!("{}-{}.json", tool_name, Uuid::new_v4().simple()));
    let request_json =
        serde_json::to_vec(request).map_err(|e| AgentOSError::SandboxSpawnFailed {
            reason: format!("Cannot serialize sandbox request: {}", e),
        })?;
    let mut file = create_private_request_file(&request_file).map_err(|e| {
        AgentOSError::SandboxSpawnFailed {
            reason: format!("Cannot create request file: {}", e),
        }
    })?;
    file.write_all(&request_json)
        .and_then(|_| file.flush())
        .map_err(|e| AgentOSError::SandboxSpawnFailed {
            reason: format!("Cannot write request file: {}", e),
        })?;

    Ok(request_file)
}

#[cfg(unix)]
fn restrict_temp_dir_permissions(temp_dir: &Path) -> Result<(), AgentOSError> {
    std::fs::set_permissions(temp_dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        AgentOSError::SandboxSpawnFailed {
            reason: format!("Cannot secure temp dir permissions: {}", e),
        }
    })
}

#[cfg(not(unix))]
fn restrict_temp_dir_permissions(_temp_dir: &Path) -> Result<(), AgentOSError> {
    Ok(())
}

#[cfg(unix)]
fn create_private_request_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_request_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Sandbox executor manages the lifecycle of sandboxed tool processes.
///
/// Each tool execution is forked into a child process with:
/// - Resource limits (memory via RLIMIT_AS, CPU via RLIMIT_CPU)
/// - Seccomp-BPF syscall filtering (Linux only)
/// - Wall-clock timeout with forced kill
/// - Captured stdout/stderr
pub struct SandboxExecutor {
    /// Working directory for tool execution.
    data_dir: PathBuf,
    /// Optional override for the sandbox child executable.
    executable_path: Option<PathBuf>,
    /// Limits the number of concurrent sandbox child processes to prevent
    /// thread/process exhaustion (e.g., rayon EAGAIN panics).
    concurrency_semaphore: Arc<Semaphore>,
}

impl SandboxExecutor {
    /// Create a new sandbox executor with the given data directory.
    ///
    /// `max_concurrent` controls the maximum number of sandbox child processes
    /// that can run simultaneously. Clamped to a minimum of 1.
    pub fn new(data_dir: PathBuf, max_concurrent: usize) -> Self {
        Self {
            data_dir,
            executable_path: None,
            concurrency_semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
        }
    }

    /// Create a sandbox executor that launches a specific executable.
    pub fn with_executable(
        data_dir: PathBuf,
        executable_path: PathBuf,
        max_concurrent: usize,
    ) -> Self {
        Self {
            data_dir,
            executable_path: Some(executable_path),
            concurrency_semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
        }
    }

    /// Get the data directory path.
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Spawn a tool in an isolated child process with sandbox restrictions applied.
    ///
    /// The method:
    /// 1. Writes the payload to a temporary file
    /// 2. Spawns a child process with `--sandbox-exec` flag
    /// 3. Applies resource limits and seccomp filters (Linux)
    /// 4. Captures stdout (expected JSON) and stderr
    /// 5. Kills the child on timeout
    ///
    /// # Errors
    ///
    /// Returns `SandboxSpawnFailed` if the child cannot be started,
    /// `SandboxTimeout` if the child exceeds the wall-clock timeout.
    ///
    /// `category_overhead_bytes` must be the per-category startup baseline for
    /// the tool being executed (for example stateless vs memory-heavy).
    pub async fn spawn(
        &self,
        request: SandboxExecRequest,
        config: &SandboxConfig,
        timeout: Duration,
        category_overhead_bytes: u64,
    ) -> Result<SandboxResult, AgentOSError> {
        // Acquire a concurrency permit before spawning. This prevents
        // thread pool exhaustion when many tools run in parallel.
        // The permit is held until this function returns, ensuring the child
        // process has fully exited before releasing the concurrency slot.
        let _permit = self.concurrency_semaphore.acquire().await.map_err(|_| {
            AgentOSError::SandboxSpawnFailed {
                reason: "Sandbox concurrency semaphore closed".to_string(),
            }
        })?;

        let start = Instant::now();
        let tool_name = request.tool_name.clone();

        // 1. Serialize the execution request to a temp file
        let temp_dir = std::env::temp_dir().join("agentos-sandbox");
        let request_file = write_request_file(&temp_dir, &tool_name, &request)?;

        // 2. Build the child command
        let current_exe = if let Some(executable_path) = self.executable_path.as_ref() {
            executable_path.clone()
        } else {
            std::env::current_exe().map_err(|e| AgentOSError::SandboxSpawnFailed {
                reason: format!("Cannot determine current executable: {}", e),
            })?
        };

        // We use the same binary with a special `--sandbox-exec` flag.
        // The child reads the request file, executes the tool, and writes JSON to stdout.
        let mut cmd = tokio::process::Command::new(&current_exe);
        cmd.arg("--sandbox-exec")
            .arg(request_file.to_string_lossy().as_ref())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());

        // Sanitize environment: clear inherited env vars (API keys, LD_PRELOAD, etc.)
        // and set only the minimum required variables.
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.data_dir)
            .env("LANG", "C.UTF-8")
            // Prevent rayon from spawning num_cpus threads in each child.
            // Without this, parallel sandbox children exhaust OS thread limits
            // and panic with EAGAIN in ThreadPoolBuilder.
            .env("RAYON_NUM_THREADS", "1");

        // 3. Set resource limits and seccomp via pre-exec hook (unsafe because pre_exec)
        //
        let exe_size = std::fs::metadata(&current_exe)
            .map(|m| m.len())
            .unwrap_or(0);
        let max_memory = effective_rlimit_as(config, category_overhead_bytes, exe_size);
        let max_fsize = config.max_memory_bytes; // tool's declared budget (for RLIMIT_FSIZE)
        let max_cpu_secs = config.max_cpu_ms.div_ceil(1000); // round up to full seconds

        #[cfg(target_os = "linux")]
        let bpf_filter = {
            let filter = crate::filter::build_seccomp_filter(config)?;
            Some(filter)
        };

        // Prepare null-terminated data_dir path for Landlock (heap allocation
        // must happen before fork; the pre_exec closure is async-signal-safe).
        #[cfg(target_os = "linux")]
        let landlock_data_dir: Vec<u8> = {
            use std::os::unix::ffi::OsStrExt;
            let mut bytes = request.data_dir.as_os_str().as_bytes().to_vec();
            bytes.push(0); // null-terminate for libc::open()
            bytes
        };

        // SAFETY: pre_exec runs in the forked child before exec. We only call
        // async-signal-safe libc functions (setrlimit, prctl, syscall).
        unsafe {
            #[cfg(target_os = "linux")]
            let bpf_for_closure = bpf_filter.clone();

            cmd.pre_exec(move || {
                // NOTE: Do NOT close inherited FDs here. pre_exec runs between
                // fork() and exec(). Tokio uses an internal error-reporting pipe
                // (FD > 2) to communicate exec failures back to the parent. If we
                // close_range(3, MAX) here, that pipe is destroyed and exec failures
                // become undiagnosable. Instead, FD cleanup happens in the child
                // binary's run_sandbox_exec() after exec has succeeded.

                // Set RLIMIT_AS (virtual memory limit)
                let mem_limit = libc::rlimit {
                    rlim_cur: max_memory,
                    rlim_max: max_memory,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &mem_limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // Set RLIMIT_CPU (CPU time limit in seconds)
                let cpu_limit = libc::rlimit {
                    rlim_cur: max_cpu_secs,
                    rlim_max: max_cpu_secs,
                };
                if libc::setrlimit(libc::RLIMIT_CPU, &cpu_limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // Note: RLIMIT_NPROC is intentionally NOT set here.
                // It is a per-user (not per-process) limit, so setting it to a
                // low value would prevent thread creation if the user already has
                // more processes than the limit.  Seccomp + RLIMIT_CPU + RLIMIT_AS
                // already constrain the child sufficiently.

                // Set RLIMIT_FSIZE (prevent disk filling via large file writes)
                // Uses the tool's declared memory budget, not the inflated RLIMIT_AS.
                let fsize_limit = libc::rlimit {
                    rlim_cur: max_fsize,
                    rlim_max: max_fsize,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &fsize_limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // Set RLIMIT_NOFILE (limit open file descriptors)
                let nofile_limit = libc::rlimit {
                    rlim_cur: 256,
                    rlim_max: 256,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &nofile_limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // On Linux: apply Landlock FS write restriction, then seccomp.
                //
                // Order matters:
                //   1. Landlock (calls PR_SET_NO_NEW_PRIVS internally, then restrict_self)
                //   2. Seccomp  (also requires PR_SET_NO_NEW_PRIVS — already set, idempotent)
                //
                // Landlock must come before seccomp because the Landlock syscalls
                // (landlock_create_ruleset, landlock_add_rule, landlock_restrict_self)
                // are not in the seccomp allowlist and would be blocked if seccomp
                // were applied first.
                #[cfg(target_os = "linux")]
                {
                    // Apply Landlock FS write restriction (kernel 5.13+; degrades
                    // gracefully to a no-op on older kernels).
                    crate::landlock::apply_write_restriction(&landlock_data_dir)?;

                    // PR_SET_NO_NEW_PRIVS is required before applying seccomp.
                    // Already set inside apply_write_restriction, but we set it
                    // again here so the invariant is documented and enforced even
                    // if Landlock was skipped.
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }

                    if let Some(ref bpf) = bpf_for_closure {
                        seccompiler::apply_filter(bpf).map_err(|e| {
                            std::io::Error::other(format!("seccomp apply failed: {:?}", e))
                        })?;
                    }
                }

                Ok(())
            });
        }

        // 4. Spawn the child process
        let mut child = cmd.spawn().map_err(|e| AgentOSError::SandboxSpawnFailed {
            reason: format!("Cannot spawn sandbox child: {}", e),
        })?;

        let child_pid = child.id();
        tracing::info!(
            tool = %tool_name,
            pid = ?child_pid,
            declared_memory_mb = config.max_memory_bytes / (1024 * 1024),
            rlimit_as_mb = max_memory / (1024 * 1024),
            exe_floor_mb = exe_size.saturating_mul(2) / (1024 * 1024),
            max_cpu_secs = max_cpu_secs,
            "Sandbox child spawned"
        );

        // 5. Wait for completion with timeout
        //
        // CRITICAL: We must read stdout/stderr CONCURRENTLY with waiting for the
        // child to exit. If we wait() first, the child may fill the OS pipe buffer
        // (typically 64 KiB on Linux) and block on write(), causing a deadlock
        // because the parent never drains the pipe.
        let mut child_stdout = child.stdout.take();
        let mut child_stderr = child.stderr.take();

        let result = tokio::time::timeout(timeout, async {
            let mut stdout_bytes = Vec::new();
            let mut stderr_bytes = Vec::new();

            let stdout_fut = async {
                if let Some(ref mut stdout) = child_stdout {
                    let _ = stdout
                        .take(MAX_SANDBOX_OUTPUT_BYTES)
                        .read_to_end(&mut stdout_bytes)
                        .await;
                }
            };
            let stderr_fut = async {
                if let Some(ref mut stderr) = child_stderr {
                    let _ = stderr
                        .take(MAX_SANDBOX_OUTPUT_BYTES)
                        .read_to_end(&mut stderr_bytes)
                        .await;
                }
            };

            // Wait for stdout drain, stderr drain, AND child exit concurrently.
            let (_, _, status) = tokio::join!(stdout_fut, stderr_fut, child.wait());

            let stdout_buf = String::from_utf8_lossy(&stdout_bytes).into_owned();
            let stderr_buf = String::from_utf8_lossy(&stderr_bytes).into_owned();

            (status, stdout_buf, stderr_buf)
        })
        .await;

        // Clean up temp file (best effort)
        std::fs::remove_file(&request_file).ok();

        let wall_time_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok((status, stdout, stderr)) => {
                let exit_code = match status {
                    Ok(s) => s.code().unwrap_or(-1),
                    Err(e) => {
                        tracing::error!(
                            tool = %tool_name,
                            error = %e,
                            "Sandbox child wait failed"
                        );
                        -1
                    }
                };

                if exit_code != 0 && !stderr.is_empty() {
                    // Truncate stderr for logging to avoid flooding
                    let stderr_preview: &str = if stderr.len() > 512 {
                        &stderr[..512]
                    } else {
                        &stderr
                    };
                    tracing::warn!(
                        tool = %tool_name,
                        exit_code = exit_code,
                        wall_time_ms = wall_time_ms,
                        stderr = %stderr_preview,
                        "Sandbox child failed"
                    );
                    // exit_code == -1 means ExitStatus::code() returned None:
                    // the child was killed by a signal (SIGILL, SIGABRT, SIGSYS…).
                    // Common causes:
                    //   - Missing syscall in seccomp allowlist (seccomp default action = EPERM
                    //     so the process keeps running, but abort() falls back to ud2 → SIGILL)
                    //   - RLIMIT_AS exhausted during Rust/tokio runtime startup → OOM → SIGABRT
                    // If you see this, check the seccomp base allowlist and RLIMIT_AS formula.
                    if exit_code == -1 {
                        tracing::error!(
                            tool = %tool_name,
                            wall_time_ms = wall_time_ms,
                            "Sandbox child killed by signal (exit -1). \
                             Likely causes: missing syscall in seccomp allowlist \
                             or RLIMIT_AS too tight for Rust runtime. \
                             Re-run outside sandbox with RUST_BACKTRACE=1 to diagnose."
                        );
                    }
                } else {
                    tracing::info!(
                        tool = %tool_name,
                        exit_code = exit_code,
                        wall_time_ms = wall_time_ms,
                        stdout_len = stdout.len(),
                        stderr_len = stderr.len(),
                        "Sandbox child completed"
                    );
                }

                Ok(SandboxResult {
                    stdout,
                    stderr,
                    exit_code,
                    wall_time_ms,
                    was_killed: false,
                })
            }
            Err(_) => {
                // Timeout — kill the child
                tracing::warn!(
                    tool = %tool_name,
                    timeout_ms = timeout.as_millis() as u64,
                    "Sandbox child timed out, killing"
                );

                child.kill().await.ok();
                // Reap the zombie so it doesn't leak a PID table entry.
                child.wait().await.ok();

                // Drain any partial output from the already-taken handles.
                let mut stdout_bytes = Vec::new();
                let mut stderr_bytes = Vec::new();
                if let Some(ref mut stdout) = child_stdout {
                    let _ = stdout
                        .take(MAX_SANDBOX_OUTPUT_BYTES)
                        .read_to_end(&mut stdout_bytes)
                        .await;
                }
                if let Some(ref mut stderr) = child_stderr {
                    let _ = stderr
                        .take(MAX_SANDBOX_OUTPUT_BYTES)
                        .read_to_end(&mut stderr_bytes)
                        .await;
                }

                Err(AgentOSError::SandboxTimeout {
                    tool_name: tool_name.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
        }
    }

    /// Parse a `SandboxResult`'s stdout as a JSON value.
    ///
    /// Returns the parsed JSON, or an error if the output is not valid JSON.
    pub fn parse_result(result: &SandboxResult) -> Result<serde_json::Value, AgentOSError> {
        if !result.is_success() {
            return Err(AgentOSError::SandboxSpawnFailed {
                reason: format!(
                    "Child exited with code {} (killed={}). stderr: {}",
                    result.exit_code,
                    result.was_killed,
                    result.stderr.chars().take(500).collect::<String>(),
                ),
            });
        }

        serde_json::from_str(&result.stdout).map_err(|e| AgentOSError::SandboxSpawnFailed {
            reason: format!(
                "Failed to parse sandbox output as JSON: {}. stdout: {}",
                e,
                result.stdout.chars().take(500).collect::<String>(),
            ),
        })
    }
}

/// Compute effective RLIMIT_AS, applying an executable-size floor.
///
/// Debug builds can be 700+ MB, and the dynamic linker maps the entire text section
/// into virtual memory on exec(). The Tokio runtime then adds thread stacks (~8 MB
/// virtual each). Without this floor, tools with small declared budgets OOM before
/// they can even start. For release builds (~30 MB), the floor is well below every
/// category overhead constant, so it has no effect.
fn effective_rlimit_as(
    config: &SandboxConfig,
    category_overhead_bytes: u64,
    exe_size_bytes: u64,
) -> u64 {
    let exe_floor = exe_size_bytes.saturating_mul(2);
    config
        .rlimit_as_bytes(category_overhead_bytes)
        .max(exe_floor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::PermissionSet;

    #[test]
    fn test_sandbox_executor_new() {
        let dir = std::env::temp_dir();
        let executor = SandboxExecutor::new(dir.clone(), 4);
        assert_eq!(executor.data_dir(), &dir);
        assert!(executor.executable_path.is_none());
    }

    #[test]
    fn test_sandbox_executor_with_executable_override() {
        let dir = std::env::temp_dir();
        let executable_path = PathBuf::from("/tmp/agentctl-test");
        let executor = SandboxExecutor::with_executable(dir.clone(), executable_path.clone(), 4);
        assert_eq!(executor.data_dir(), &dir);
        assert_eq!(executor.executable_path.as_ref(), Some(&executable_path));
    }

    #[test]
    fn test_parse_result_success() {
        let result = SandboxResult {
            stdout: r#"{"parsed": true}"#.to_string(),
            stderr: String::new(),
            exit_code: 0,
            wall_time_ms: 100,
            was_killed: false,
        };
        let parsed = SandboxExecutor::parse_result(&result).unwrap();
        assert_eq!(parsed["parsed"], true);
    }

    #[test]
    fn test_parse_result_failure() {
        let result = SandboxResult {
            stdout: String::new(),
            stderr: "some error".to_string(),
            exit_code: 1,
            wall_time_ms: 50,
            was_killed: false,
        };
        assert!(SandboxExecutor::parse_result(&result).is_err());
    }

    #[test]
    fn test_parse_result_killed() {
        let result = SandboxResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: -1,
            wall_time_ms: 5000,
            was_killed: true,
        };
        assert!(SandboxExecutor::parse_result(&result).is_err());
    }

    #[test]
    fn test_parse_result_invalid_json() {
        let result = SandboxResult {
            stdout: "not json".to_string(),
            stderr: String::new(),
            exit_code: 0,
            wall_time_ms: 100,
            was_killed: false,
        };
        assert!(SandboxExecutor::parse_result(&result).is_err());
    }

    #[test]
    fn test_temp_file_name_is_unique() {
        let a = Uuid::new_v4().simple().to_string();
        let b = Uuid::new_v4().simple().to_string();
        assert_ne!(a, b, "Each UUID must be unique");
        assert_eq!(a.len(), 32, "simple UUID should be 32 hex chars");
    }

    #[cfg(unix)]
    #[test]
    fn test_request_file_permissions_are_private() {
        let temp_root = tempfile::TempDir::new().unwrap();
        let sandbox_dir = temp_root.path().join("agentos-sandbox");
        let request = SandboxExecRequest {
            tool_name: "datetime".to_string(),
            payload: serde_json::json!({}),
            data_dir: temp_root.path().join("data"),
            manifest_weight: None,
            task_id: None,
            agent_id: None,
            trace_id: None,
            permissions: PermissionSet::new(),
            workspace_paths: None,
        };

        let request_file = write_request_file(&sandbox_dir, &request.tool_name, &request).unwrap();

        let file_mode = std::fs::metadata(&request_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = std::fs::metadata(&sandbox_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn test_effective_rlimit_as_uses_category_when_larger() {
        // Release binary ~30 MB → floor = 60 MB, category = 256 MB → category wins
        let config = SandboxConfig {
            max_memory_bytes: 64 * 1024 * 1024,
            ..Default::default()
        };
        let result =
            effective_rlimit_as(&config, SandboxConfig::OVERHEAD_STATELESS, 30 * 1024 * 1024);
        assert_eq!(
            result,
            config.rlimit_as_bytes(SandboxConfig::OVERHEAD_STATELESS)
        );
    }

    #[test]
    fn test_effective_rlimit_as_uses_exe_floor_when_larger() {
        // Debug binary ~700 MB → floor = 1400 MB, category = 208 MB → floor wins
        let config = SandboxConfig {
            max_memory_bytes: 16 * 1024 * 1024,
            ..Default::default()
        };
        let exe_size = 700 * 1024 * 1024;
        let result = effective_rlimit_as(&config, SandboxConfig::OVERHEAD_HAL, exe_size);
        assert_eq!(result, exe_size * 2);
        assert!(result > config.rlimit_as_bytes(SandboxConfig::OVERHEAD_HAL));
    }

    #[test]
    fn test_effective_rlimit_as_zero_exe_size_uses_category() {
        let config = SandboxConfig::default();
        let result = effective_rlimit_as(&config, SandboxConfig::OVERHEAD_DEFAULT, 0);
        assert_eq!(
            result,
            config.rlimit_as_bytes(SandboxConfig::OVERHEAD_DEFAULT)
        );
    }
}
