use crate::error::ApiError;
use crate::types::*;
use agentos_kernel::ChatStreamEvent;
use agentos_types::{NotificationID, SecretMetadata, TaskID, ToolID};
use async_trait::async_trait;
use tokio::sync::mpsc;

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
}
