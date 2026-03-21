use agentos_types::*;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

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
    pub agent_id: AgentID,
    pub trace_id: TraceID,
    pub permissions: PermissionSet,
    pub vault: Option<std::sync::Arc<agentos_vault::ProxyVault>>,
    pub hal: Option<std::sync::Arc<agentos_hal::HardwareAbstractionLayer>>,
    /// Shared file lock registry injected by `ToolRunner`. `None` when tools
    /// are called directly in tests without going through the runner.
    pub file_lock_registry: Option<std::sync::Arc<crate::file_lock::FileLockRegistry>>,
    /// Snapshot of the agent registry at task dispatch time. `None` outside kernel context.
    pub agent_registry: Option<std::sync::Arc<dyn AgentRegistryQuery>>,
    /// Snapshot of the task store at task dispatch time. `None` outside kernel context.
    pub task_registry: Option<std::sync::Arc<dyn TaskQuery>>,
    /// Snapshot of the escalation manager at task dispatch time. `None` outside kernel context.
    pub escalation_query: Option<std::sync::Arc<dyn EscalationQuery>>,
    /// Additional directories the agent may access beyond `data_dir`.
    /// Populated from `tools.workspace.allowed_paths` in the kernel config.
    /// Paths are pre-canonicalized at kernel startup.
    pub workspace_paths: Vec<PathBuf>,
    /// Cancellation token for this tool invocation. Tools that perform
    /// long-running I/O (HTTP, shell exec) should check this token periodically
    /// and return early with a `ToolExecutionFailed` error if it is cancelled.
    pub cancellation_token: CancellationToken,
}

/// Resolve a user-supplied path for file tools, respecting workspace paths.
///
/// Resolution rules:
/// - Relative path → joined onto `data_dir`.
/// - Absolute path that starts with a configured workspace prefix → used as-is.
/// - Absolute path with no workspace match → the leading `/` is stripped and the
///   remainder is joined onto `data_dir` (legacy behavior for data-dir-relative
///   absolute paths).
///
/// The caller must still canonicalize the result and verify containment within
/// `data_dir` or one of `workspace_paths`.
pub fn resolve_tool_path(path_str: &str, data_dir: &Path, workspace_paths: &[PathBuf]) -> PathBuf {
    let p = Path::new(path_str);
    if p.is_absolute() {
        // If this absolute path is within a configured workspace, use it directly.
        for wp in workspace_paths {
            if p.starts_with(wp) {
                return p.to_path_buf();
            }
        }
        // Fall back: strip the leading `/` and resolve relative to data_dir.
        let stripped = p.strip_prefix("/").unwrap_or(p);
        data_dir.join(stripped)
    } else {
        data_dir.join(p)
    }
}
