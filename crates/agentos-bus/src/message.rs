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
        /// Roles assigned to the agent; defaults to ["general"] if empty.
        #[serde(default)]
        roles: Vec<String>,
        /// When true, the agent is immediately given an ecosystem-testing prompt
        /// instead of starting idle. Used for evaluating AgentOS usability.
        #[serde(default)]
        test_mode: bool,
        /// Extra permissions to grant on connect (format: "resource:flags", e.g. "process.exec:x").
        #[serde(default)]
        extra_permissions: Vec<String>,
    },
    ListAgents,
    DisconnectAgent {
        agent_id: AgentID,
    },

    // Task management
    RunTask {
        agent_name: Option<String>,
        prompt: String,
        /// When true, runs without iteration/timeout limits (autonomous mode).
        #[serde(default)]
        autonomous: bool,
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
        /// Raw scope string from CLI (e.g. "agent:notifier") for kernel-side resolution.
        #[serde(default)]
        scope_raw: Option<String>,
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
        from_name: String,
        group_name: String,
        content: String,
    },

    // System
    GetStatus,
    GetAuditLogs {
        limit: u32,
    },
    VerifyAuditChain {
        from_seq: Option<i64>,
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

    // Escalation management
    ListEscalations {
        pending_only: bool,
    },
    GetEscalation {
        id: u64,
    },
    ResolveEscalation {
        id: u64,
        decision: String,
    },

    // Cost management
    GetCostReport {
        agent_name: Option<String>,
    },
    GetRetrievalMetrics,

    // Pipeline management
    InstallPipeline {
        yaml: String,
    },
    RunPipeline {
        name: String,
        input: String,
        detach: bool,
        /// Agent whose permissions govern pipeline execution. Required.
        agent_name: Option<String>,
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

    // Resource arbitration (Spec §8)
    ListResourceLocks,
    ReleaseResourceLock {
        resource_id: String,
        agent_name: String,
    },
    ReleaseAllResourceLocks {
        agent_name: String,
    },

    // Checkpoint / Rollback (Spec §5)
    ListSnapshots {
        task_id: TaskID,
    },
    RollbackTask {
        task_id: TaskID,
        /// Snapshot reference (e.g. "snap_0001"). None = most recent.
        snapshot_ref: Option<String>,
    },

    // Vault lockdown (Spec §3)
    VaultLockdown,

    // Identity management (Spec §10)
    IdentityShow {
        agent_name: String,
    },
    IdentityRevoke {
        agent_name: String,
    },

    // Audit export
    ExportAuditChain {
        limit: Option<u32>,
    },

    // Resource contention
    ResourceContention,

    // Hardware Abstraction Layer (Spec §9)
    HalListDevices,
    HalApproveDevice {
        device_id: String,
        agent_name: String,
    },
    HalDenyDevice {
        device_id: String,
    },
    HalRevokeDevice {
        device_id: String,
        agent_name: String,
    },
    HalRegisterDevice {
        device_id: String,
        device_type: String,
    },

    // Event system
    EventSubscribe {
        agent_name: String,
        /// Event type filter: "all", "category:AgentLifecycle", or exact like "AgentAdded"
        event_filter: String,
        /// Optional payload predicate (e.g. "cpu_percent > 85 AND severity == Critical")
        payload_filter: Option<String>,
        /// Optional throttle: "none", "once_per:30s", "max:5/60s"
        throttle: Option<String>,
        /// Subscription priority: "critical", "high", "normal", "low"
        priority: Option<String>,
    },
    EventUnsubscribe {
        subscription_id: String,
    },
    EventListSubscriptions {
        agent_name: Option<String>,
    },
    EventGetSubscription {
        subscription_id: String,
    },
    EventEnableSubscription {
        subscription_id: String,
    },
    EventDisableSubscription {
        subscription_id: String,
    },
    EventHistory {
        last: u32,
    },

    // Logging control
    /// Dynamically update the active log filter level at runtime.
    /// Accepts any `EnvFilter`-compatible string, e.g. "debug", "warn",
    /// "agentos=debug,agentos_kernel=trace".
    SetLogLevel {
        level: String,
    },
}

impl KernelCommand {
    /// Returns an agent-identifying key for per-agent rate limiting, if the command
    /// targets a specific agent. Returns `None` for agent-agnostic commands.
    pub fn agent_key(&self) -> Option<String> {
        match self {
            // Agent-targeting commands that can be issued repeatedly — rate limit per agent name.
            KernelCommand::RunTask {
                agent_name: Some(name),
                ..
            } => Some(name.clone()),
            KernelCommand::ConnectAgent { name, .. } => Some(name.clone()),
            KernelCommand::GrantPermission { agent_name, .. } => Some(agent_name.clone()),
            KernelCommand::RevokePermission { agent_name, .. } => Some(agent_name.clone()),
            KernelCommand::SendAgentMessage { from_name, .. } => Some(from_name.clone()),
            KernelCommand::BroadcastToGroup { from_name, .. } => Some(from_name.clone()),
            KernelCommand::EventSubscribe { agent_name, .. } => Some(agent_name.clone()),
            KernelCommand::RunPipeline {
                agent_name: Some(name),
                ..
            } => Some(name.clone()),
            // DisconnectAgent is intentionally excluded: it is a one-shot cleanup op and its
            // agent_id (UUID) differs from the name-keyed limiter, which would cause a
            // separate entry that never gets evicted, leaking memory.
            _ => None,
        }
    }
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

    // Escalation
    EscalationList(Vec<serde_json::Value>),

    // Cost
    CostReport(Vec<agentos_types::CostSnapshot>),

    // Pipeline
    PipelineList(Vec<serde_json::Value>),
    PipelineRunStatus(serde_json::Value),
    PipelineStepLogs(Vec<serde_json::Value>),

    // Resource arbitration
    ResourceLockList(Vec<serde_json::Value>),

    // Checkpoint / Rollback
    SnapshotList(Vec<serde_json::Value>),

    // Audit export
    AuditChainExport(String),

    // Resource contention
    ResourceContentionStats(serde_json::Value),

    // Event system
    EventSubscriptionId(String),
    EventSubscriptionList(Vec<serde_json::Value>),
    EventHistoryList(Vec<serde_json::Value>),

    // Hardware Abstraction Layer
    HalDeviceList(Vec<serde_json::Value>),
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
