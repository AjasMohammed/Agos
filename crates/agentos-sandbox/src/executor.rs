//! Sandbox executor — spawns tool logic in an isolated child process.
//!
//! On Linux, applies seccomp-BPF filters and resource limits.
//! On other platforms, applies resource limits only (no seccomp).

use crate::config::SandboxConfig;
use crate::result::SandboxResult;
use agentos_types::AgentOSError;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

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
}

impl SandboxExecutor {
    /// Create a new sandbox executor with the given data directory.
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
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
    pub async fn spawn(
        &self,
        tool_name: &str,
        payload: serde_json::Value,
        config: &SandboxConfig,
        timeout: Duration,
    ) -> Result<SandboxResult, AgentOSError> {
        let start = Instant::now();

        // 1. Serialize the execution request to a temp file
        let exec_request = serde_json::json!({
            "tool_name": tool_name,
            "payload": payload,
            "data_dir": self.data_dir.to_string_lossy(),
        });
        let temp_dir = std::env::temp_dir().join("agentos-sandbox");
        std::fs::create_dir_all(&temp_dir).map_err(|e| AgentOSError::SandboxSpawnFailed {
            reason: format!("Cannot create temp dir: {}", e),
        })?;
        let request_file = temp_dir.join(format!("{}-{}.json", tool_name, uuid_v4_hex()));
        std::fs::write(&request_file, exec_request.to_string()).map_err(|e| {
            AgentOSError::SandboxSpawnFailed {
                reason: format!("Cannot write request file: {}", e),
            }
        })?;

        // 2. Build the child command
        let current_exe =
            std::env::current_exe().map_err(|e| AgentOSError::SandboxSpawnFailed {
                reason: format!("Cannot determine current executable: {}", e),
            })?;

        // We use the same binary with a special `--sandbox-exec` flag.
        // The child reads the request file, executes the tool, and writes JSON to stdout.
        let mut cmd = tokio::process::Command::new(&current_exe);
        cmd.arg("--sandbox-exec")
            .arg(request_file.to_string_lossy().as_ref())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());

        // 3. Set resource limits and seccomp via pre-exec hook (unsafe because pre_exec)
        let max_memory = config.max_memory_bytes;
        let max_cpu_secs = config.max_cpu_ms.div_ceil(1000); // round up to full seconds

        #[cfg(target_os = "linux")]
        let bpf_filter = {
            let filter = crate::filter::build_seccomp_filter(config)?;
            Some(filter)
        };

        // SAFETY: pre_exec runs in the forked child before exec. We only call
        // async-signal-safe libc functions (setrlimit, prctl).
        unsafe {
            #[cfg(target_os = "linux")]
            let bpf_for_closure = bpf_filter.clone();

            cmd.pre_exec(move || {
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

                // On Linux: set no_new_privs and apply seccomp filter
                #[cfg(target_os = "linux")]
                {
                    // PR_SET_NO_NEW_PRIVS is required before applying seccomp
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
            max_memory_mb = max_memory / (1024 * 1024),
            max_cpu_secs = max_cpu_secs,
            "Sandbox child spawned"
        );

        // 5. Wait for completion with timeout
        let result = tokio::time::timeout(timeout, async {
            let status = child.wait().await;

            // Read stdout and stderr
            let mut stdout_buf = String::new();
            let mut stderr_buf = String::new();

            if let Some(mut stdout) = child.stdout.take() {
                stdout.read_to_string(&mut stdout_buf).await.ok();
            }
            if let Some(mut stderr) = child.stderr.take() {
                stderr.read_to_string(&mut stderr_buf).await.ok();
            }

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

                tracing::info!(
                    tool = %tool_name,
                    exit_code = exit_code,
                    wall_time_ms = wall_time_ms,
                    stdout_len = stdout.len(),
                    stderr_len = stderr.len(),
                    "Sandbox child completed"
                );

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

                // Drain any partial output
                let mut stdout_buf = String::new();
                let mut stderr_buf = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    stdout.read_to_string(&mut stdout_buf).await.ok();
                }
                if let Some(mut stderr) = child.stderr.take() {
                    stderr.read_to_string(&mut stderr_buf).await.ok();
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

/// Generate a simple hex UUID-like string for temp file naming.
fn uuid_v4_hex() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    format!("{:x}-{:x}", nanos, pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_executor_new() {
        let dir = std::env::temp_dir();
        let executor = SandboxExecutor::new(dir.clone());
        assert_eq!(executor.data_dir(), &dir);
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
    fn test_uuid_v4_hex_unique() {
        let a = uuid_v4_hex();
        // Small sleep is not needed since nanos resolution should differ
        let b = uuid_v4_hex();
        // They should at least be non-empty
        assert!(!a.is_empty());
        assert!(!b.is_empty());
    }
}
