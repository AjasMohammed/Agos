//! Implementation of [`KernelService`] for the real [`Kernel`].
//!
//! Each method delegates to the appropriate kernel subsystem (agent_registry,
//! scheduler, tool_registry, etc.) and converts internal types into the
//! `Api`-prefixed DTOs defined in `crate::types`.

use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::*;
use crate::util::task_state_str;
use agentos_kernel::{ChatStreamEvent, Kernel};
use agentos_types::{
    DeliveryChannel, LLMProvider, NotificationID, SecretMetadata, SecretScope, TaskID, TaskState,
    ToolID, UserResponse,
};
use async_trait::async_trait;
use tokio::sync::mpsc;

// ── Stable string serialization helpers ─────────────────────────────────────

fn provider_str(p: &LLMProvider) -> &str {
    match p {
        LLMProvider::Ollama => "ollama",
        LLMProvider::OpenAI => "openai",
        LLMProvider::Anthropic => "anthropic",
        LLMProvider::Gemini => "gemini",
        LLMProvider::Custom(s) => s.as_str(),
    }
}

fn status_str(s: &agentos_types::AgentStatus) -> &str {
    match s {
        agentos_types::AgentStatus::Online => "online",
        agentos_types::AgentStatus::Idle => "idle",
        agentos_types::AgentStatus::Busy => "busy",
        agentos_types::AgentStatus::Offline => "offline",
    }
}

fn trust_tier_str(t: &agentos_types::TrustTier) -> &str {
    match t {
        agentos_types::TrustTier::Core => "core",
        agentos_types::TrustTier::Verified => "verified",
        agentos_types::TrustTier::Community => "community",
        agentos_types::TrustTier::Blocked => "blocked",
    }
}

fn tool_status_str(s: &agentos_types::ToolStatus) -> &str {
    match s {
        agentos_types::ToolStatus::Available => "available",
        agentos_types::ToolStatus::Running => "running",
        agentos_types::ToolStatus::Disabled => "disabled",
    }
}

// ── Helper conversions ──────────────────────────────────────────────────────

fn parse_provider(s: &str) -> Result<LLMProvider, ApiError> {
    match s.to_lowercase().as_str() {
        "ollama" => Ok(LLMProvider::Ollama),
        "openai" => Ok(LLMProvider::OpenAI),
        "anthropic" => Ok(LLMProvider::Anthropic),
        "gemini" => Ok(LLMProvider::Gemini),
        other => Ok(LLMProvider::Custom(other.to_string())),
    }
}

fn parse_scope(s: &str) -> SecretScope {
    match s.to_lowercase().as_str() {
        "kernel" => SecretScope::Kernel,
        "global" | "" => SecretScope::Global,
        _ => SecretScope::Global,
    }
}

fn agent_summary(profile: &agentos_types::AgentProfile, supports_images: bool) -> ApiAgentSummary {
    ApiAgentSummary {
        id: profile.id,
        name: profile.name.clone(),
        provider: provider_str(&profile.provider).to_string(),
        model: profile.model.clone(),
        status: status_str(&profile.status).to_string(),
        roles: profile.roles.clone(),
        connected_at: profile.created_at,
        supports_images,
    }
}

fn tool_summary(tool: &agentos_types::RegisteredTool) -> ApiToolSummary {
    ApiToolSummary {
        id: tool.id,
        name: tool.manifest.manifest.name.clone(),
        version: tool.manifest.manifest.version.clone(),
        description: tool.manifest.manifest.description.clone(),
        author: tool.manifest.manifest.author.clone(),
        trust_tier: trust_tier_str(&tool.manifest.manifest.trust_tier).to_string(),
        status: tool_status_str(&tool.status).to_string(),
    }
}

// ── Implementation ──────────────────────────────────────────────────────────

#[async_trait]
impl KernelService for Kernel {
    // ── Agents ──────────────────────────────────────────────────────────

    async fn list_agents(&self) -> Result<Vec<ApiAgentSummary>, ApiError> {
        let registry = self.agent_registry.read().await;
        let llms = self.active_llms.read().await;
        Ok(registry
            .list_online()
            .into_iter()
            .map(|p| {
                let supports_images = llms
                    .get(&p.id)
                    .map(|c| c.supports_images())
                    .unwrap_or(false);
                agent_summary(p, supports_images)
            })
            .collect())
    }

    async fn connect_agent(&self, req: ConnectAgentRequest) -> Result<ApiAgentSummary, ApiError> {
        let provider = parse_provider(&req.provider)?;
        self.api_connect_agent(
            req.name.clone(),
            provider,
            req.model.clone(),
            req.base_url.clone(),
            req.roles.clone(),
            req.description.clone(),
            req.thinking_level.clone(),
            req.system_prompt.clone(),
        )
        .await
        .map_err(ApiError::Internal)?;

        // Read back the newly connected agent to return its summary.
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(&req.name)
            .ok_or_else(|| ApiError::Internal("Agent registered but not found".into()))?;
        let supports_images = self
            .active_llms
            .read()
            .await
            .get(&profile.id)
            .map(|c| c.supports_images())
            .unwrap_or(false);
        Ok(agent_summary(profile, supports_images))
    }

    async fn disconnect_agent(&self, agent_id: agentos_types::AgentID) -> Result<(), ApiError> {
        self.api_disconnect_agent(agent_id)
            .await
            .map_err(ApiError::Internal)
    }

    async fn get_agent_detail(&self, name: &str) -> Result<ApiAgentDetail, ApiError> {
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(name)
            .ok_or_else(|| ApiError::NotFound(format!("Agent '{}' not found", name)))?;

        let summary = {
            let llms = self.active_llms.read().await;
            let supports_images = llms
                .get(&profile.id)
                .map(|c| c.supports_images())
                .unwrap_or(false);
            agent_summary(profile, supports_images)
        };
        let effective = registry.compute_effective_permissions(&profile.id);
        let permissions: Vec<String> = effective
            .entries()
            .iter()
            .map(|e| e.resource.clone())
            .collect();

        let cost_snapshot = self.cost_tracker.get_snapshot(&profile.id).await;

        // Fetch recent tasks assigned to this agent.
        let all_tasks = self.scheduler.list_tasks().await;
        let recent_tasks: Vec<ApiTaskSummary> = all_tasks
            .iter()
            .filter(|t| {
                // Match by agent name via the agent_registry lookup.
                let ag = registry.get_by_id(&t.agent_id);
                ag.is_some_and(|a| a.name == name)
            })
            .take(10)
            .map(|t| {
                let agent_name = registry.get_by_id(&t.agent_id).map(|a| a.name.clone());
                ApiTaskSummary {
                    id: t.id,
                    agent_name,
                    prompt_preview: t.prompt_preview.clone(),
                    status: task_state_str(&t.state).to_string(),
                    created_at: t.created_at,
                    completed_at: None,
                }
            })
            .collect();

        Ok(ApiAgentDetail {
            summary,
            permissions,
            recent_tasks,
            cost_snapshot,
        })
    }

    async fn update_agent_settings(&self, req: UpdateAgentSettingsRequest) -> Result<(), ApiError> {
        self.api_update_agent_settings(
            req.agent_name,
            req.description,
            req.thinking_level,
            req.system_prompt,
        )
        .await
        .map_err(ApiError::Internal)
    }

    async fn grant_permission(&self, req: PermissionRequest) -> Result<(), ApiError> {
        self.api_grant_permission(req.agent_name, req.permission)
            .await
            .map_err(ApiError::Internal)
    }

    async fn revoke_permission(&self, req: PermissionRequest) -> Result<(), ApiError> {
        self.api_revoke_permission(req.agent_name, req.permission)
            .await
            .map_err(ApiError::Internal)
    }

    // ── Tasks ───────────────────────────────────────────────────────────

    async fn list_tasks(&self, filter: TaskFilter) -> Result<(Vec<ApiTaskSummary>, u64), ApiError> {
        let all_tasks = self.scheduler.list_tasks().await;
        let registry = self.agent_registry.read().await;

        let mut filtered: Vec<_> = all_tasks
            .into_iter()
            .filter(|t| {
                if let Some(ref status) = filter.status {
                    let task_status = task_state_str(&t.state);
                    if task_status != status.to_lowercase() {
                        return false;
                    }
                }
                if let Some(ref agent_name) = filter.agent_name {
                    let matches = registry
                        .get_by_id(&t.agent_id)
                        .is_some_and(|a| a.name == *agent_name);
                    if !matches {
                        return false;
                    }
                }
                true
            })
            .collect();

        let total = filtered.len() as u64;
        let offset = filter.offset.unwrap_or(0) as usize;
        let limit = filter.limit.unwrap_or(50) as usize;

        filtered.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        let page: Vec<ApiTaskSummary> = filtered
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|t| {
                let agent_name = registry.get_by_id(&t.agent_id).map(|a| a.name.clone());
                ApiTaskSummary {
                    id: t.id,
                    agent_name,
                    prompt_preview: t.prompt_preview.clone(),
                    status: task_state_str(&t.state).to_string(),
                    created_at: t.created_at,
                    completed_at: None,
                }
            })
            .collect();

        Ok((page, total))
    }

    async fn get_task(&self, id: TaskID) -> Result<ApiTaskDetail, ApiError> {
        let task = self
            .scheduler
            .get_task(&id)
            .await
            .ok_or_else(|| ApiError::NotFound(format!("Task {} not found", id)))?;

        let registry = self.agent_registry.read().await;
        let agent_name = registry.get_by_id(&task.agent_id).map(|a| a.name.clone());

        Ok(ApiTaskDetail {
            id: task.id,
            agent_name,
            prompt: task.original_prompt.clone(),
            status: task_state_str(&task.state).to_string(),
            created_at: task.created_at,
            completed_at: None,
        })
    }

    async fn run_task(&self, _req: RunTaskRequest) -> Result<TaskID, ApiError> {
        Err(ApiError::NotImplemented(
            "Task execution via API not yet wired".into(),
        ))
    }

    async fn cancel_task(&self, id: TaskID) -> Result<(), ApiError> {
        self.scheduler
            .update_state(&id, TaskState::Cancelled)
            .await
            .map_err(ApiError::from)?;
        Ok(())
    }

    async fn get_task_trace(
        &self,
        id: TaskID,
    ) -> Result<agentos_types::task_trace::TaskTrace, ApiError> {
        let trace = self
            .trace_collector
            .get_trace(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Trace for task {} not found", id)))?;
        Ok(trace)
    }

    // ── Tools ───────────────────────────────────────────────────────────

    async fn list_tools(&self) -> Result<Vec<ApiToolSummary>, ApiError> {
        let registry = self.tool_registry.read().await;
        Ok(registry.list_all().into_iter().map(tool_summary).collect())
    }

    async fn install_tool(&self, req: InstallToolRequest) -> Result<ToolID, ApiError> {
        self.api_install_tool(req.manifest_path.clone())
            .await
            .map_err(ApiError::Internal)?;

        // Placeholder ID: `api_install_tool` does not yet return the tool ID
        // directly. Return a new UUID; the caller can look up the tool by name.
        Ok(ToolID::new())
    }

    async fn remove_tool(&self, name: &str) -> Result<(), ApiError> {
        self.api_remove_tool(name.to_string())
            .await
            .map_err(ApiError::Internal)
    }

    // ── Secrets ─────────────────────────────────────────────────────────

    async fn list_secrets(&self) -> Result<Vec<SecretMetadata>, ApiError> {
        self.vault
            .list()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn set_secret(&self, req: SetSecretRequest) -> Result<(), ApiError> {
        let scope = parse_scope(&req.scope);
        self.api_set_secret(req.name, req.value, scope)
            .await
            .map_err(ApiError::Internal)
    }

    async fn revoke_secret(&self, name: &str) -> Result<(), ApiError> {
        self.api_revoke_secret(name.to_string())
            .await
            .map_err(ApiError::Internal)
    }

    // ── Chat ────────────────────────────────────────────────────────────

    async fn agent_supports_images(&self, agent_name: &str) -> Result<bool, ApiError> {
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(agent_name)
            .ok_or_else(|| ApiError::NotFound(format!("Agent '{}' not found", agent_name)))?;
        let llms = self.active_llms.read().await;
        Ok(llms
            .get(&profile.id)
            .map(|c| c.supports_images())
            .unwrap_or(false))
    }

    async fn chat_send(&self, req: ChatRequest) -> Result<ChatResponse, ApiError> {
        let history: Vec<(String, String)> = req.history;
        let user_parts = (!req.parts.is_empty()).then_some(req.parts.clone());
        let result = self
            .chat_infer_with_tools(&req.agent_name, &history, &req.message, user_parts)
            .await
            .map_err(ApiError::Internal)?;

        let tool_calls: Vec<serde_json::Value> = result
            .tool_calls
            .into_iter()
            .map(|tc| {
                serde_json::json!({
                    "tool_name": tc.tool_name,
                    "intent_type": tc.intent_type,
                    "payload": tc.payload,
                    "result": tc.result,
                })
            })
            .collect();

        Ok(ChatResponse {
            message: result.answer,
            tool_calls,
        })
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<ChatStreamEvent>,
    ) -> Result<(), ApiError> {
        // Run the same chat_infer_with_tools path but emit events along the way.
        // For now we perform full inference and emit Thinking → Done events.
        // This unblocks SSE clients while a full token-level streaming implementation
        // is wired in a future iteration.
        let _ = tx.send(ChatStreamEvent::Thinking { iteration: 1 }).await;

        let history: Vec<(String, String)> = req.history;
        let user_parts = (!req.parts.is_empty()).then_some(req.parts.clone());
        let result = self
            .chat_infer_with_tools(&req.agent_name, &history, &req.message, user_parts)
            .await
            .map_err(ApiError::Internal)?;

        let tool_calls: Vec<agentos_kernel::kernel::ChatToolCallRecord> = result.tool_calls;

        // Emit tool events
        for tc in &tool_calls {
            let _ = tx
                .send(ChatStreamEvent::ToolResult {
                    tool_name: tc.tool_name.clone(),
                    result_preview: {
                        let s = tc.result.to_string();
                        s.chars().take(200).collect()
                    },
                    duration_ms: 0,
                    success: true,
                })
                .await;
        }

        let _ = tx
            .send(ChatStreamEvent::Done {
                answer: result.answer,
                tool_calls,
                iterations: result.iterations,
                tokens_used: result.tokens_used,
                cost_usd: result.cost_usd,
            })
            .await;

        Ok(())
    }

    // ── Pipelines ───────────────────────────────────────────────────────

    async fn list_pipelines(&self) -> Result<Vec<ApiPipelineSummary>, ApiError> {
        let store = self.pipeline_engine.store_arc();
        let summaries = tokio::task::spawn_blocking(move || store.list_pipelines())
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        Ok(summaries
            .into_iter()
            .map(|s| ApiPipelineSummary {
                name: s.name,
                description: s.description,
                step_count: s.step_count,
            })
            .collect())
    }

    async fn save_pipeline(&self, req: SavePipelineRequest) -> Result<(), ApiError> {
        let yaml = serde_json::to_string_pretty(&req.definition)
            .map_err(|e| ApiError::BadRequest(format!("Invalid pipeline definition: {e}")))?;
        let store = self.pipeline_engine.store_arc();
        let name = req.name.clone();
        tokio::task::spawn_blocking(move || store.install_pipeline(&name, "1.0.0", &yaml))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn run_pipeline(&self, req: RunPipelineRequest) -> Result<serde_json::Value, ApiError> {
        // Use fully qualified syntax to call the inherent Kernel::run_pipeline,
        // not the KernelService trait method (which would recurse).
        Kernel::run_pipeline(self, req.name, req.input, req.detach, req.agent_name)
            .await
            .map_err(ApiError::Internal)
    }

    async fn delete_pipeline(&self, name: &str) -> Result<(), ApiError> {
        let store = self.pipeline_engine.store_arc();
        let name_owned = name.to_string();
        tokio::task::spawn_blocking(move || store.remove_pipeline(&name_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    // ── Audit ───────────────────────────────────────────────────────────

    async fn query_audit(&self, filter: AuditFilter) -> Result<Vec<AuditEntrySummary>, ApiError> {
        let audit = self.audit.clone();
        let limit = filter.limit.unwrap_or(50);

        let entries = tokio::task::spawn_blocking(move || audit.query_recent(limit))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        Ok(entries
            .into_iter()
            .map(|e| AuditEntrySummary {
                timestamp: e.timestamp,
                event_type: serde_json::to_string(&e.event_type).unwrap_or_default(),
                agent_id: e.agent_id.map(|id| id.to_string()),
                details: e.details.to_string(),
            })
            .collect())
    }

    async fn get_audit_detail(&self, trace_id: &str) -> Result<AuditEntryDetail, ApiError> {
        let tid = trace_id
            .parse::<agentos_types::TraceID>()
            .map_err(|_| ApiError::BadRequest(format!("Invalid trace ID: {trace_id}")))?;

        let audit = self.audit.clone();
        let entries = tokio::task::spawn_blocking(move || audit.query_by_trace(&tid))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let entry = entries.into_iter().next().ok_or_else(|| {
            ApiError::NotFound(format!("Audit entry for trace {} not found", trace_id))
        })?;

        Ok(AuditEntryDetail {
            timestamp: entry.timestamp,
            event_type: serde_json::to_string(&entry.event_type).unwrap_or_default(),
            agent_id: entry.agent_id.map(|id| id.to_string()),
            task_id: entry.task_id.map(|id| id.to_string()),
            trace_id: Some(entry.trace_id.to_string()),
            details: entry.details.to_string(),
            metadata: entry.details,
        })
    }

    // ── Costs ───────────────────────────────────────────────────────────

    async fn get_cost_summary(&self) -> Result<Vec<CostSummaryEntry>, ApiError> {
        let snapshots = self.cost_tracker.get_all_snapshots().await;
        Ok(snapshots
            .into_iter()
            .map(|s| CostSummaryEntry {
                agent_id: s.agent_id,
                agent_name: s.agent_name,
                period_start: s.period_start,
                tokens_used: s.tokens_used,
                cost_usd: s.cost_usd,
                tool_calls: s.tool_calls,
            })
            .collect())
    }

    async fn get_agent_costs(&self, agent_name: &str) -> Result<CostSummaryEntry, ApiError> {
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(agent_name)
            .ok_or_else(|| ApiError::NotFound(format!("Agent '{}' not found", agent_name)))?;
        let agent_id = profile.id;
        drop(registry);

        let snapshot = self
            .cost_tracker
            .get_snapshot(&agent_id)
            .await
            .ok_or_else(|| {
                ApiError::NotFound(format!("No cost data for agent '{}'", agent_name))
            })?;

        Ok(CostSummaryEntry {
            agent_id: snapshot.agent_id,
            agent_name: snapshot.agent_name,
            period_start: snapshot.period_start,
            tokens_used: snapshot.tokens_used,
            cost_usd: snapshot.cost_usd,
            tool_calls: snapshot.tool_calls,
        })
    }

    // ── Notifications ───────────────────────────────────────────────────

    async fn list_notifications(
        &self,
        filter: NotificationFilter,
    ) -> Result<Vec<NotificationSummary>, ApiError> {
        let inbox = self.notification_router.inbox();
        let unread_only = filter.unread_only.unwrap_or(false);
        let limit = filter.limit.unwrap_or(50) as usize;

        let messages = inbox
            .list(unread_only, limit)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        Ok(messages
            .into_iter()
            .map(|m| NotificationSummary {
                id: m.id,
                subject: m.subject.clone(),
                priority: m.priority.to_string(),
                read: m.read,
                timestamp: m.created_at.to_rfc3339(),
                from: match &m.from {
                    agentos_types::NotificationSource::Agent(id) => format!("Agent {}", id),
                    agentos_types::NotificationSource::Kernel => "Kernel".to_string(),
                    agentos_types::NotificationSource::System => "System".to_string(),
                },
                body: m.body.clone(),
            })
            .collect())
    }

    async fn get_notification(
        &self,
        id: NotificationID,
    ) -> Result<agentos_types::UserMessage, ApiError> {
        let inbox = self.notification_router.inbox();
        inbox
            .get(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Notification {} not found", id)))
    }

    async fn respond_to_notification(
        &self,
        id: NotificationID,
        text: String,
    ) -> Result<(), ApiError> {
        let response = UserResponse {
            text,
            responded_at: chrono::Utc::now(),
            channel: DeliveryChannel::web(),
        };
        self.notification_router
            .route_response(id, response)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn get_unread_count(&self) -> Result<u64, ApiError> {
        let inbox = self.notification_router.inbox();
        Ok(inbox.count_unread().await as u64)
    }

    // ── Dashboard ───────────────────────────────────────────────────────

    async fn get_dashboard_summary(&self) -> Result<DashboardSummary, ApiError> {
        let online_agents = self.list_agents().await?;
        let agent_count = {
            let registry = self.agent_registry.read().await;
            registry.list_all().len()
        };

        let all_tasks = self.scheduler.list_tasks().await;
        let running = all_tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .count();
        let completed = all_tasks
            .iter()
            .filter(|t| t.state == TaskState::Complete)
            .count();
        let failed = all_tasks
            .iter()
            .filter(|t| t.state == TaskState::Failed)
            .count();
        let total = all_tasks.len();

        let tool_count = {
            let registry = self.tool_registry.read().await;
            registry.list_all().len()
        };

        let uptime = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .to_std()
            .unwrap_or_default();

        let audit_filter = AuditFilter {
            limit: Some(10),
            ..Default::default()
        };
        let recent_audit = self.query_audit(audit_filter).await.unwrap_or_default();

        let background_tasks = self.background_pool.list_running().await;

        Ok(DashboardSummary {
            agent_count,
            online_agents,
            task_counts: TaskCounts {
                running,
                completed,
                failed,
                total,
            },
            tool_count,
            uptime_secs: uptime.as_secs(),
            recent_audit,
            background_task_count: background_tasks.len(),
        })
    }

    // ── System ──────────────────────────────────────────────────────────

    async fn get_status(&self) -> Result<SystemStatus, ApiError> {
        let agent_count = {
            let registry = self.agent_registry.read().await;
            registry.list_online().len()
        };
        let task_count = self.scheduler.list_tasks().await.len();
        let tool_count = {
            let registry = self.tool_registry.read().await;
            registry.list_all().len()
        };
        let uptime = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .to_std()
            .unwrap_or_default();

        Ok(SystemStatus {
            uptime_secs: uptime.as_secs(),
            agent_count,
            task_count,
            tool_count,
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    async fn get_uptime(&self) -> std::time::Duration {
        chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .to_std()
            .unwrap_or_default()
    }

    async fn verify_webhook_secret(
        &self,
        channel_id: &str,
        secret: &str,
    ) -> Result<bool, ApiError> {
        let cid: agentos_types::ChannelInstanceID = channel_id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid channel ID: {channel_id}")))?;
        let secrets = self.webhook_secrets.read().await;
        Ok(secrets.get(&cid).map(|s| s.as_str()) == Some(secret))
    }

    async fn channel_pinned_external_id(
        &self,
        channel_id: &str,
    ) -> Result<Option<String>, ApiError> {
        let cid: agentos_types::ChannelInstanceID = channel_id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid channel ID: {channel_id}")))?;
        let ch = self
            .channel_registry
            .get_by_id(&cid)
            .await
            .map_err(|e| ApiError::Internal(format!("Channel registry error: {e}")))?;
        Ok(ch.map(|c| c.external_id))
    }

    async fn forward_webhook_message(
        &self,
        message: agentos_kernel::notification_router::InboundMessage,
    ) -> Result<(), ApiError> {
        self.inbound_tx
            .send(message)
            .await
            .map_err(|_| ApiError::Internal("Inbound message channel closed".into()))
    }
}
