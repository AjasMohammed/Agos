use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct SystemMountsTool;

impl SystemMountsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemMountsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for SystemMountsTool {
    fn name(&self) -> &str {
        "system-mounts"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("system.mounts".to_string(), PermissionOp::Read)]
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
        perms.grant("system.mounts".to_string(), true, false, false, None);

        hal.query(
            "mounts",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use crate::traits::ToolExecutionContext;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::sync::Arc;

    /// A context wired with the real default HAL (which registers `MountsDriver`),
    /// mirroring how the kernel builds the chat/task tool-execution context.
    fn hal_ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: Some(Arc::new(
                agentos_hal::HardwareAbstractionLayer::new_with_defaults(),
            )),
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
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
        }
    }

    /// Executing `system-mounts` through a real HAL returns the actual root
    /// filesystem with non-zero capacity from `statvfs`. Guards against the tool
    /// returning empty/fabricated data — the failure that lets a model emit the
    /// "2 GiB /dev/sda1 at 50%" hallucination when real mount data is absent.
    #[tokio::test]
    async fn system_mounts_returns_real_root_mount() {
        let tool = SystemMountsTool::new();
        let result = tool
            .execute(serde_json::json!({}), hal_ctx())
            .await
            .expect("system-mounts should execute with a HAL present");

        let mounts = result["mounts"].as_array().expect("mounts array present");
        assert!(!mounts.is_empty(), "expected at least one real mount");

        let root = mounts
            .iter()
            .find(|m| m["mount_point"] == "/")
            .expect("root '/' mount present");
        let total = root["total_bytes"].as_u64().unwrap_or(0);
        assert!(
            total > 0,
            "root mount must report real capacity via statvfs, got {total}"
        );

        println!(
            "system-mounts root → device={} total_bytes={} use_percent={}",
            root["device"], total, root["use_percent"]
        );
    }

    /// Filter params flow through the tool into the HAL driver: an impossible
    /// `fs_type` returns zero matches while the unfiltered call returns some.
    #[tokio::test]
    async fn system_mounts_filter_passthrough() {
        let tool = SystemMountsTool::new();

        let all = tool
            .execute(serde_json::json!({}), hal_ctx())
            .await
            .unwrap();
        let filtered = tool
            .execute(
                serde_json::json!({ "fs_type": "definitely-not-a-real-fs-xyz" }),
                hal_ctx(),
            )
            .await
            .unwrap();

        assert!(all["returned"].as_u64().unwrap() > 0);
        assert_eq!(filtered["returned"].as_u64().unwrap(), 0);
    }

    /// Without a HAL in context the tool fails cleanly (no panic), surfacing a
    /// `ToolExecutionFailed` tagged with the tool name.
    #[tokio::test]
    async fn system_mounts_without_hal_errors_cleanly() {
        let tool = SystemMountsTool::new();
        let mut ctx = hal_ctx();
        ctx.hal = None;

        let err = tool
            .execute(serde_json::json!({}), ctx)
            .await
            .expect_err("missing HAL must error");
        match err {
            AgentOSError::ToolExecutionFailed { tool_name, .. } => {
                assert_eq!(tool_name, "system-mounts");
            }
            other => panic!("expected ToolExecutionFailed, got {other:?}"),
        }
    }
}
