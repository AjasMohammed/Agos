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

    /// Kernel pushes a new notification to all subscribers (e.g. web SSE consumer).
    NotificationPush(agentos_types::UserMessage),
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
        #[serde(default)]
        extra_permissions: Vec<String>,
        /// When true, grants full root access to all resources.
        #[serde(default)]
        root: bool,
        /// When true, the kernel skips the pre-flight LLM health check and
        /// registers the agent even if the backend appears unreachable.
        #[serde(default)]
        skip_health_check: bool,
    },
    ListAgents,
    DisconnectAgent {
        agent_id: AgentID,
    },
    /// Change the LLM endpoint URL for a connected agent (takes effect immediately).
    SetAgentBaseUrl {
        name: String,
        url: String,
    },
    /// Probe an LLM backend's reachability without registering an agent.
    /// Builds the adapter exactly as `ConnectAgent` would, runs `health_check`,
    /// and returns a `LLMHealth` response.
    PingLLM {
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
        /// Optional agent name used to look up per-agent vault keys (e.g. `alice_openai_api_key`).
        /// When omitted, falls back to the global key name (e.g. `openai_api_key`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_name: Option<String>,
    },

    // Task management
    RunTask {
        agent_name: Option<String>,
        prompt: String,
        /// When true, runs without iteration/timeout limits (autonomous mode).
        #[serde(default)]
        autonomous: bool,
        /// When true, the task executor skips checkpoint writes (ephemeral execution).
        #[serde(default)]
        no_checkpoint: bool,
        /// Extended thinking level for the task (off/low/medium/high/max).
        #[serde(default)]
        thinking_level: agentos_types::ThinkingLevel,
    },
    ListTasks,
    GetTaskLogs {
        task_id: TaskID,
    },
    CancelTask {
        task_id: TaskID,
    },
    /// Spawn a child task from within a running parent task.
    /// The child inherits a scoped subset of the parent's capabilities.
    SpawnSubAgent {
        /// The parent task that is spawning this child.
        parent_task_id: TaskID,
        /// Name of the registered agent to run the child task.
        agent_name: String,
        /// The prompt / goal for the child task.
        prompt: String,
        /// Permissions requested for the child (intersected with parent's at spawn time).
        #[serde(default)]
        requested_permissions: Vec<String>,
        /// Optional slice of parent context to seed the child's context window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_slice: Option<agentos_types::ContextSlice>,
    },
    /// Wait for a set of child tasks to complete and retrieve their results.
    AwaitSubAgents {
        /// The parent task that is waiting.
        parent_task_id: TaskID,
        /// IDs of child tasks to wait for.
        child_task_ids: Vec<TaskID>,
    },
    /// Execute a named agent team against a goal.
    /// The coordinator agent is spawned automatically; it uses spawn_agent to delegate to workers.
    RunTeam {
        /// JSON-encoded `TeamConfig`.
        config: String,
    },
    /// Get the current status of a running team (by coordinator task ID).
    TeamStatus {
        team_task_id: TaskID,
    },
    /// Resume a task from its latest checkpoint.
    ResumeTask {
        task_id: TaskID,
    },
    /// List all tasks that have checkpoints available for resume.
    ListCheckpoints,
    /// Retrieve the execution trace for a completed task.
    TaskGetTrace {
        task_id: TaskID,
    },
    /// List recent task traces (up to `limit`), optionally filtered by agent.
    TaskListTraces {
        agent_id: Option<AgentID>,
        limit: u32,
    },

    // Tool management
    ListTools,
    InstallTool {
        manifest_path: String,
    },
    /// Hot-reload a tool from an already-written manifest on disk.
    /// Used by `agentos tool add` (registry install) and the web UI marketplace
    /// after writing the manifest to `tools/user/<name>.toml`.
    /// Returns the assigned ToolID on success.
    ToolLoad {
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

    // Notification system (UNIS Phase 1)
    /// Send a fire-and-forget notification to the user.
    /// Requires `user.notify` (write) permission when `from_agent` is set.
    SendUserNotification {
        subject: String,
        body: String,
        priority: agentos_types::NotificationPriority,
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<agentos_types::UserMessageKind>,
        trace_id: agentos_types::TraceID,
        /// Originating agent ID — if set, `user.notify:w` permission is enforced
        /// and the rate limiter is applied.  `None` means kernel-sourced.
        #[serde(skip_serializing_if = "Option::is_none")]
        from_agent: Option<agentos_types::AgentID>,
    },

    /// List notifications from the user inbox.
    ListNotifications {
        unread_only: bool,
        limit: u32,
    },

    /// Fetch a single notification by ID.
    GetNotification {
        notification_id: agentos_types::NotificationID,
    },

    /// Mark a notification as read.
    MarkNotificationRead {
        notification_id: agentos_types::NotificationID,
    },

    /// Submit a response to an interactive notification (Question kind).
    RespondToNotification {
        notification_id: agentos_types::NotificationID,
        response_text: String,
        channel: agentos_types::DeliveryChannel,
    },

    // Channel management (Phase 6)
    /// Register a new bidirectional communication channel.
    ConnectChannel {
        kind: agentos_types::ChannelKind,
        /// Channel-specific external identifier (Telegram chat_id, ntfy topic, email address).
        /// Optional for Telegram — when omitted, the adapter auto-discovers the chat_id
        /// from the first inbound message after the user sends `/start` to the bot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_id: Option<String>,
        display_name: String,
        /// Vault key where the credential (bot token, password) is stored.
        #[serde(default)]
        credential_key: String,
        /// ntfy reply-topic for inbound messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_topic: Option<String>,
        /// ntfy server URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_url: Option<String>,
        /// Public URL for Telegram webhook mode (e.g. "https://example.com").
        /// When set, the adapter calls `setWebhook` instead of long-polling.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        webhook_url: Option<String>,
        /// Default agent for inbound channel chat (`/agent` without retyping each message).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_agent_name: Option<String>,
    },
    /// Set or clear the default chat agent for a connected channel.
    SetChannelActiveAgent {
        channel_id: String,
        /// Agent name, or omit / empty to clear the default.
        #[serde(default)]
        agent_name: Option<String>,
    },
    /// Deregister a channel and stop its listener.
    DisconnectChannel {
        channel_id: String,
    },
    /// List all registered channels.
    ListChannels,
    /// Send a test notification to a registered channel.
    TestChannel {
        channel_id: String,
    },
    // Plugin management
    /// List all discovered plugins with their status.
    ListPlugins,
    /// Activate a plugin by ID.
    EnablePlugin {
        plugin_id: String,
    },
    /// Deactivate a plugin by ID.
    DisablePlugin {
        plugin_id: String,
    },

    /// Query the health status of all configured MCP server connections.
    McpStatus,
    /// Attach a new MCP server to the running kernel at runtime.
    ///
    /// Spawns the server process (stdio) or opens an HTTP connection, performs
    /// the MCP handshake, discovers tools, and registers them with the kernel's
    /// `ToolRunner`. Does not modify `config/default.toml` — the attachment is
    /// ephemeral and lost on kernel restart.
    McpAttach {
        /// Unique name for this server (used in logs, status, and detach).
        name: String,
        /// Executable to spawn for stdio transport (e.g. `"npx"`).
        /// Mutually exclusive with `url`.
        command: Option<String>,
        /// Arguments passed to the executable.
        #[serde(default)]
        args: Vec<String>,
        /// HTTP endpoint URL for HTTP transport. Mutually exclusive with `command`.
        url: Option<String>,
        /// Static Bearer auth token for HTTP transport.
        /// Mutually exclusive with `oauth_connector_id`.
        auth_token: Option<String>,
        /// OAuth2 connector ID referencing a credential stored in the vault via
        /// `McpOAuthStore`. Mutually exclusive with `auth_token`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth_connector_id: Option<String>,
        /// Per-request timeout in seconds (default: 30).
        timeout_secs: Option<u64>,
        /// Environment variables for the subprocess.
        /// Values of the form `"vault:KEY"` are resolved from the kernel vault at attach time.
        #[serde(default)]
        env: std::collections::HashMap<String, String>,
    },
    /// Detach a previously attached (or boot-configured) MCP server.
    ///
    /// Closes the connection, removes the server from the supervisor, and
    /// deletes the persistence record so it is not restored on next restart.
    McpDetach {
        /// Name of the server to detach.
        name: String,
    },
    /// Store an OAuth2 credential in the vault for use with an MCP server.
    ///
    /// The credential is encrypted at rest and can be referenced by `connector_id`
    /// in subsequent `McpAttach` commands via the `oauth_connector_id` field.
    McpOAuthStore {
        /// Unique identifier for this credential (e.g. "zomato").
        connector_id: String,
        /// Human-readable provider name (e.g. "zomato", "github").
        provider: String,
        /// OAuth2 access token.
        access_token: String,
        /// OAuth2 refresh token (used to obtain new access tokens).
        refresh_token: Option<String>,
        /// OAuth2 token endpoint URL (e.g. "https://accounts.zomato.com/oauth/token").
        token_endpoint: String,
        /// OAuth2 client ID registered with the provider.
        client_id: String,
        /// OAuth2 client secret (for confidential clients).
        client_secret: Option<String>,
        /// Scopes granted by this token (e.g. ["order:read", "order:write"]).
        #[serde(default)]
        scopes: Vec<String>,
        /// Token lifetime in seconds (used to compute `expires_at`).
        expires_in_secs: Option<i64>,
    },

    // Agent context memory
    /// Read an agent's context memory document.
    ContextMemoryRead {
        agent_id: String,
    },
    /// Write/replace an agent's context memory document.
    ContextMemoryUpdate {
        agent_id: String,
        content: String,
        reason: Option<String>,
    },
    /// List context memory version history.
    ContextMemoryHistory {
        agent_id: String,
        limit: u32,
    },
    /// Rollback to a specific version (creates a new version).
    ContextMemoryRollback {
        agent_id: String,
        version: u32,
    },
    /// Clear the agent's context memory.
    ContextMemoryClear {
        agent_id: String,
    },
    /// Set context memory from external content (bootstrap).
    ContextMemorySet {
        agent_id: String,
        content: String,
    },

    // Skills management
    /// Install a skill from a directory containing SKILL.toml + prompt.
    SkillInstall {
        path: String,
    },
    /// Remove an installed skill by name.
    SkillRemove {
        name: String,
    },
    /// List all installed skills.
    SkillList,
    /// Run a skill by name with optional input text.
    SkillRun {
        name: String,
        input: Option<String>,
    },
    /// Get the status/details of an installed skill.
    SkillStatus {
        name: String,
    },

    // Provider catalog
    /// List all available LLM providers (built-in + catalog).
    ListProviders,
    /// Override the base URL for a catalog provider (persisted to providers.toml).
    SetProviderUrl {
        name: String,
        url: String,
    },

    // Scratchpad management
    /// List all scratchpad pages for an agent.
    ScratchListPages {
        agent_id: String,
    },
    /// Read a scratchpad page by title.
    ScratchReadPage {
        agent_id: String,
        title: String,
    },
    /// Delete a scratchpad page.
    ScratchDeletePage {
        agent_id: String,
        title: String,
    },
    /// Show the wikilink graph for a page.
    ScratchGraphPage {
        agent_id: String,
        title: String,
        depth: usize,
    },

    // Webhook endpoint management
    /// Create a new webhook endpoint for an agent.
    CreateWebhookEndpoint {
        agent_name: String,
        provider: String,
        debounce_seconds: u64,
    },
    /// List webhook endpoints (optionally filtered by agent).
    ListWebhookEndpoints {
        agent_name: Option<String>,
    },
    /// Delete a webhook endpoint.
    DeleteWebhookEndpoint {
        endpoint_id: String,
    },

    // Container runtime management
    /// Create a new container for an agent.
    ContainerCreate {
        agent_name: String,
        image: String,
        memory_mb: u64,
        cpu: f64,
        network: String,
        ttl_seconds: u64,
    },
    /// Execute a command in a running container (ownership verified).
    ContainerExec {
        agent_name: String,
        container_id: String,
        command: Vec<String>,
        timeout_ms: u64,
    },
    /// Read logs from a container (ownership verified).
    ContainerLogs {
        agent_name: String,
        container_id: String,
        tail: usize,
    },
    /// Destroy a container (ownership verified).
    ContainerDestroy {
        agent_name: String,
        container_id: String,
    },
    /// List containers (optionally filtered by agent).
    ContainerList {
        agent_name: Option<String>,
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
            KernelCommand::ContainerCreate { agent_name, .. } => Some(agent_name.clone()),
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
    Success {
        data: Option<serde_json::Value>,
    },
    Error {
        message: String,
    },
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
    TimerList(Vec<agentos_types::schedule::TimerEntry>),
    TimerId(agentos_types::ScheduleID),
    OnceJobList(Vec<agentos_types::schedule::OnceJob>),
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
    CheckpointList(Vec<serde_json::Value>),

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

    // Notification system (UNIS Phase 1)
    NotificationList(Vec<agentos_types::UserMessage>),
    NotificationSent {
        id: agentos_types::NotificationID,
    },
    /// Single notification fetched by ID; `None` if not found.
    NotificationDetail(Box<Option<agentos_types::UserMessage>>),

    // Channel management (Phase 6)
    ChannelList(Vec<agentos_types::RegisteredChannel>),

    // MCP server health
    McpServerStatusList(Vec<McpServerStatus>),
    /// MCP server successfully attached; includes the names of registered tools.
    McpAttached {
        tool_count: usize,
        tools: Vec<String>,
    },
    /// MCP server successfully detached.
    McpDetached,
    /// OAuth credential successfully stored in the vault.
    McpOAuthStored {
        connector_id: String,
    },

    // Provider catalog
    ProviderList(Vec<serde_json::Value>),

    // Skills
    SkillList(Vec<serde_json::Value>),
    SkillRunResult {
        task_id: String,
    },
    SkillStatusInfo(serde_json::Value),

    // Task trace / debugger
    TaskTrace(Box<agentos_types::TaskTrace>),
    TaskTraces(Vec<agentos_types::TaskTraceSummary>),

    // Sub-agent coordination
    /// A sub-agent task was successfully spawned.
    SubAgentSpawned {
        child_task_id: TaskID,
    },
    /// Results from awaited sub-agent tasks.
    SubAgentResults {
        /// (child_task_id, result_summary) pairs.
        results: Vec<(TaskID, String)>,
    },

    // Agent teams
    /// A team run was successfully started.
    TeamStarted {
        /// Task ID of the coordinator agent.
        coordinator_task_id: TaskID,
        /// Task IDs of pre-spawned workers (empty if workers are spawned dynamically).
        worker_task_ids: Vec<TaskID>,
    },

    // Webhook endpoints
    WebhookEndpointList {
        endpoints: Vec<agentos_types::WebhookEndpointMeta>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
    pub tool_count: usize,
    pub last_error: Option<String>,
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
