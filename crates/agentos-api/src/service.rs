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

    /// Permanently remove an agent: profile, identity, memory tiers, scratchpad,
    /// inboxes, checkpoints and schedules. Unlike `disconnect_agent` (which only
    /// marks the profile offline) this works on an offline agent and cannot be
    /// undone. Returns the kernel's wipe summary.
    async fn remove_agent(
        &self,
        agent_id: agentos_types::AgentID,
    ) -> Result<Option<serde_json::Value>, ApiError>;

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

    /// Mark a single notification read. Idempotent: `false` = no such id.
    async fn mark_notification_read(&self, id: NotificationID) -> Result<bool, ApiError>;

    async fn dismiss_notification(&self, id: NotificationID) -> Result<bool, ApiError>;

    async fn clear_read_notifications(&self) -> Result<usize, ApiError>;

    /// Delete every notification except live (unanswered, unexpired) questions.
    async fn clear_all_notifications(&self) -> Result<usize, ApiError>;

    async fn mark_all_notifications_read(&self) -> Result<usize, ApiError>;

    async fn get_unread_count(&self) -> Result<u64, ApiError>;

    /// The notification routing matrix: which event kinds reach which channels.
    async fn get_notification_routes(&self) -> Result<ApiNotificationRoutes, ApiError>;

    /// Partial update of the matrix; unlisted cells are untouched.
    async fn set_notification_routes(
        &self,
        rules: Vec<ApiRouteRule>,
    ) -> Result<ApiNotificationRoutes, ApiError>;

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

    /// Ingest an external provider webhook (`/webhooks/incoming/{id}`): resolve
    /// the endpoint, throttle, verify the provider signature against the stored
    /// secret, and enqueue the event for the owning agent. Errors map to the
    /// status the caller should see (404 unknown/inactive, 429, 401 bad signature).
    async fn receive_webhook(
        &self,
        endpoint_id: &str,
        headers: std::collections::HashMap<String, String>,
        body: Vec<u8>,
    ) -> Result<(), ApiError>;

    /// The WhatsApp webhook GET verify-token for a channel, if configured.
    async fn whatsapp_verify_token(&self, channel_id: &str) -> Result<Option<String>, ApiError>;

    /// Acknowledge a Telegram inline-keyboard tap so the client drops the
    /// button spinner. Best-effort — the default no-op keeps the spinner, never
    /// the approval, since only the kernel impl can reach the bot token.
    async fn telegram_ack_callback(&self, _channel_id: &str, _callback_query_id: &str) {}

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
        remember: bool,
        // `actor`: who resolved it (e.g. `api-key:<name>`); recorded as
        // `granted_by` on a remembered grant.
        actor: String,
    ) -> Result<ResolveEscalationResponse, ApiError>;

    // ── Approval policies (standing grants) ──────────────────────────────────

    async fn list_approval_policies(&self) -> Result<Vec<ApiApprovalPolicy>, ApiError>;

    async fn add_approval_policy(
        &self,
        req: AddApprovalPolicyRequest,
    ) -> Result<ApiApprovalPolicy, ApiError>;

    async fn revoke_approval_policy(&self, id: i64) -> Result<(), ApiError>;

    // ── Workspace grants (folder access) ─────────────────────────────────────

    /// List active workspace grants. With `agent_name`, list the grants that
    /// apply to that agent (its own plus the global ones).
    async fn list_workspace_grants(
        &self,
        agent_name: Option<String>,
    ) -> Result<Vec<ApiWorkspaceGrant>, ApiError>;

    /// Grant a host directory to one agent, or to every agent when
    /// `req.agent_name` is `None`. `actor` is the calling API key's name and is
    /// recorded on the grant and in the audit entry.
    async fn grant_workspace(
        &self,
        req: GrantWorkspaceRequest,
        actor: &str,
    ) -> Result<ApiWorkspaceGrant, ApiError>;

    /// Revoke an active grant matching `(path, agent scope)`. Returns the
    /// number of rows revoked — the unique index makes that 0 or 1, never more:
    /// this revokes one exact path, not a subtree.
    async fn revoke_workspace(
        &self,
        path: String,
        agent_name: Option<String>,
        actor: &str,
    ) -> Result<u64, ApiError>;

    /// Browse or search one memory tier for an agent (read-only). Empty `q`
    /// returns most-recent items; a non-empty `q` searches the tier.
    async fn browse_agent_memory(
        &self,
        agent_id: String,
        tier: String,
        q: Option<String>,
        limit: Option<usize>,
    ) -> Result<Vec<ApiMemoryItem>, ApiError>;

    /// Built-in + catalog LLM providers (read-only). Drives the provider and
    /// model pickers when connecting an agent.
    async fn list_providers(&self) -> Result<Vec<ApiProvider>, ApiError>;

    /// List installed skills (read-only).
    async fn list_skills(&self) -> Result<Vec<ApiSkillSummary>, ApiError>;

    /// Get one installed skill's full detail by name.
    async fn get_skill(&self, name: String) -> Result<ApiSkillDetail, ApiError>;

    /// Agent-to-agent message timeline for an agent (read-only).
    async fn agent_inbox(
        &self,
        agent_id: String,
        limit: Option<usize>,
    ) -> Result<Vec<ApiInboxMessage>, ApiError>;

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
    /// Re-attach a server under the same name with a new configuration.
    async fn update_mcp_server(
        &self,
        _name: &str,
        _req: AttachMcpRequest,
    ) -> Result<McpAttachedResponse, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    // NOTE(2026-08-30): the methods below were declared by the in-progress
    // MCP/channel/plugin/connector REST work (kernel side landed:
    // `oauth_flow.rs`, `commands/connector.rs`) but their `Kernel` impls and
    // handlers were not written before that session was interrupted. Default
    // bodies keep the crate compiling; replace them with real impls in
    // `kernel_impl.rs` when that work resumes.
    async fn attach_mcp_server(
        &self,
        _req: AttachMcpRequest,
    ) -> Result<McpAttachedResponse, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn list_mcp_catalog(
        &self,
        _q: Option<&str>,
    ) -> Result<Vec<ApiMcpCatalogEntry>, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn get_mcp_catalog_entry(&self, _id: &str) -> Result<serde_json::Value, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn install_mcp_server(
        &self,
        _id: &str,
        _req: InstallMcpRequest,
    ) -> Result<McpAttachedResponse, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }

    async fn connect_channel(
        &self,
        _req: ConnectChannelRequest,
    ) -> Result<ApiChannelSummary, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn update_channel(
        &self,
        _id: &str,
        _req: UpdateChannelRequest,
    ) -> Result<ApiChannelSummary, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn test_channel(&self, _id: &str) -> Result<(), ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn set_channel_agent(
        &self,
        _id: &str,
        _agent_name: Option<String>,
    ) -> Result<(), ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn list_pairings(&self) -> Result<ApiPairings, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn approve_pairing(&self, _code: &str) -> Result<ApiPairingEntry, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn approve_pending_pairing(
        &self,
        _channel_id: &str,
        _sender_id: &str,
    ) -> Result<ApiPairingEntry, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn revoke_pairing(&self, _channel_id: &str, _sender_id: &str) -> Result<(), ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }

    async fn install_plugin(&self, _manifest_toml: &str) -> Result<ApiPluginSummary, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn update_plugin(
        &self,
        _id: &str,
        _manifest_toml: &str,
    ) -> Result<ApiPluginSummary, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn remove_plugin(&self, _id: &str) -> Result<(), ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }

    async fn list_connectors(&self) -> Result<Vec<ApiConnectorSummary>, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn get_connector(&self, _id: &str) -> Result<ApiConnectorDetail, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn disconnect_connector(&self, _id: &str) -> Result<(), ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn add_connector(&self, _manifest_toml: &str) -> Result<ApiConnectorDetail, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn update_connector(
        &self,
        _id: &str,
        _manifest_toml: &str,
    ) -> Result<ApiConnectorDetail, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn remove_connector(&self, _id: &str) -> Result<(), ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    /// Begin the OAuth2 PKCE flow; returns the provider authorize URL.
    async fn start_connector_oauth(
        &self,
        _id: &str,
        _redirect_uri: &str,
    ) -> Result<String, ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    /// Complete the flow from the provider callback (state validated by the vault).
    async fn complete_connector_oauth(
        &self,
        _id: &str,
        _code: &str,
        _state: &str,
    ) -> Result<(), ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }
    async fn store_connector_credential(
        &self,
        _id: &str,
        _req: StoreCredentialRequest,
    ) -> Result<(), ApiError> {
        Err(ApiError::NotImplemented(
            "not wired to the kernel yet".into(),
        ))
    }

    async fn list_event_subscriptions(&self) -> Result<Vec<ApiEventSubscription>, ApiError>;
    async fn create_event_subscription(
        &self,
        req: CreateSubscriptionRequest,
    ) -> Result<ApiEventSubscription, ApiError>;
    async fn delete_event_subscription(&self, id: &str) -> Result<(), ApiError>;
    async fn enable_event_subscription(&self, id: &str) -> Result<(), ApiError>;
    async fn disable_event_subscription(&self, id: &str) -> Result<(), ApiError>;
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
    ///
    /// `file_ids` is a comma-separated list of upload ids the composer attached.
    /// They are resolved into typed context parts (text extraction, vision) the
    /// same way the web chat resolves them — without this the panel could attach
    /// a file and the agent would never learn it existed.
    ///
    /// `owner_principal` must be the caller's file-owner identity — the API key
    /// id, which is what `POST /api/v1/files` stamps on every upload. Passing an
    /// empty principal matches only unowned rows (channel media and CLI writes),
    /// so the panel's own attachments would resolve to nothing at all.
    async fn send_chat_message(
        &self,
        session_id: &str,
        text: String,
        file_ids: Option<String>,
        owner_principal: &str,
    ) -> Result<ApiChatMessage, ApiError>;

    /// Streaming variant of [`Self::send_chat_message`]: forwards
    /// `ChatStreamEvent`s (token chunks, tool events, done) to `out_tx` as they
    /// arrive, persisting both the user and assistant turns. The channel is
    /// closed when inference completes.
    async fn stream_chat_message(
        &self,
        session_id: &str,
        text: String,
        file_ids: Option<String>,
        owner_principal: &str,
        out_tx: mpsc::Sender<ChatStreamEvent>,
    ) -> Result<(), ApiError>;

    // ── Agent conversations (multi-agent convos, read-only) ──────────────────

    /// `kind` filters to `"dm"` or `"operator"`; `None` lists every convo.
    async fn list_convos(&self, kind: Option<&str>) -> Result<Vec<ApiConvoSummary>, ApiError>;

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

    /// Reopen a finished conversation for `turns` more agent turns. Returns its
    /// summary and the new turn ceiling to pass to [`Self::run_agent_chat`].
    /// `Conflict` while it is still running (or finishing a stopped turn).
    async fn continue_agent_chat(
        &self,
        id: &str,
        turns: u32,
    ) -> Result<(ApiConvoSummary, u32), ApiError>;

    /// Post an operator message into a conversation. A running conversation
    /// answers it on its next turn (returns `None`); a finished one is reopened
    /// for one round and the new ceiling is returned for the caller to run.
    async fn post_agent_chat_message(
        &self,
        id: &str,
        content: String,
    ) -> Result<(ApiConvoSummary, Option<u32>), ApiError>;

    // ── Realtime (Phase 08) ───────────────────────────────────────────────────

    /// Subscribe to the kernel's coarse realtime event broadcast (for SSE fan-out).
    /// The same stream feeds the WebSocket relay.
    fn subscribe_realtime(&self) -> tokio::sync::broadcast::Receiver<agentos_types::RealtimeEvent>;
}
