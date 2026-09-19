use crate::sandbox_fs;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::time::Duration;

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

        // SECURITY: bind the agent's own home into the sandbox, never the kernel
        // state dir (audit.db, api_keys.db, chat.db, agents.json live there).
        let data_dir_str = context.agent_files_dir()?.to_string_lossy().to_string();

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

        // Writable: the agent home plus every `--mode rwx` workspace grant, at
        // their real paths so `ls`, `cargo build`, `python` act on real files.
        // Everything else — kernel state, other agents, the operator's home —
        // does not exist inside. The environment is cleared (the kernel's holds
        // every provider API key) and network is opt-in.
        let data_dir = std::path::Path::new(&data_dir_str);
        let mut sandbox = sandbox_fs::Sandbox::new("shell-exec")
            .bind_rw(data_dir)
            .network(allow_network)
            .env("HOME", data_dir);
        for exec_path in
            sandbox_fs::grants_outside(&context.workspace_paths_executable, &context.data_dir)
        {
            if exec_path.starts_with(data_dir) {
                continue;
            }
            // Say why a granted folder is missing inside; the builder skips it
            // because bwrap would abort the whole call on it.
            if !exec_path.exists() {
                tracing::warn!(
                    path = %exec_path.display(),
                    "shell-exec: executable workspace grant no longer exists; not binding it"
                );
                continue;
            }
            sandbox = sandbox.bind_rw(exec_path);
        }
        let mut cmd = sandbox.command(data_dir, "sh").await?;
        cmd.arg("-c").arg(command);

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
        if !crate::sandbox_fs::bwrap_usable().await {
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
