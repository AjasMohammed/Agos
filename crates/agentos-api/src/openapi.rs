//! OpenAPI 3.1 document for the AgentOS REST API.
//!
//! The spec is generated from `#[utoipa::path]` annotations on handlers and
//! `#[derive(ToSchema)]` on DTOs. It is served at `GET /api/v1/openapi.json`
//! (with a Scalar UI at `GET /api/v1/docs`) and emitted to the committed
//! `crates/agentos-api/openapi.json` by the `gen-openapi` binary — the contract
//! consumed by the standalone React control panel.

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// Adds the `bearer_auth` security scheme to the generated document.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("agos_<key>")
                        .description(Some(
                            "AgentOS API key presented as a Bearer token, e.g. \
                             `Authorization: Bearer agos_<key>`.",
                        ))
                        .build(),
                ),
            );
        }
    }
}

/// The complete OpenAPI document for `/api/v1/*`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "AgentOS API",
        version = "0.1.0",
        description = "REST + WebSocket control-plane API for AgentOS.\n\n\
            **Response envelope:** all success responses are wrapped in \
            `{ \"data\": ... }`; paginated list endpoints also include a \
            `{ \"meta\": { \"total\": N } }` block. \
            Errors use `{ \"error\": { \"code\", \"message\", \"status\" } }`.\n\n\
            **Auth:** send `Authorization: Bearer agos_<key>` on protected routes. \
            The WebSocket endpoint authenticates via a `?token=agos_<key>` query parameter.\n\n\
            The OpenAI-compatible `POST /api/v1/chat/completions` returns its native shape \
            (not the `{ data }` envelope) and streams `text/event-stream` chunks when `stream` is true."
    ),
    paths(
        crate::handlers::system::health,
        crate::handlers::system::status,
        crate::handlers::chat::completions,
        crate::handlers::chat_sessions::list,
        crate::handlers::chat_sessions::create,
        crate::handlers::chat_sessions::get,
        crate::handlers::chat_sessions::rename,
        crate::handlers::chat_sessions::delete,
        crate::handlers::chat_sessions::fork,
        crate::handlers::chat_sessions::messages,
        crate::handlers::chat_sessions::send,
        crate::handlers::chat_sessions::send_stream,
        crate::handlers::chat_sessions::export,
        crate::handlers::agent_chats::list,
        crate::handlers::agent_chats::get,
        crate::handlers::agent_chats::create,
        crate::handlers::agent_chats::stop,
        crate::handlers::agents::list,
        crate::handlers::agents::connect,
        crate::handlers::agents::detail,
        crate::handlers::agents::disconnect,
        crate::handlers::agents::grant_permission,
        crate::handlers::agents::update_settings,
        crate::handlers::agents::revoke_permission,
        crate::handlers::tasks::list,
        crate::handlers::tasks::run,
        crate::handlers::tasks::get,
        crate::handlers::tasks::cancel,
        crate::handlers::tasks::trace,
        crate::handlers::tools::list,
        crate::handlers::tools::install,
        crate::handlers::tools::get,
        crate::handlers::tools::remove,
        crate::handlers::pipelines::list,
        crate::handlers::pipelines::save,
        crate::handlers::pipelines::delete,
        crate::handlers::pipelines::run,
        crate::handlers::secrets::list,
        crate::handlers::secrets::set,
        crate::handlers::secrets::revoke,
        crate::handlers::audit::logs,
        crate::handlers::audit::detail,
        crate::handlers::audit::verify,
        crate::handlers::costs::summary,
        crate::handlers::costs::agent_costs,
        crate::handlers::notifications::list,
        crate::handlers::notifications::unread_count,
        crate::handlers::notifications::clear_read,
        crate::handlers::notifications::get,
        crate::handlers::notifications::dismiss,
        crate::handlers::notifications::respond,
        crate::handlers::webhooks::telegram_webhook,
        crate::handlers::auth::login,
        crate::handlers::auth::me,
        crate::handlers::auth::refresh,
        crate::handlers::keys::list,
        crate::handlers::keys::create,
        crate::handlers::keys::revoke,
        crate::handlers::escalations::list,
        crate::handlers::escalations::get,
        crate::handlers::escalations::resolve,
        crate::handlers::prefs::list_proposals,
        crate::handlers::prefs::accept,
        crate::handlers::prefs::reject,
        crate::handlers::prefs::stats,
        crate::handlers::roles::list,
        crate::handlers::roles::create,
        crate::handlers::roles::get,
        crate::handlers::roles::delete,
        crate::handlers::dashboard::get,
        crate::handlers::config::get_tree,
        crate::handlers::config::get_key,
        crate::handlers::config::set_key,
        crate::handlers::doctor::checks,
        crate::handlers::doctor::fix,
        crate::handlers::logs::query,
        crate::handlers::system_info::resources,
        crate::handlers::system_info::hal,
        crate::handlers::tasks::resume,
        crate::handlers::tasks::checkpoints,
        crate::handlers::pipelines::import,
        crate::handlers::pipelines::export,
        crate::handlers::pipelines::get,
        crate::handlers::pipelines::run_events,
        crate::handlers::schedules::list,
        crate::handlers::schedules::create,
        crate::handlers::schedules::preview,
        crate::handlers::schedules::pause,
        crate::handlers::schedules::resume,
        crate::handlers::schedules::delete,
        crate::handlers::schedules::runs,
        crate::handlers::workflows::list,
        crate::handlers::workflows::get,
        crate::handlers::workflows::create,
        crate::handlers::workflows::update,
        crate::handlers::workflows::delete,
        crate::handlers::plugins::list,
        crate::handlers::plugins::discover,
        crate::handlers::plugins::detail,
        crate::handlers::plugins::enable,
        crate::handlers::plugins::disable,
        crate::handlers::channels::list,
        crate::handlers::channels::detail,
        crate::handlers::channels::disconnect,
        crate::handlers::mcp::list,
        crate::handlers::mcp::detach,
        crate::handlers::connectors::list,
        crate::handlers::connectors::detail,
        crate::handlers::connectors::disconnect,
        crate::handlers::events::list_subscriptions,
        crate::handlers::events::create_subscription,
        crate::handlers::events::delete_subscription,
        crate::handlers::events::emit,
        crate::handlers::sse::events_stream,
        crate::handlers::marketplace::search,
        crate::handlers::marketplace::detail,
        crate::handlers::marketplace::review,
        crate::handlers::webhooks_admin::list,
        crate::handlers::webhooks_admin::create,
        crate::handlers::webhooks_admin::rotate,
        crate::handlers::webhooks_admin::delete,
        crate::handlers::identity::get,
        crate::handlers::files::upload,
        crate::handlers::files::list,
        crate::handlers::files::get,
        crate::handlers::files::download,
        crate::handlers::files::delete,
        crate::handlers::scratchpad::list_global,
        crate::handlers::scratchpad::get_global,
        crate::handlers::scratchpad::put_global,
        crate::handlers::scratchpad::delete_global,
        crate::handlers::scratchpad::list_agent,
        crate::handlers::scratchpad::get_agent,
        crate::handlers::scratchpad::put_agent,
        crate::handlers::scratchpad::delete_agent,
    ),
    components(schemas(
        crate::error::ApiErrorBody,
        crate::types::LoginRequest,
        crate::types::IssuedKeyResponse,
        crate::types::AuthMe,
        crate::types::ApiKeyMeta,
        crate::types::CreateKeyRequest,
        crate::types::EscalationListQuery,
        crate::types::ApiEscalation,
        crate::types::ResolveEscalationRequest,
        crate::types::ResolveEscalationResponse,
        crate::types::PrefProposalQuery,
        crate::types::ApiPrefProposal,
        crate::types::ApiProposalStats,
        crate::types::ApiRole,
        crate::types::CreateRoleRequest,
        crate::types::ConfigTree,
        crate::types::ConfigValue,
        crate::types::SetConfigRequest,
        crate::types::DoctorReport,
        crate::types::DoctorCheck,
        crate::types::DoctorFixRequest,
        crate::types::LogLine,
        crate::types::ResourceInfo,
        crate::types::ResourceLockInfo,
        crate::types::HalInfo,
        crate::types::HalDevice,
        crate::types::CostBudget,
        crate::types::ApiCheckpointSummary,
        crate::types::ImportPipelineRequest,
        crate::types::PipelineExport,
        crate::types::ApiScheduleSummary,
        crate::types::CreateScheduleRequest,
        crate::types::CronPreviewRequest,
        crate::types::CronPreviewResponse,
        crate::types::ApiScheduleRun,
        crate::types::ApiWorkflowSummary,
        crate::types::SaveWorkflowRequest,
        crate::types::WorkflowSaveResponse,
        crate::types::ApiPluginSummary,
        crate::types::ApiPluginDetail,
        crate::types::DiscoverPluginsResponse,
        crate::types::ApiChannelSummary,
        crate::types::ApiMcpServer,
        crate::types::ApiMcpStats,
        crate::types::ApiConnectorSummary,
        crate::types::ApiConnectorDetail,
        crate::types::ApiEventSubscription,
        crate::types::CreateSubscriptionRequest,
        crate::types::EmitEventRequest,
        crate::types::ApiWebhookEndpoint,
        crate::types::CreateWebhookRequest,
        crate::types::WebhookSecretResponse,
        crate::types::ApiAgentIdentity,
        crate::types::ApiAgentSummary,
        crate::types::ApiAgentDetail,
        crate::types::ConnectAgentRequest,
        crate::types::UpdateAgentSettingsRequest,
        crate::types::PermissionRequest,
        crate::types::ApiTaskSummary,
        crate::types::ApiTaskDetail,
        crate::types::RunTaskRequest,
        crate::types::TaskFilter,
        crate::types::ApiToolSummary,
        crate::types::InstallToolRequest,
        crate::types::SetSecretRequest,
        crate::types::AuditEntrySummary,
        crate::types::AuditEntryDetail,
        crate::types::AuditFilter,
        crate::types::CostSummaryEntry,
        crate::types::NotificationSummary,
        crate::types::NotificationResponseRequest,
        crate::types::NotificationFilter,
        crate::types::SystemStatus,
        crate::types::DashboardSummary,
        crate::types::TaskCounts,
        crate::types::ApiPipelineSummary,
        crate::types::SavePipelineRequest,
        crate::types::RunPipelineRequest,
        crate::handlers::chat::OpenAIChatRequest,
        crate::handlers::chat::OpenAIMessage,
        crate::handlers::chat::OpenAIContent,
        crate::handlers::chat::OpenAIContentPart,
        crate::handlers::chat::OpenAIImageUrl,
        crate::handlers::chat::OpenAIChatResponse,
        crate::handlers::chat::OpenAIChoice,
        crate::handlers::chat::OpenAIUsage,
        crate::types::ApiChatSessionSummary,
        crate::types::CreateChatSessionRequest,
        crate::types::ApiChatMessage,
        crate::types::ApiChatSessionDetail,
        crate::types::RenameChatSessionRequest,
        crate::types::ForkChatSessionRequest,
        crate::types::ForkChatSessionResponse,
        crate::types::SendChatMessageRequest,
        crate::types::ApiConvoSummary,
        crate::types::ApiConvoTurn,
        crate::types::ApiConvoDetail,
        crate::types::CreateConvoRequest,
        crate::types::SubmitReviewRequest,
        crate::types::ApiFileMeta,
        crate::types::ApiScratchPage,
        crate::types::ApiPageSummary,
        crate::types::SavePageRequest,
        crate::types::ScratchListResponse,
    )),
    modifiers(&SecurityAddon),
    tags(
        (name = "system", description = "Health and kernel status"),
        (name = "agents", description = "Agent registry, permissions, settings"),
        (name = "tasks", description = "Task execution, trace, lifecycle"),
        (name = "tools", description = "Installed tool management"),
        (name = "pipelines", description = "Pipeline definitions and runs"),
        (name = "secrets", description = "Vault-backed secret management"),
        (name = "audit", description = "Append-only audit log"),
        (name = "costs", description = "Token and cost accounting"),
        (name = "notifications", description = "User notification inbox"),
        (name = "chat", description = "OpenAI-compatible chat completions"),
        (name = "chat-sessions", description = "Persisted chat-session management (list, create, rename, fork, export, messages)"),
        (name = "agent-chats", description = "Multi-agent conversation history (read-only)"),
        (name = "webhooks", description = "Inbound channel webhooks"),
        (name = "auth", description = "Operator login, key refresh, identity"),
        (name = "keys", description = "API key management"),
        (name = "escalations", description = "Human-in-the-loop escalation review and resolution"),
        (name = "prefs", description = "User-preference proposal governance"),
        (name = "roles", description = "Role definitions and permission grouping"),
        (name = "config", description = "Runtime configuration read/write"),
        (name = "schedules", description = "Cron schedule automation"),
        (name = "workflows", description = "Visual workflow definitions"),
        (name = "plugins", description = "Plugin discovery and lifecycle"),
        (name = "channels", description = "Connected bidirectional channels"),
        (name = "mcp", description = "MCP server attachments"),
        (name = "connectors", description = "OAuth connector management"),
        (name = "events", description = "Event subscriptions and emission"),
        (name = "marketplace", description = "Tool registry proxy (search, detail, reviews)"),
        (name = "files", description = "Uploaded file storage"),
        (name = "scratchpad", description = "Agent + global scratchpad pages"),
    )
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::ApiDoc;
    use utoipa::OpenApi;

    /// The document must serialize and declare the OpenAPI 3.1 version.
    #[test]
    fn openapi_document_builds() {
        let json = ApiDoc::openapi().to_pretty_json().expect("spec serializes");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["openapi"].as_str().unwrap().starts_with("3."));
        assert_eq!(v["info"]["title"], "AgentOS API");
    }

    /// The Bearer security scheme must be present so generated clients send auth.
    #[test]
    fn declares_bearer_security_scheme() {
        let json = ApiDoc::openapi().to_pretty_json().unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v["components"]["securitySchemes"]["bearer_auth"].is_object(),
            "bearer_auth security scheme must be defined"
        );
    }

    /// Every registered route must be documented. Bump this when routes change —
    /// the committed `openapi.json` (regenerated by the `gen_openapi` bin) is the
    /// source of truth, and CI diffs it against this generated output.
    #[test]
    fn covers_all_documented_operations() {
        let json = ApiDoc::openapi().to_pretty_json().unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let methods = ["get", "post", "put", "delete", "patch"];
        let ops: usize = v["paths"]
            .as_object()
            .unwrap()
            .values()
            .map(|item| methods.iter().filter(|m| item.get(*m).is_some()).count())
            .sum();
        assert_eq!(ops, 135, "expected 135 documented operations");
    }
}
