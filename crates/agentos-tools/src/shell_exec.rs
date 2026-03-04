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

        let mut cmd = if bwrap_check.is_ok() {
            // Build the bwrap command
            // We want to mount the root filesystem read-only,
            // mount the agent's data directory read-write into a known location (or keeping its path),
            // and hide sensitive directories by mounting an empty tmpfs over them.
            let mut proc = Command::new("bwrap");

            proc.arg("--ro-bind").arg("/usr").arg("/usr")
                .arg("--ro-bind").arg("/lib").arg("/lib")
                .arg("--ro-bind").arg("/lib64").arg("/lib64")
                .arg("--ro-bind").arg("/bin").arg("/bin")
                .arg("--ro-bind").arg("/sbin").arg("/sbin")
                // Bind the data dir as the only writable place
                .arg("--bind").arg(&data_dir_str).arg(&data_dir_str)
                // Hide sensitive directories
                .arg("--tmpfs").arg("/root")
                .arg("--tmpfs").arg("/etc")
                .arg("--tmpfs").arg("/var")
                .arg("--tmpfs").arg("/home") // hide other users' homes
                // Give it a fresh /tmp and /dev
                .arg("--tmpfs").arg("/tmp")
                .arg("--dev").arg("/dev")
                .arg("--proc").arg("/proc")
                .arg("--unshare-all")
                .arg("--share-net") // or omit depending on network requirements for this tool
                // Change to the data dir
                .arg("--chdir").arg(&data_dir_str)
                // Finally, pass the shell and the command
                .arg("--")
                .arg("sh")
                .arg("-c")
                .arg(command);

            proc
        } else {
            // Fallback for development without bwrap (Warn user in logs)
            tracing::warn!("bwrap not found, running shell-exec WITHOUT path isolation! This is dangerous in production!");
            let mut proc = Command::new("sh");
            proc.arg("-c").arg(command);
            proc.current_dir(&data_dir_str);
            proc
        };

        let output = tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output())
            .await
            .map_err(|_| AgentOSError::ToolExecutionFailed {
                tool_name: "shell-exec".into(),
                reason: format!("Command timed out after {}s", timeout_secs),
            })?
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "shell-exec".into(),
                reason: format!("Failed to execute command: {}", e),
            })?;

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

        Ok(serde_json::json!({
            "command": command,
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": stdout_display,
            "stderr": stderr_display,
            "success": output.status.success(),
        }))
    }
}
