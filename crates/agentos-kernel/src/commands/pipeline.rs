use crate::agent_registry::AgentRegistry;
use crate::context::ContextManager;
use crate::kernel::Kernel;
use crate::tool_registry::ToolRegistry;
use agentos_audit::AuditLog;
use agentos_bus::KernelResponse;
use agentos_capability::CapabilityEngine;
use agentos_hal::HardwareAbstractionLayer;
use agentos_llm::LLMCore;
use agentos_tools::runner::ToolRunner;
use agentos_tools::traits::ToolExecutionContext;
use agentos_types::*;
use agentos_vault::SecretsVault;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

impl Kernel {
    pub(crate) async fn cmd_install_pipeline(&self, yaml: String) -> KernelResponse {
        let definition = match agentos_pipeline::PipelineDefinition::from_yaml(&yaml) {
            Ok(d) => d,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Invalid pipeline YAML: {}", e),
                }
            }
        };

        match self.pipeline_engine.store().install_pipeline(
            &definition.name,
            &definition.version,
            &yaml,
        ) {
            Ok(()) => {
                tracing::info!(
                    pipeline = %definition.name,
                    version = %definition.version,
                    "Pipeline installed"
                );

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::IntentCompleted,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "action": "pipeline_installed",
                        "pipeline_name": definition.name,
                        "pipeline_version": definition.version,
                        "steps": definition.steps.len(),
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "name": definition.name,
                        "version": definition.version,
                        "steps": definition.steps.len(),
                    })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    /// Resolve the pipeline's governing agent from the provided `agent_name`.
    /// Returns the agent's `AgentID` or a `KernelResponse::Error`.
    async fn resolve_pipeline_agent(
        &self,
        agent_name: &Option<String>,
    ) -> Result<AgentID, KernelResponse> {
        let name = agent_name.as_deref().ok_or_else(|| KernelResponse::Error {
            message: "Pipeline execution requires --agent <name> to specify the governing agent"
                .to_string(),
        })?;

        let registry = self.agent_registry.read().await;
        let agent = registry
            .get_by_name(name)
            .ok_or_else(|| KernelResponse::Error {
                message: format!("Agent '{}' not found for pipeline execution", name),
            })?;
        Ok(agent.id)
    }

    pub(crate) async fn cmd_run_pipeline(
        &self,
        name: String,
        input: String,
        detach: bool,
        agent_name: Option<String>,
    ) -> KernelResponse {
        // Resolve the governing agent — required for permission enforcement.
        let agent_id = match self.resolve_pipeline_agent(&agent_name).await {
            Ok(id) => id,
            Err(resp) => return resp,
        };

        let yaml = match self.pipeline_engine.store().get_pipeline_yaml(&name) {
            Ok(y) => y,
            Err(e) => {
                return KernelResponse::Error {
                    message: e.to_string(),
                }
            }
        };

        let definition = match agentos_pipeline::PipelineDefinition::from_yaml(&yaml) {
            Ok(d) => d,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to parse stored pipeline: {}", e),
                }
            }
        };

        let run_id = agentos_types::RunID::new();

        // Audit: pipeline run started
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::IntentReceived,
            agent_id: Some(agent_id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "action": "pipeline_run_started",
                "pipeline_name": name,
                "run_id": run_id.to_string(),
                "detach": detach,
                "agent_name": agent_name,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        if detach {
            let executor = OwnedPipelineExecutor {
                agent_registry: self.agent_registry.clone(),
                active_llms: self.active_llms.clone(),
                tool_runner: self.tool_runner.clone(),
                tool_registry: self.tool_registry.clone(),
                vault: self.vault.clone(),
                hal: self.hal.clone(),
                data_dir: self.data_dir.clone(),
                workspace_paths: self.workspace_paths.clone(),
                context_manager: self.context_manager.clone(),
                cost_tracker: self.cost_tracker.clone(),
                agent_id,
                capability_engine: self.capability_engine.clone(),
                injection_scanner: self.injection_scanner.clone(),
                event_sender: self.event_sender.clone(),
                audit: self.audit.clone(),
                cancellation_token: self.cancellation_token.child_token(),
            };

            let engine = self.pipeline_engine.clone();
            let bg_pool = self.background_pool.clone();
            let task_id = TaskID::new();
            let pipeline_name = name.clone();
            let input_clone = input.clone();

            let bg_task = BackgroundTask {
                id: task_id,
                name: format!("pipeline:{}", pipeline_name),
                agent_name: agent_name.unwrap_or_else(|| "pipeline-engine".to_string()),
                task_prompt: format!(
                    "Run pipeline '{}' with input: {}",
                    pipeline_name, input_clone
                ),
                state: TaskState::Running,
                started_at: Some(chrono::Utc::now()),
                completed_at: None,
                result: None,
                detached: true,
                scheduled_job_id: None,
            };
            bg_pool.register(bg_task).await;

            tokio::spawn(async move {
                match engine
                    .run(&definition, &input_clone, run_id, &executor)
                    .await
                {
                    Ok(run) => {
                        let run_json = serde_json::to_value(&run).unwrap_or_default();
                        bg_pool.complete(&task_id, run_json).await;
                    }
                    Err(e) => {
                        bg_pool.fail(&task_id, e.to_string()).await;
                    }
                }
            });

            KernelResponse::Success {
                data: Some(serde_json::json!({
                    "id": run_id.to_string(),
                    "status": "running",
                    "detached": true,
                    "background_task_id": task_id.to_string(),
                })),
            }
        } else {
            let executor = KernelPipelineExecutor {
                kernel: self,
                agent_id,
            };

            match self
                .pipeline_engine
                .run(&definition, &input, run_id, &executor)
                .await
            {
                Ok(run) => {
                    let run_json = serde_json::to_value(&run).unwrap_or_default();
                    KernelResponse::Success {
                        data: Some(run_json),
                    }
                }
                Err(e) => KernelResponse::Error {
                    message: e.to_string(),
                },
            }
        }
    }

    pub(crate) async fn cmd_pipeline_status(&self, run_id: String) -> KernelResponse {
        let run_id = match uuid::Uuid::parse_str(&run_id) {
            Ok(u) => agentos_types::RunID::from_uuid(u),
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Invalid run ID: {}", e),
                }
            }
        };

        match self.pipeline_engine.store().get_run(&run_id) {
            Ok(run) => {
                let run_json = serde_json::to_value(&run).unwrap_or_default();
                KernelResponse::PipelineRunStatus(run_json)
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_pipeline_list(&self) -> KernelResponse {
        match self.pipeline_engine.store().list_pipelines() {
            Ok(list) => {
                let json_list: Vec<serde_json::Value> = list
                    .into_iter()
                    .map(|s| serde_json::to_value(s).unwrap_or_default())
                    .collect();
                KernelResponse::PipelineList(json_list)
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_pipeline_logs(
        &self,
        run_id: String,
        step_id: String,
    ) -> KernelResponse {
        let run_id = match uuid::Uuid::parse_str(&run_id) {
            Ok(u) => agentos_types::RunID::from_uuid(u),
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Invalid run ID: {}", e),
                }
            }
        };

        match self
            .pipeline_engine
            .store()
            .get_step_logs(&run_id, &step_id)
        {
            Ok(logs) => {
                let json_logs: Vec<serde_json::Value> = logs
                    .into_iter()
                    .map(|l| serde_json::to_value(l).unwrap_or_default())
                    .collect();
                KernelResponse::PipelineStepLogs(json_logs)
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_remove_pipeline(&self, name: String) -> KernelResponse {
        match self.pipeline_engine.store().remove_pipeline(&name) {
            Ok(()) => {
                tracing::info!(pipeline = %name, "Pipeline removed");

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::IntentCompleted,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "action": "pipeline_removed",
                        "pipeline_name": name,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                KernelResponse::Success {
                    data: Some(serde_json::json!({ "removed": name })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }
}

/// Bridges the pipeline engine to kernel subsystems for executing agent tasks and tools.
/// Uses borrowed kernel reference — suitable for synchronous (non-detach) pipeline runs.
pub(crate) struct KernelPipelineExecutor<'a> {
    pub(crate) kernel: &'a Kernel,
    pub(crate) agent_id: AgentID,
}

#[async_trait::async_trait]
impl<'a> agentos_pipeline::PipelineExecutor for KernelPipelineExecutor<'a> {
    async fn run_agent_task(&self, agent_name: &str, prompt: &str) -> Result<String, AgentOSError> {
        // Delegate to cmd_run_task which already has full security:
        // capability token issuance, injection scanning, intent validation, audit logging.
        let response = self
            .kernel
            .cmd_run_task(Some(agent_name.to_string()), prompt.to_string())
            .await;
        match response {
            KernelResponse::Success { data: Some(data) } => Ok(data
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()),
            KernelResponse::Error { message } => Err(AgentOSError::KernelError { reason: message }),
            _ => Err(AgentOSError::KernelError {
                reason: "Unexpected response from task execution".to_string(),
            }),
        }
    }

    async fn run_tool(
        &self,
        tool_name: &str,
        input: serde_json::Value,
    ) -> Result<String, AgentOSError> {
        // Resolve the agent's actual permissions from the capability engine.
        // Fail hard if the agent has no registered permissions — never fall back to empty.
        let permissions = self
            .kernel
            .capability_engine
            .get_permissions(&self.agent_id)
            .map_err(|e| AgentOSError::PermissionDenied {
                resource: "pipeline_tool_execution".into(),
                operation: format!(
                    "Agent {} has no registered permissions: {}",
                    self.agent_id, e
                ),
            })?;

        let trace_id = TraceID::new();
        let task_id = TaskID::new();

        let context = ToolExecutionContext {
            data_dir: self.kernel.data_dir.clone(),
            task_id,
            agent_id: self.agent_id,
            trace_id,
            permissions,
            vault: Some(std::sync::Arc::new(agentos_vault::ProxyVault::new(
                self.kernel.vault.clone(),
            ))),
            hal: Some(self.kernel.hal.clone()),
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            workspace_paths: self.kernel.workspace_paths.clone(),
            cancellation_token: self.kernel.cancellation_token.child_token(),
        };

        // Audit: tool execution started
        self.kernel.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
            agent_id: Some(self.agent_id),
            task_id: Some(task_id),
            tool_id: None,
            details: serde_json::json!({
                "tool_name": tool_name,
                "source": "pipeline",
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        let result = self
            .kernel
            .tool_runner
            .execute(tool_name, input, context)
            .await;

        // Audit: tool execution completed/failed
        match &result {
            Ok(_) => {
                self.kernel.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
                    agent_id: Some(self.agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "tool_name": tool_name,
                        "source": "pipeline",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
            }
            Err(e) => {
                self.kernel.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ToolExecutionFailed,
                    agent_id: Some(self.agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "tool_name": tool_name,
                        "source": "pipeline",
                        "error": e.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
            }
        }

        let result = result?;
        Ok(serde_json::to_string(&result).unwrap_or_default())
    }

    async fn check_budget(&self) -> Result<(), AgentOSError> {
        use crate::cost_tracker::BudgetCheckResult;
        match self.kernel.cost_tracker.check_budget(&self.agent_id).await {
            BudgetCheckResult::Ok
            | BudgetCheckResult::Warning { .. }
            | BudgetCheckResult::ModelDowngradeRecommended { .. } => Ok(()),
            BudgetCheckResult::PauseRequired { resource, .. } => Err(AgentOSError::KernelError {
                reason: format!("Pipeline budget pause required: {}", resource),
            }),
            BudgetCheckResult::HardLimitExceeded { resource, .. } => {
                Err(AgentOSError::KernelError {
                    reason: format!("Pipeline budget exceeded: {}", resource),
                })
            }
            BudgetCheckResult::ModelNotAllowed { model, .. } => Err(AgentOSError::KernelError {
                reason: format!("Pipeline model not allowed: {}", model),
            }),
            BudgetCheckResult::WallTimeExceeded {
                elapsed_secs,
                limit_secs,
            } => Err(AgentOSError::KernelError {
                reason: format!(
                    "Pipeline wall-time exceeded: {}s elapsed, {}s limit",
                    elapsed_secs, limit_secs
                ),
            }),
        }
    }
}

/// Owned pipeline executor that can be moved into a spawned task for detach mode.
/// Holds Arc references to kernel subsystems instead of borrowing from Kernel.
pub(crate) struct OwnedPipelineExecutor {
    pub(crate) agent_registry: Arc<RwLock<AgentRegistry>>,
    pub(crate) active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn LLMCore>>>>,
    pub(crate) tool_runner: Arc<ToolRunner>,
    pub(crate) tool_registry: Arc<RwLock<ToolRegistry>>,
    pub(crate) vault: Arc<SecretsVault>,
    pub(crate) hal: Arc<HardwareAbstractionLayer>,
    pub(crate) data_dir: PathBuf,
    pub(crate) workspace_paths: Vec<PathBuf>,
    pub(crate) context_manager: Arc<ContextManager>,
    pub(crate) cost_tracker: Arc<crate::cost_tracker::CostTracker>,
    pub(crate) agent_id: AgentID,
    // Security subsystems — required for permission enforcement and audit trail.
    pub(crate) capability_engine: Arc<CapabilityEngine>,
    pub(crate) injection_scanner: Arc<crate::injection_scanner::InjectionScanner>,
    pub(crate) event_sender: tokio::sync::mpsc::Sender<agentos_types::EventMessage>,
    pub(crate) audit: Arc<AuditLog>,
    pub(crate) cancellation_token: CancellationToken,
}

#[async_trait::async_trait]
impl agentos_pipeline::PipelineExecutor for OwnedPipelineExecutor {
    async fn run_agent_task(&self, agent_name: &str, prompt: &str) -> Result<String, AgentOSError> {
        let registry = self.agent_registry.read().await;
        let agent = registry
            .get_by_name(agent_name)
            .ok_or_else(|| AgentOSError::AgentNotFound(agent_name.to_string()))?
            .clone();
        drop(registry);

        let llm = {
            let active = self.active_llms.read().await;
            active.get(&agent.id).cloned()
        }
        .ok_or_else(|| AgentOSError::KernelError {
            reason: format!("LLM adapter for agent {} not connected", agent.name),
        })?;

        let tools_desc = self.tool_registry.read().await.tools_for_prompt();
        let system_prompt = format!(
            "You are an AI agent operating inside AgentOS.\n\
             Available tools:\n{}\n\
             To use a tool, respond with a JSON block:\n\
             ```json\n{{\"tool\": \"tool-name\", \"intent_type\": \"read|write|execute|query|observe|delegate|message|broadcast|escalate|subscribe|unsubscribe\", \"payload\": {{...}}}}\n```\n\
             When done, provide your final answer as plain text without any tool call blocks.",
            tools_desc
        );

        let task_id = TaskID::new();
        let trace_id = TraceID::new();

        // Emit TaskStarted event
        crate::event_dispatch::emit_signed_event(
            &self.capability_engine,
            &self.audit,
            &self.event_sender,
            EventType::TaskStarted,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "task_id": task_id.to_string(),
                "agent_name": agent_name,
                "source": "pipeline",
            }),
            0,
            trace_id,
            Some(agent.id),
            Some(task_id),
        );

        self.context_manager
            .create_context(task_id, &system_prompt)
            .await;
        self.context_manager
            .push_entry(
                &task_id,
                ContextEntry {
                    role: ContextRole::User,
                    content: prompt.to_string(),
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                    importance: 0.9,
                    pinned: false,
                    reference_count: 0,
                    partition: ContextPartition::default(),
                    category: ContextCategory::Task,
                    is_summary: false,
                },
            )
            .await
            .ok();

        let context = match self.context_manager.get_context(&task_id).await {
            Ok(ctx) => ctx,
            Err(e) => {
                self.context_manager.remove_context(&task_id).await;
                crate::event_dispatch::emit_signed_event(
                    &self.capability_engine,
                    &self.audit,
                    &self.event_sender,
                    EventType::TaskFailed,
                    EventSource::TaskScheduler,
                    EventSeverity::Warning,
                    serde_json::json!({
                        "task_id": task_id.to_string(),
                        "agent_name": agent_name,
                        "source": "pipeline",
                        "error": e.to_string(),
                    }),
                    0,
                    trace_id,
                    Some(agent.id),
                    Some(task_id),
                );
                return Err(AgentOSError::KernelError {
                    reason: format!("Context error: {}", e),
                });
            }
        };

        let inference = match llm.infer(&context).await {
            Ok(r) => r,
            Err(e) => {
                self.context_manager.remove_context(&task_id).await;
                crate::event_dispatch::emit_signed_event(
                    &self.capability_engine,
                    &self.audit,
                    &self.event_sender,
                    EventType::TaskFailed,
                    EventSource::TaskScheduler,
                    EventSeverity::Warning,
                    serde_json::json!({
                        "task_id": task_id.to_string(),
                        "agent_name": agent_name,
                        "source": "pipeline",
                        "error": e.to_string(),
                    }),
                    0,
                    trace_id,
                    Some(agent.id),
                    Some(task_id),
                );
                return Err(e);
            }
        };

        // Scan inference output for injection attempts — block on high-threat matches.
        let scan_result = self.injection_scanner.scan(&inference.text);
        if scan_result.is_suspicious
            && matches!(
                scan_result.max_threat,
                Some(crate::injection_scanner::ThreatLevel::High)
            )
        {
            let match_count = scan_result.matches.len();

            if let Err(e) = self.audit.append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id,
                event_type: agentos_audit::AuditEventType::RiskEscalation,
                agent_id: Some(agent.id),
                task_id: Some(task_id),
                tool_id: None,
                details: serde_json::json!({
                    "source": "pipeline",
                    "threat_level": "high",
                    "matches": match_count,
                }),
                severity: agentos_audit::AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            }) {
                tracing::error!(error = %e, "Failed to write injection scan audit entry");
            }

            self.context_manager.remove_context(&task_id).await;

            crate::event_dispatch::emit_signed_event(
                &self.capability_engine,
                &self.audit,
                &self.event_sender,
                EventType::TaskFailed,
                EventSource::TaskScheduler,
                EventSeverity::Warning,
                serde_json::json!({
                    "task_id": task_id.to_string(),
                    "agent_name": agent_name,
                    "source": "pipeline",
                    "error": format!("injection scanner detected {} high-threat pattern(s)", match_count),
                }),
                0,
                trace_id,
                Some(agent.id),
                Some(task_id),
            );

            return Err(AgentOSError::KernelError {
                reason: format!(
                    "Pipeline agent task blocked: injection scanner detected {} high-threat pattern(s) in LLM output",
                    match_count
                ),
            });
        }

        self.context_manager.remove_context(&task_id).await;

        // Emit TaskCompleted event
        crate::event_dispatch::emit_signed_event(
            &self.capability_engine,
            &self.audit,
            &self.event_sender,
            EventType::TaskCompleted,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "task_id": task_id.to_string(),
                "agent_name": agent_name,
                "source": "pipeline",
            }),
            0,
            trace_id,
            Some(agent.id),
            Some(task_id),
        );

        Ok(inference.text)
    }

    async fn run_tool(
        &self,
        tool_name: &str,
        input: serde_json::Value,
    ) -> Result<String, AgentOSError> {
        // Resolve the agent's actual permissions from the capability engine.
        // Fail hard if the agent has no registered permissions — never fall back to empty.
        let permissions = self
            .capability_engine
            .get_permissions(&self.agent_id)
            .map_err(|e| AgentOSError::PermissionDenied {
                resource: "pipeline_tool_execution".into(),
                operation: format!(
                    "Agent {} has no registered permissions: {}",
                    self.agent_id, e
                ),
            })?;

        let trace_id = TraceID::new();
        let task_id = TaskID::new();

        let context = ToolExecutionContext {
            data_dir: self.data_dir.clone(),
            task_id,
            agent_id: self.agent_id,
            trace_id,
            permissions,
            vault: Some(std::sync::Arc::new(agentos_vault::ProxyVault::new(
                self.vault.clone(),
            ))),
            hal: Some(self.hal.clone()),
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            workspace_paths: self.workspace_paths.clone(),
            cancellation_token: self.cancellation_token.child_token(),
        };

        // Audit: tool execution started
        if let Err(e) = self.audit.append(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
            agent_id: Some(self.agent_id),
            task_id: Some(task_id),
            tool_id: None,
            details: serde_json::json!({
                "tool_name": tool_name,
                "source": "pipeline",
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        }) {
            tracing::error!(error = %e, "Failed to write tool audit entry");
        }

        let result = self.tool_runner.execute(tool_name, input, context).await;

        // Audit: tool execution completed/failed
        match &result {
            Ok(_) => {
                if let Err(e) = self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
                    agent_id: Some(self.agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "tool_name": tool_name,
                        "source": "pipeline",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                }) {
                    tracing::error!(error = %e, "Failed to write tool completion audit entry");
                }
            }
            Err(err) => {
                if let Err(e) = self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ToolExecutionFailed,
                    agent_id: Some(self.agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "tool_name": tool_name,
                        "source": "pipeline",
                        "error": err.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                }) {
                    tracing::error!(error = %e, "Failed to write tool failure audit entry");
                }
            }
        }

        let result = result?;
        Ok(serde_json::to_string(&result).unwrap_or_default())
    }

    async fn check_budget(&self) -> Result<(), AgentOSError> {
        use crate::cost_tracker::BudgetCheckResult;
        match self.cost_tracker.check_budget(&self.agent_id).await {
            BudgetCheckResult::Ok
            | BudgetCheckResult::Warning { .. }
            | BudgetCheckResult::ModelDowngradeRecommended { .. } => Ok(()),
            BudgetCheckResult::PauseRequired { resource, .. } => Err(AgentOSError::KernelError {
                reason: format!("Pipeline budget pause required: {}", resource),
            }),
            BudgetCheckResult::HardLimitExceeded { resource, .. } => {
                Err(AgentOSError::KernelError {
                    reason: format!("Pipeline budget exceeded: {}", resource),
                })
            }
            BudgetCheckResult::ModelNotAllowed { model, .. } => Err(AgentOSError::KernelError {
                reason: format!("Pipeline model not allowed: {}", model),
            }),
            BudgetCheckResult::WallTimeExceeded {
                elapsed_secs,
                limit_secs,
            } => Err(AgentOSError::KernelError {
                reason: format!(
                    "Pipeline wall-time exceeded: {}s elapsed, {}s limit",
                    elapsed_secs, limit_secs
                ),
            }),
        }
    }
}
