use agentos_audit::AuditEntry;
use agentos_types::*;
use serde::{Deserialize, Serialize};

/// Messages sent over the bus. This is the top-level envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BusMessage {
    /// CLI/tool sends an intent to the kernel
    Intent(IntentMessage),

    /// Kernel sends a result back to CLI/tool
    IntentResult(IntentResult),

    /// CLI sends a command to the kernel (non-intent operations)
    Command(KernelCommand),

    /// Kernel sends a response to a command
    CommandResponse(KernelResponse),

    /// Kernel pushes a status update (for task monitoring)
    StatusUpdate(StatusUpdate),
}

/// Commands from CLI to kernel that aren't task intents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KernelCommand {
    // Agent management
    ConnectAgent {
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
    },
    ListAgents,
    DisconnectAgent {
        agent_id: AgentID,
    },

    // Task management
    RunTask {
        agent_name: Option<String>,
        prompt: String,
    },
    ListTasks,
    GetTaskLogs {
        task_id: TaskID,
    },
    CancelTask {
        task_id: TaskID,
    },

    // Tool management
    ListTools,
    InstallTool {
        manifest_path: String,
    },
    RemoveTool {
        tool_name: String,
    },

    // Secret management
    SetSecret {
        name: String,
        value: String, // encrypted in transit? No — UDS is local-only
        scope: SecretScope,
    },
    ListSecrets,
    RevokeSecret {
        name: String,
    },
    RotateSecret {
        name: String,
        new_value: String,
    },

    // Permission management
    GrantPermission {
        agent_name: String,
        permission: String, // e.g. "fs.user_data:rw"
    },
    RevokePermission {
        agent_name: String,
        permission: String,
    },
    ShowPermissions {
        agent_name: String,
    },

    // Permissions & Roles
    CreateRole {
        role_name: String,
        description: String,
    },
    DeleteRole {
        role_name: String,
    },
    ListRoles,
    RoleGrant {
        role_name: String,
        permission: String,
    },
    RoleRevoke {
        role_name: String,
        permission: String,
    },
    AssignRole {
        agent_name: String,
        role_name: String,
    },
    RemoveRole {
        agent_name: String,
        role_name: String,
    },

    // Permission Profiles (Advanced)
    CreatePermProfile {
        name: String,
        description: String,
        permissions: Vec<String>,
    },
    DeletePermProfile {
        name: String,
    },
    ListPermProfiles,
    AssignPermProfile {
        agent_name: String,
        profile_name: String,
    },
    GrantPermissionTimed {
        agent_name: String,
        permission: String,
        expires_secs: u64,
    },

    // Agent Communication
    SendAgentMessage {
        from_name: String,
        to_name: String,
        content: String,
    },
    ListAgentMessages {
        agent_name: String,
        limit: u32,
    },
    CreateAgentGroup {
        group_name: String,
        members: Vec<String>,
    },
    BroadcastToGroup {
        group_name: String,
        content: String,
    },

    // System
    GetStatus,
    GetAuditLogs {
        limit: u32,
    },
    Shutdown,

    // Schedule (agentd)
    CreateSchedule {
        name: String,
        cron: String,
        agent_name: String,
        task: String,
        permissions: Vec<String>,
    },
    ListSchedules,
    PauseSchedule {
        name: String,
    },
    ResumeSchedule {
        name: String,
    },
    DeleteSchedule {
        name: String,
    },

    // Background (agentd)
    RunBackground {
        name: String,
        agent_name: String,
        task: String,
        detach: bool,
    },
    ListBackground,
    GetBackgroundLogs {
        name: String,
        follow: bool,
    },
    KillBackground {
        name: String,
    },

    // Pipeline management
    InstallPipeline {
        yaml: String,
    },
    RunPipeline {
        name: String,
        input: String,
        detach: bool,
    },
    PipelineStatus {
        name: String,
        run_id: String,
    },
    PipelineList,
    PipelineLogs {
        name: String,
        run_id: String,
        step_id: String,
    },
    RemovePipeline {
        name: String,
    },
}

/// Responses from kernel to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KernelResponse {
    Success { data: Option<serde_json::Value> },
    Error { message: String },
    AgentList(Vec<AgentProfile>),
    TaskList(Vec<TaskSummary>),
    TaskLogs(Vec<String>),
    ToolList(Vec<agentos_types::ToolManifest>),
    SecretList(Vec<SecretMetadata>),
    Permissions(agentos_types::PermissionSet),
    RoleList(Vec<agentos_types::role::Role>),
    Status(SystemStatus),
    AuditLogs(Vec<AuditEntry>),
    AgentMessageList(Vec<AgentMessage>),
    PermProfileList(Vec<agentos_capability::profiles::PermissionProfile>),

    // agentd
    ScheduleList(Vec<agentos_types::schedule::ScheduledJob>),
    ScheduleId(agentos_types::ScheduleID),
    BackgroundPoolList(Vec<agentos_types::schedule::BackgroundTask>),
    BackgroundLogs(Vec<String>),

    // Pipeline
    PipelineList(Vec<serde_json::Value>),
    PipelineRunStatus(serde_json::Value),
    PipelineStepLogs(Vec<serde_json::Value>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub uptime_secs: u64,
    pub connected_agents: u32,
    pub active_tasks: u32,
    pub installed_tools: u32,
    pub total_audit_entries: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusUpdate {
    pub task_id: TaskID,
    pub state: TaskState,
    pub message: String,
}
