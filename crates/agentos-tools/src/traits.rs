use agentos_types::*;
use async_trait::async_trait;
use std::path::PathBuf;

/// Every tool implements this trait.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// The tool's name (must match manifest).
    fn name(&self) -> &str;

    /// Execute the tool with the given payload.
    /// The kernel has already validated the capability token and permissions
    /// before calling this method.
    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError>;

    /// Return the permissions this tool requires to operate.
    fn required_permissions(&self) -> Vec<(String, PermissionOp)>;
}

/// Context provided to the tool at execution time.
/// Contains references to kernel resources the tool is allowed to use.
#[derive(Clone)]
pub struct ToolExecutionContext {
    pub data_dir: PathBuf, // /opt/agentos/data — where tools read/write files
    pub task_id: TaskID,
    pub trace_id: TraceID,
}
