use crate::error::ApiError;
use crate::types::*;
use agentos_audit::AuditEventType;
use agentos_kernel::ChatStreamEvent;
use agentos_types::{NotificationID, SecretMetadata, TaskID, ToolID};
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Result of verifying an operator login credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialCheck {
    /// Credential matched the configured operator token.
    Valid,
    /// Credential did not match.
    Invalid,
    /// No operator token is configured — login is disabled server-side.
    NotConfigured,
}

/// Core service trait defining the complete API surface for interacting with
/// the AgentOS kernel. Implemented by `Kernel` in `kernel_impl.rs`.
///
/// Every method returns `Result<T, ApiError>` so transport layers (HTTP, gRPC,
/// WebSocket) can translate errors uniformly.
#[async_trait]
pub trait KernelService: Send + Sync {
    // ── Agents ──────────────────────────────────────────────────────────────

    async fn list_agents(&self) -> Result<Vec<ApiAgentSummary>, ApiError>;

    async fn connect_agent(&self, req: ConnectAgentRequest) -> Result<ApiAgentSummary, ApiError>;

    async fn disconnect_agent(&self, agent_id: agentos_types::AgentID) -> Result<(), ApiError>;

    async fn get_agent_detail(&self, name: &str) -> Result<ApiAgentDetail, ApiError>;

    async fn update_agent_settings(&self, req: UpdateAgentSettingsRequest) -> Result<(), ApiError>;

    async fn grant_permission(&self, req: PermissionRequest) -> Result<(), ApiError>;

    async fn revoke_permission(&self, req: PermissionRequest) -> Result<(), ApiError>;

    // ── Tasks ───────────────────────────────────────────────────────────────

    async fn list_tasks(&self, filter: TaskFilter) -> Result<(Vec<ApiTaskSummary>, u64), ApiError>;

    async fn get_task(&self, id: TaskID) -> Result<ApiTaskDetail, ApiError>;

    async fn run_task(&self, req: RunTaskRequest) -> Result<TaskID, ApiError>;

    async fn cancel_task(&self, id: TaskID) -> Result<(), ApiError>;

    async fn get_task_trace(
        &self,
        id: TaskID,
    ) -> Result<agentos_types::task_trace::TaskTrace, ApiError>;

    // ── Tools ───────────────────────────────────────────────────────────────

    async fn list_tools(&self) -> Result<Vec<ApiToolSummary>, ApiError>;

    async fn install_tool(&self, req: InstallToolRequest) -> Result<ToolID, ApiError>;

    async fn remove_tool(&self, name: &str) -> Result<(), ApiError>;

    // ── Secrets ─────────────────────────────────────────────────────────────

    async fn list_secrets(&self) -> Result<Vec<SecretMetadata>, ApiError>;

    async fn set_secret(&self, req: SetSecretRequest) -> Result<(), ApiError>;

    async fn revoke_secret(&self, name: &str) -> Result<(), ApiError>;

    // ── Chat ────────────────────────────────────────────────────────────────

    /// Whether the agent's active LLM adapter accepts image parts (vision).
    async fn agent_supports_images(&self, agent_name: &str) -> Result<bool, ApiError>;

    async fn chat_send(&self, req: ChatRequest) -> Result<ChatResponse, ApiError>;

    /// Streaming chat: spawns inference and sends `ChatStreamEvent`s to the
    /// provided channel. The channel is closed when inference is complete.
    async fn chat_stream(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<ChatStreamEvent>,
    ) -> Result<(), ApiError>;

    // ── Pipelines ───────────────────────────────────────────────────────────

    async fn list_pipelines(&self) -> Result<Vec<ApiPipelineSummary>, ApiError>;

    async fn save_pipeline(&self, req: SavePipelineRequest) -> Result<(), ApiError>;

    async fn run_pipeline(&self, req: RunPipelineRequest) -> Result<serde_json::Value, ApiError>;

    async fn delete_pipeline(&self, name: &str) -> Result<(), ApiError>;

    // ── Audit ───────────────────────────────────────────────────────────────

    async fn query_audit(&self, filter: AuditFilter) -> Result<Vec<AuditEntrySummary>, ApiError>;

    async fn get_audit_detail(&self, trace_id: &str) -> Result<AuditEntryDetail, ApiError>;

    // ── Costs ───────────────────────────────────────────────────────────────

    async fn get_cost_summary(&self) -> Result<Vec<CostSummaryEntry>, ApiError>;

    async fn get_agent_costs(&self, agent_name: &str) -> Result<CostSummaryEntry, ApiError>;

    // ── Notifications ───────────────────────────────────────────────────────

    async fn list_notifications(
        &self,
        filter: NotificationFilter,
    ) -> Result<Vec<NotificationSummary>, ApiError>;

    async fn get_notification(
        &self,
        id: NotificationID,
    ) -> Result<agentos_types::UserMessage, ApiError>;

    async fn respond_to_notification(
        &self,
        id: NotificationID,
        text: String,
    ) -> Result<(), ApiError>;

    async fn dismiss_notification(&self, id: NotificationID) -> Result<bool, ApiError>;

    async fn clear_read_notifications(&self) -> Result<usize, ApiError>;

    async fn get_unread_count(&self) -> Result<u64, ApiError>;

    // ── Dashboard (composite) ───────────────────────────────────────────────

    async fn get_dashboard_summary(&self) -> Result<DashboardSummary, ApiError>;

    // ── System ──────────────────────────────────────────────────────────────

    async fn get_status(&self) -> Result<SystemStatus, ApiError>;

    async fn get_uptime(&self) -> std::time::Duration;

    // ── Webhooks ───────────────────────────────────────────────────────────

    /// Verify a webhook secret token for a given channel instance ID.
    /// Returns `true` if the secret matches the stored one.
    async fn verify_webhook_secret(&self, channel_id: &str, secret: &str)
        -> Result<bool, ApiError>;

    /// Returns the configured `external_id` for a channel (e.g. Telegram `chat_id`).
    ///
    /// Used by webhook handlers to ignore traffic from chats other than the pinned
    /// recipient. An empty string means auto-discovery is still in progress.
    async fn channel_pinned_external_id(
        &self,
        channel_id: &str,
    ) -> Result<Option<String>, ApiError>;

    /// Forward a raw inbound message from a webhook to the kernel's inbound router.
    async fn forward_webhook_message(
        &self,
        message: agentos_kernel::notification_router::InboundMessage,
    ) -> Result<(), ApiError>;

    /// Verify a WhatsApp Cloud API `X-Hub-Signature-256` over the raw body using
    /// the channel's app secret (vault `{credential_key}.app_secret`).
    async fn verify_whatsapp_signature(
        &self,
        channel_id: &str,
        body: &[u8],
        signature: &str,
    ) -> Result<bool, ApiError>;

    /// The WhatsApp webhook GET verify-token for a channel, if configured.
    async fn whatsapp_verify_token(&self, channel_id: &str) -> Result<Option<String>, ApiError>;

    // ── Control-plane auth (React control panel) ─────────────────────────────

    /// Verify an operator login credential (constant-time) against the configured
    /// `[api] operator_token`. Backs `POST /api/v1/auth/login`.
    async fn verify_operator_credential(&self, credential: &str) -> CredentialCheck;

    /// Record a control-plane audit event (login attempts, key issue/revoke).
    ///
    /// `details` must **never** contain secret material (raw API keys or the
    /// login credential) — only non-secret identifiers like the public key id.
    async fn record_audit(&self, event_type: AuditEventType, details: serde_json::Value);

    // ── Escalations (HITL) ───────────────────────────────────────────────────

    async fn list_escalations(&self, pending_only: bool) -> Result<Vec<ApiEscalation>, ApiError>;

    async fn get_escalation(&self, id: u64) -> Result<ApiEscalation, ApiError>;

    async fn resolve_escalation(
        &self,
        id: u64,
        decision: String,
        note: Option<String>,
    ) -> Result<ResolveEscalationResponse, ApiError>;

    // ── User-preference proposals (governance) ───────────────────────────────

    async fn list_pref_proposals(
        &self,
        status: String,
        limit: u32,
    ) -> Result<Vec<ApiPrefProposal>, ApiError>;

    async fn accept_pref_proposal(&self, id: String) -> Result<(), ApiError>;

    async fn reject_pref_proposal(&self, id: String) -> Result<(), ApiError>;

    async fn pref_proposal_stats(&self) -> Result<ApiProposalStats, ApiError>;

    // ── Roles (governance) ───────────────────────────────────────────────────

    async fn list_roles(&self) -> Result<Vec<ApiRole>, ApiError>;

    async fn create_role(&self, req: CreateRoleRequest) -> Result<ApiRole, ApiError>;

    async fn get_role(&self, name: &str) -> Result<ApiRole, ApiError>;

    async fn delete_role(&self, name: &str) -> Result<(), ApiError>;

    // ── Audit integrity ──────────────────────────────────────────────────────

    async fn verify_audit_chain(&self) -> Result<serde_json::Value, ApiError>;

    // ── Observability & system (config / doctor / logs / resources / hal) ────

    /// Full config tree as JSON with secret-bearing leaves redacted.
    async fn get_config_tree(&self) -> Result<serde_json::Value, ApiError>;

    /// Resolve a dotted config key (e.g. `"llm.primary"`) from the live file.
    async fn get_config_key(&self, key: &str) -> Result<serde_json::Value, ApiError>;

    /// Write a dotted config key to the live file (preserving comments).
    async fn set_config_key(&self, key: &str, value: serde_json::Value) -> Result<(), ApiError>;

    /// Whether `[api] config_writable` is enabled.
    fn config_writable(&self) -> bool;

    async fn run_doctor(&self) -> Result<Vec<DoctorCheck>, ApiError>;

    async fn apply_doctor_fix(&self, check: &str) -> Result<(), ApiError>;

    async fn query_logs(
        &self,
        level: Option<String>,
        since: Option<String>,
        limit: u32,
    ) -> Result<Vec<LogLine>, ApiError>;

    async fn get_resources(&self) -> Result<ResourceInfo, ApiError>;

    async fn get_hal_info(&self) -> Result<HalInfo, ApiError>;

    // ── Automation (Phase 03) ────────────────────────────────────────────────

    async fn resume_task(&self, id: TaskID) -> Result<serde_json::Value, ApiError>;
    async fn list_task_checkpoints(
        &self,
        id: TaskID,
    ) -> Result<Vec<ApiCheckpointSummary>, ApiError>;

    async fn import_pipeline(&self, yaml: String) -> Result<String, ApiError>;
    async fn export_pipeline(&self, name: &str) -> Result<String, ApiError>;
    async fn get_pipeline_definition(&self, name: &str) -> Result<serde_json::Value, ApiError>;
    async fn get_pipeline_run(&self, run_id: String) -> Result<serde_json::Value, ApiError>;

    async fn list_schedules(&self) -> Result<Vec<ApiScheduleSummary>, ApiError>;
    async fn create_schedule(
        &self,
        req: CreateScheduleRequest,
    ) -> Result<ApiScheduleSummary, ApiError>;
    async fn pause_schedule(&self, id: &str) -> Result<(), ApiError>;
    async fn resume_schedule(&self, id: &str) -> Result<(), ApiError>;
    async fn delete_schedule(&self, id: &str) -> Result<(), ApiError>;
    async fn get_schedule_runs(
        &self,
        id: &str,
        limit: u32,
    ) -> Result<Vec<ApiScheduleRun>, ApiError>;

    async fn list_workflows(&self) -> Result<Vec<ApiWorkflowSummary>, ApiError>;
    async fn get_workflow(&self, id: &str) -> Result<serde_json::Value, ApiError>;
    async fn save_workflow(&self, req: SaveWorkflowRequest) -> Result<String, ApiError>;
    async fn delete_workflow(&self, id: &str) -> Result<(), ApiError>;

    // ── Extensibility (Phase 05) ─────────────────────────────────────────────

    async fn list_plugins(&self) -> Result<Vec<ApiPluginSummary>, ApiError>;
    async fn get_plugin(&self, id: &str) -> Result<ApiPluginDetail, ApiError>;
    async fn discover_plugins(&self) -> Result<DiscoverPluginsResponse, ApiError>;
    async fn set_plugin_enabled(&self, id: &str, enabled: bool) -> Result<(), ApiError>;

    async fn list_channels(&self) -> Result<Vec<ApiChannelSummary>, ApiError>;
    async fn get_channel(&self, id: &str) -> Result<ApiChannelSummary, ApiError>;
    async fn disconnect_channel(&self, id: &str) -> Result<(), ApiError>;

    async fn list_mcp_servers(&self) -> Result<Vec<ApiMcpServer>, ApiError>;
    async fn detach_mcp_server(&self, name: &str) -> Result<(), ApiError>;

    async fn list_connectors(&self) -> Result<Vec<ApiConnectorSummary>, ApiError>;
    async fn get_connector(&self, id: &str) -> Result<ApiConnectorDetail, ApiError>;
    async fn disconnect_connector(&self, id: &str) -> Result<(), ApiError>;

    async fn list_event_subscriptions(&self) -> Result<Vec<ApiEventSubscription>, ApiError>;
    async fn create_event_subscription(
        &self,
        req: CreateSubscriptionRequest,
    ) -> Result<ApiEventSubscription, ApiError>;
    async fn delete_event_subscription(&self, id: &str) -> Result<(), ApiError>;
    async fn emit_event(&self, req: EmitEventRequest) -> Result<(), ApiError>;

    async fn list_webhooks(&self) -> Result<Vec<ApiWebhookEndpoint>, ApiError>;
    async fn create_webhook(
        &self,
        req: CreateWebhookRequest,
    ) -> Result<WebhookSecretResponse, ApiError>;
    async fn rotate_webhook(&self, id: &str) -> Result<WebhookSecretResponse, ApiError>;
    async fn delete_webhook(&self, id: &str) -> Result<(), ApiError>;

    async fn get_agent_identity(&self, name: &str) -> Result<ApiAgentIdentity, ApiError>;

    // ── Files (Phase 06) ──────────────────────────────────────────────────────

    async fn upload_file(
        &self,
        owner: &str,
        original_name: &str,
        mime: &str,
        scope: &str,
        tags: &[String],
        bytes: Vec<u8>,
    ) -> Result<ApiFileMeta, ApiError>;

    async fn list_files(
        &self,
        owner: &str,
        scope: Option<&str>,
        tag: Option<&str>,
        q: Option<&str>,
    ) -> Result<Vec<ApiFileMeta>, ApiError>;

    async fn get_file(&self, owner: &str, id: &str) -> Result<ApiFileMeta, ApiError>;

    /// Returns `(mime, original_name, bytes)` with a download-safe MIME.
    async fn download_file(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<(String, String, Vec<u8>), ApiError>;

    async fn delete_file(&self, owner: &str, id: &str) -> Result<(), ApiError>;

    // ── Scratchpad (Phase 06) ─────────────────────────────────────────────────

    async fn get_scratchpad(&self, agent_id: &str) -> Result<Vec<ApiPageSummary>, ApiError>;

    async fn get_scratchpad_page(
        &self,
        agent_id: &str,
        title: &str,
    ) -> Result<ApiScratchPage, ApiError>;

    async fn save_scratchpad_page(
        &self,
        agent_id: &str,
        title: &str,
        content: String,
        tags: Vec<String>,
    ) -> Result<ApiScratchPage, ApiError>;

    async fn delete_scratchpad_page(&self, agent_id: &str, title: &str) -> Result<(), ApiError>;

    // ── Chat sessions (Phase 02 Conversational) ──────────────────────────────

    async fn list_chat_sessions(&self) -> Result<Vec<ApiChatSessionSummary>, ApiError>;

    async fn create_chat_session(
        &self,
        req: CreateChatSessionRequest,
    ) -> Result<ApiChatSessionDetail, ApiError>;

    async fn get_chat_session(&self, id: &str) -> Result<ApiChatSessionDetail, ApiError>;

    async fn rename_chat_session(&self, id: &str, title: Option<String>) -> Result<(), ApiError>;

    async fn delete_chat_session(&self, id: &str) -> Result<(), ApiError>;

    async fn fork_chat_session(&self, id: &str, title: Option<String>) -> Result<String, ApiError>;

    /// Returns `(bytes, content_type, filename)` for a downloadable export.
    async fn export_chat_session(
        &self,
        id: &str,
        format: &str,
    ) -> Result<(Vec<u8>, String, String), ApiError>;

    async fn get_chat_messages(&self, id: &str) -> Result<Vec<ApiChatMessage>, ApiError>;

    /// Send a user message to a session, run inference, persist both turns, and
    /// return the assistant reply (non-streaming).
    async fn send_chat_message(
        &self,
        session_id: &str,
        text: String,
    ) -> Result<ApiChatMessage, ApiError>;

    /// Streaming variant of [`Self::send_chat_message`]: forwards
    /// `ChatStreamEvent`s (token chunks, tool events, done) to `out_tx` as they
    /// arrive, persisting both the user and assistant turns. The channel is
    /// closed when inference completes.
    async fn stream_chat_message(
        &self,
        session_id: &str,
        text: String,
        out_tx: mpsc::Sender<ChatStreamEvent>,
    ) -> Result<(), ApiError>;

    // ── Agent conversations (multi-agent convos, read-only) ──────────────────

    async fn list_convos(&self) -> Result<Vec<ApiConvoSummary>, ApiError>;

    async fn get_convo(&self, id: &str) -> Result<ApiConvoDetail, ApiError>;

    /// Create a multi-agent conversation record and return its summary. The
    /// orchestration loop is started separately via [`Self::run_agent_chat`].
    async fn create_agent_chat(
        &self,
        topic: String,
        participants: Vec<String>,
        max_turns: u32,
    ) -> Result<ApiConvoSummary, ApiError>;

    /// Run the turn-by-turn orchestration loop for a conversation to completion
    /// (each participant responds in round-robin order). Intended to be spawned
    /// as a background task; persists each turn and the final status.
    async fn run_agent_chat(
        &self,
        id: &str,
        topic: String,
        participants: Vec<String>,
        max_turns: u32,
    );

    /// Request a running conversation to stop after its current turn.
    async fn stop_agent_chat(&self, id: &str) -> Result<(), ApiError>;

    // ── Realtime (Phase 08) ───────────────────────────────────────────────────

    /// Subscribe to the kernel's coarse realtime event broadcast (for SSE fan-out).
    /// The same stream feeds the WebSocket relay.
    fn subscribe_realtime(&self) -> tokio::sync::broadcast::Receiver<agentos_types::RealtimeEvent>;
}
