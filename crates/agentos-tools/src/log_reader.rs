use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct LogReaderTool;

impl LogReaderTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LogReaderTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for LogReaderTool {
    fn name(&self) -> &str {
        "log-reader"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("fs.app_logs".to_string(), PermissionOp::Read),
            ("fs.system_logs".to_string(), PermissionOp::Read),
        ]
    }

    /// The runner checks *every* entry of the returned list, so without this
    /// override reading an app log demanded `fs.system_logs` too — an agent
    /// scoped to its own logs could not read them at all.
    ///
    /// `source` is `"SystemLog"`, `"KernelLog"`, or `{"AppLog": "<name>"}`.
    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        let is_app_log = payload
            .get("source")
            .is_some_and(|source| source.get("AppLog").is_some());
        if is_app_log {
            vec![("fs.app_logs".to_string(), PermissionOp::Read)]
        } else {
            vec![("fs.system_logs".to_string(), PermissionOp::Read)]
        }
    }

    async fn execute(
        &self,
        payload: Value,
        context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        let hal = context
            .hal
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "Hardware Abstraction Layer (HAL) not available in this context"
                    .to_string(),
            })?;

        let mut perms = agentos_types::PermissionSet::new();
        perms.grant("fs.app_logs".to_string(), true, false, false, None);
        perms.grant("fs.system_logs".to_string(), true, false, false, None);

        hal.query(
            "log",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Both permissions used to be demanded for every read, so an agent scoped
    /// to its own application logs could not read them at all.
    #[test]
    fn app_log_does_not_require_system_logs() {
        let tool = LogReaderTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({"source": {"AppLog": "agentos"}})),
            vec![("fs.app_logs".to_string(), PermissionOp::Read)]
        );
    }

    #[test]
    fn system_and_kernel_logs_require_system_logs() {
        let tool = LogReaderTool::new();
        let expected = vec![("fs.system_logs".to_string(), PermissionOp::Read)];
        assert_eq!(
            tool.required_permissions_for(&json!({"source": "SystemLog"})),
            expected
        );
        assert_eq!(
            tool.required_permissions_for(&json!({"source": "KernelLog"})),
            expected
        );
        // `source` is required by the manifest; a malformed payload must fail
        // closed on the stricter of the two rather than the looser.
        assert_eq!(tool.required_permissions_for(&json!({})), expected);
    }
}
