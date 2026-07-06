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

        // Determine whether network access is explicitly requested. Network
        // egress from a sandboxed command is itself a capability: requesting it
        // requires the `network.outbound` permission, exactly like web-fetch and
        // http-client. Without this gate `allow_network:true` would be a free
        // SSRF/egress escape hatch for any agent holding only `process.exec`.
        let allow_network = payload
            .get("allow_network")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if allow_network
            && !context
                .permissions
                .check("network.outbound", PermissionOp::Execute)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "network.outbound".into(),
                operation: "shell-exec allow_network=true requires the network.outbound permission"
                    .into(),
            });
        }

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
                // Bind the data dir as the always-writable place (after the
                // /home tmpfs so it isn't shadowed when data_dir lives under
                // /home/<user>).
                .arg("--bind")
                .arg(&data_dir_str)
                .arg(&data_dir_str);

            // Bind every `workspace_paths_executable` entry as writable. These
            // are user-granted directories with `--mode rwx` — the sandbox
            // child sees them at their real on-disk path so commands like
            // `ls`, `cargo build`, `python` act on real files. Bindings come
            // AFTER the tmpfs steps so they survive being shadowed. Skip any
            // path under data_dir (already bound) to avoid bwrap "already
            // bound" errors.
            let data_dir_canon = std::path::Path::new(&data_dir_str);
            for exec_path in &context.workspace_paths_executable {
                if exec_path.starts_with(data_dir_canon) {
                    continue;
                }
                proc.arg("--bind").arg(exec_path).arg(exec_path);
            }

            proc.arg("--dev")
                .arg("/dev")
                .arg("--proc")
                .arg("/proc")
                .arg("--unshare-all");

            // SECURITY: scrub the environment. bwrap inherits the parent process
            // environment by default, which on the kernel host contains every
            // provider/API secret (OPENAI_API_KEY, ANTHROPIC_API_KEY, BRAVE/…,
            // cloud creds). `--clearenv` drops all of it inside the sandbox; we
            // then re-inject only a minimal, non-sensitive set so ordinary
            // commands still work. `env`/`printenv` in the sandbox now sees only
            // these, never the kernel's secrets.
            proc.arg("--clearenv")
                .arg("--setenv")
                .arg("PATH")
                .arg("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
                .arg("--setenv")
                .arg("HOME")
                .arg(&data_dir_str)
                .arg("--setenv")
                .arg("TMPDIR")
                .arg("/tmp")
                .arg("--setenv")
                .arg("LANG")
                .arg("C.UTF-8");

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
    use crate::traits::ToolExecutionContext;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use tokio_util::sync::CancellationToken;

    fn make_context(data_dir: std::path::PathBuf) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir,
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: CancellationToken::new(),
            tool_categories: None,
        }
    }

    #[tokio::test]
    async fn test_shell_exec_includes_sandbox_envelope() {
        if std::process::Command::new("bwrap")
            .arg("--version")
            .output()
            .is_err()
        {
            println!("Skipping test: bwrap not installed");
            return;
        }

        let tool = ShellExec::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let context = make_context(temp_dir.path().to_path_buf());

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
