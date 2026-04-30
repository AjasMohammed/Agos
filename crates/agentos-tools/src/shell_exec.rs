use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;

pub struct ShellExec;

impl ShellExec {
    pub fn new() -> Self {
        Self
    }

    fn sandbox_context(&self, allow_network: bool) -> serde_json::Value {
        serde_json::json!({
            "kind": "bwrap",
            "pid_namespace": "isolated",
            "network": if allow_network { "host" } else { "isolated" },
            "filesystem": "tmpfs+data_dir_bind",
            "note": "Process list, network sockets, and most of /proc reflect \
                     the sandbox container, not the host. For host-level \
                     inspection use process-manager, network-sockets, \
                     system-mounts, system-services, or system-open-files."
        })
    }
}

impl Default for ShellExec {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ShellExec {
    fn name(&self) -> &str {
        "shell-exec"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("process.exec".to_string(), PermissionOp::Execute),
            ("fs.user_data".to_string(), PermissionOp::Write),
        ]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let command = payload
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("shell-exec requires 'command' field".into())
            })?;

        let timeout_secs = payload
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);

        // Sanitize the command string very basically (though bwrap provides the real isolation)
        if command.contains('\0') {
            return Err(AgentOSError::PermissionDenied {
                resource: "process.exec".into(),
                operation: "Command contains null bytes".into(),
            });
        }

        let data_dir_str = context.data_dir.to_string_lossy().to_string();

        // Check if bwrap is available (at runtime)
        let bwrap_check = Command::new("bwrap").arg("--version").output().await;

        // Determine whether network access is explicitly requested
        let allow_network = payload
            .get("allow_network")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut cmd = if bwrap_check.is_ok() {
            // Build the bwrap command
            // We want to mount the root filesystem read-only,
            // mount the agent's data directory read-write into a known location (or keeping its path),
            // and hide sensitive directories by mounting an empty tmpfs over them.
            let mut proc = Command::new("bwrap");

            proc.arg("--ro-bind")
                .arg("/usr")
                .arg("/usr")
                .arg("--ro-bind")
                .arg("/lib")
                .arg("/lib")
                .arg("--ro-bind")
                .arg("/lib64")
                .arg("/lib64")
                .arg("--ro-bind")
                .arg("/bin")
                .arg("/bin")
                .arg("--ro-bind")
                .arg("/sbin")
                .arg("/sbin")
                // Hide sensitive directories first — bwrap applies args in order,
                // so any --bind on these paths must come AFTER the tmpfs to survive.
                .arg("--tmpfs")
                .arg("/root")
                .arg("--tmpfs")
                .arg("/etc")
                .arg("--tmpfs")
                .arg("/var")
                .arg("--tmpfs")
                .arg("/home")
                .arg("--tmpfs")
                .arg("/tmp")
                // Bind the data dir as the only writable place (after /home tmpfs
                // so it isn't shadowed when data_dir lives under /home/<user>).
                .arg("--bind")
                .arg(&data_dir_str)
                .arg(&data_dir_str)
                .arg("--dev")
                .arg("/dev")
                .arg("--proc")
                .arg("/proc")
                .arg("--unshare-all");

            // Only share network if explicitly requested — default is isolated
            if allow_network {
                proc.arg("--share-net");
            }

            proc
                // Change to the data dir
                .arg("--chdir")
                .arg(&data_dir_str)
                // Finally, pass the shell and the command
                .arg("--")
                .arg("sh")
                .arg("-c")
                .arg(command);

            proc
        } else {
            // SECURITY: Refuse to run without sandbox isolation.
            // Running arbitrary shell commands without bwrap is an unacceptable risk
            // in any environment. Install bwrap (bubblewrap) to use shell-exec.
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "shell-exec".into(),
                reason: "bwrap (bubblewrap) is not installed. shell-exec requires sandbox isolation and cannot run without it. Install bwrap to enable shell command execution.".into(),
            });
        };

        // Truncate command preview to avoid logging secrets at debug level
        let cmd_preview = if command.len() > 120 {
            &command[..120]
        } else {
            command
        };
        tracing::debug!(
            command_preview = cmd_preview,
            timeout_secs,
            allow_network,
            "shell-exec: starting"
        );

        // kill_on_drop ensures the sandboxed process is killed if the future
        // is dropped (e.g. when the cancellation branch fires in select!).
        cmd.kill_on_drop(true);

        let output = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()) => {
                result
                    .map_err(|_| AgentOSError::ToolExecutionFailed {
                        tool_name: "shell-exec".into(),
                        reason: format!("Command timed out after {}s", timeout_secs),
                    })?
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "shell-exec".into(),
                        reason: format!("Failed to execute command: {}", e),
                    })?
            }
            _ = context.cancellation_token.cancelled() => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "shell-exec".into(),
                    reason: "Tool execution cancelled".into(),
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Truncate large outputs
        let max_output = 50_000;
        let stdout_display = if stdout.len() > max_output {
            format!("{}... [TRUNCATED]", &stdout[..max_output])
        } else {
            stdout.to_string()
        };
        let stderr_display = if stderr.len() > max_output {
            format!("{}... [TRUNCATED]", &stderr[..max_output])
        } else {
            stderr.to_string()
        };

        let exit_code = output.status.code().unwrap_or(-1);
        if !output.status.success() {
            // Truncate command to avoid leaking secrets (API keys, tokens in env vars)
            let cmd_preview = if command.len() > 120 {
                &command[..120]
            } else {
                command
            };
            tracing::warn!(
                command_preview = cmd_preview,
                exit_code,
                stderr_bytes = output.stderr.len(),
                "shell-exec: command exited with non-zero status"
            );
        } else {
            tracing::debug!(exit_code, "shell-exec: completed");
        }

        Ok(serde_json::json!({
            "command": command,
            "exit_code": exit_code,
            "stdout": stdout_display,
            "stderr": stderr_display,
            "success": output.status.success(),
            "sandbox": self.sandbox_context(allow_network),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::ToolExecutionContext;
    use std::path::PathBuf;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_shell_exec_includes_sandbox_envelope() {
        if which::which("bwrap").is_err() {
            println!("Skipping test: bwrap not installed");
            return;
        }

        let tool = ShellExec::new();
        let (tx, _rx) = mpsc::channel(1);
        let temp_dir = tempfile::tempdir().unwrap();
        let context = ToolExecutionContext {
            agent_id: "test-agent".into(),
            data_dir: temp_dir.path().to_path_buf(),
            log_sender: tx,
            cancellation_token: CancellationToken::new(),
        };

        let payload = serde_json::json!({
            "command": "echo hello",
        });

        let result = tool.execute(payload, context).await.unwrap();
        let sandbox = result
            .get("sandbox")
            .expect("Result should have 'sandbox' field");

        assert_eq!(sandbox["kind"], "bwrap");
        assert_eq!(sandbox["pid_namespace"], "isolated");
        assert_eq!(sandbox["network"], "isolated");
        assert!(sandbox["note"]
            .as_str()
            .unwrap()
            .contains("host-level inspection"));
    }
}
