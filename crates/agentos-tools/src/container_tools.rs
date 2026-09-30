use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Names of the container tools, all served by [`ContainerTool`].
pub const CONTAINER_TOOL_NAMES: &[&str] = &[
    "container-create",
    "container-exec",
    "container-logs",
    "container-destroy",
    "container-list",
];

/// Stateless shim for the `container-*` tools.
///
/// The Docker runtime, per-agent quotas, ownership checks and audit all live
/// in the kernel (`commands/container.rs`). This tool only forwards the
/// payload as a `_kernel_action`; the kernel runs it as the calling agent.
pub struct ContainerTool {
    name: &'static str,
}

impl ContainerTool {
    /// Returns `None` for a name that is not in [`CONTAINER_TOOL_NAMES`].
    pub fn new(name: &str) -> Option<Self> {
        CONTAINER_TOOL_NAMES
            .iter()
            .find(|n| **n == name)
            .map(|n| Self { name: n })
    }
}

#[async_trait]
impl AgentTool for ContainerTool {
    fn name(&self) -> &str {
        self.name
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        let (resource, op) = match self.name {
            "container-create" => ("container.create", PermissionOp::Execute),
            "container-exec" => ("container.exec", PermissionOp::Execute),
            "container-destroy" => ("container.destroy", PermissionOp::Execute),
            "container-logs" => ("container.logs", PermissionOp::Read),
            _ => ("container.list", PermissionOp::Read),
        };
        vec![(resource.to_string(), op)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "_kernel_action": "container",
            "tool": self.name,
            "payload": payload,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_builds_and_unknown_does_not() {
        for name in CONTAINER_TOOL_NAMES {
            assert_eq!(ContainerTool::new(name).unwrap().name(), *name);
        }
        assert!(ContainerTool::new("container-nope").is_none());
    }

    #[test]
    fn permissions_match_manifests() {
        let perm = |n: &str| ContainerTool::new(n).unwrap().required_permissions()[0].clone();
        assert_eq!(
            perm("container-create"),
            ("container.create".to_string(), PermissionOp::Execute)
        );
        assert_eq!(
            perm("container-list"),
            ("container.list".to_string(), PermissionOp::Read)
        );
        assert_eq!(
            perm("container-logs"),
            ("container.logs".to_string(), PermissionOp::Read)
        );
    }
}
