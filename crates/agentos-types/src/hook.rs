use crate::ids::{AgentID, TaskID};
use serde::{Deserialize, Serialize};

/// All lifecycle points at which hooks can fire.
/// Marked `#[non_exhaustive]` so adding new variants is not a breaking change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HookEvent {
    /// Before a task starts execution.
    TaskStart { task_id: TaskID, agent_id: AgentID },
    /// After a task completes (success or failure).
    TaskEnd {
        task_id: TaskID,
        agent_id: AgentID,
        success: bool,
    },
    /// Before a tool is invoked.
    /// Aborting this event cancels the tool call.
    ToolPre {
        task_id: TaskID,
        agent_id: AgentID,
        /// Registered tool name (matches `ToolManifest.manifest.name`).
        /// Use this for registry lookup — `ToolID` is not available at the fire site.
        tool_name: String,
        /// JSON-serialized tool input.
        input_json: String,
    },
    /// After a tool returns.
    ToolPost {
        task_id: TaskID,
        agent_id: AgentID,
        tool_name: String,
        /// JSON-serialized tool output.
        output_json: String,
        duration_ms: u64,
    },
    /// Before memory is searched.
    MemorySearch { query: String },
    /// After memory is written.
    MemoryWrite { content: String, tier: String },
    /// A child agent was spawned from a task.
    AgentSpawned {
        parent_task: TaskID,
        child_agent: AgentID,
    },
    /// A checkpoint was written.
    CheckpointWritten { task_id: TaskID },
    /// A channel message was received.
    ChannelMessageReceived { channel_id: String, sender: String },
    /// A channel message was sent.
    ChannelMessageSent {
        channel_id: String,
        recipient: String,
    },
    /// Config file changed on disk.
    ConfigReloaded,
    /// Kernel is shutting down.
    Shutdown,
    /// A plugin was activated (tools loaded, status → Active).
    PluginActivated { plugin_id: String },
    /// A plugin was deactivated (tools unloaded, status → Disabled).
    PluginDeactivated { plugin_id: String },
}

/// Return value from a hook invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookResult {
    /// Continue with the operation normally.
    Continue,
    /// Abort the operation with a reason string.
    /// Only meaningful for Pre-hooks (e.g., `ToolPre`).
    /// Post-hooks and informational hooks that return `Abort` are treated as `Continue`.
    Abort(String),
}

impl HookResult {
    pub fn is_abort(&self) -> bool {
        matches!(self, HookResult::Abort(_))
    }
}
