use crate::agent_registry::AgentRegistry;
use crate::background_pool::BackgroundPool;
use crate::config::{load_config, KernelConfig};
use crate::context::ContextManager;
use crate::schedule_manager::ScheduleManager;
use crate::scheduler::TaskScheduler;
use crate::tool_call::parse_tool_call;
use crate::tool_registry::ToolRegistry;
use agentos_audit::AuditLog;
use agentos_bus::*;
use agentos_capability::profiles::ProfileManager;
use agentos_capability::CapabilityEngine;
use agentos_hal::{
    drivers::{
        log_reader::LogReaderDriver, network::NetworkDriver, process::ProcessDriver,
        system::SystemDriver,
    },
    HardwareAbstractionLayer,
};
use agentos_llm::{AnthropicCore, CustomCore, GeminiCore, LLMCore, OllamaCore, OpenAICore};
use agentos_pipeline::{PipelineEngine, PipelineStore};
use agentos_sandbox::{SandboxConfig, SandboxExecutor};
use agentos_tools::runner::ToolRunner;
use agentos_tools::traits::ToolExecutionContext;
use agentos_types::*;
use agentos_vault::SecretsVault;
use agentos_wasm::WasmToolExecutor;
use secrecy::SecretString;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct Kernel {
    pub config: KernelConfig,
    pub audit: Arc<AuditLog>,
    pub vault: Arc<SecretsVault>,
    pub capability_engine: Arc<CapabilityEngine>,
    pub scheduler: Arc<TaskScheduler>,
    pub context_manager: Arc<ContextManager>,
    pub tool_registry: Arc<RwLock<ToolRegistry>>,
    pub agent_registry: Arc<RwLock<AgentRegistry>>,
    pub bus: Arc<BusServer>,
    pub tool_runner: Arc<ToolRunner>,
    pub sandbox: Arc<SandboxExecutor>,
    pub router: Arc<crate::router::TaskRouter>,
    pub active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn LLMCore>>>>,
    pub message_bus: Arc<crate::agent_message_bus::AgentMessageBus>,
    pub profile_manager: Arc<ProfileManager>,
    pub episodic_memory: Arc<agentos_memory::EpisodicStore>,
    pub schedule_manager: Arc<ScheduleManager>,
    pub background_pool: Arc<BackgroundPool>,
    pub hal: Arc<HardwareAbstractionLayer>,
    pub pipeline_engine: Arc<PipelineEngine>,
    data_dir: PathBuf,
    started_at: chrono::DateTime<chrono::Utc>,
}

impl Kernel {
    /// Boot the kernel: load config, open subsystems, start bus, begin accepting.
    pub async fn boot(config_path: &Path, vault_passphrase: &str) -> Result<Self, anyhow::Error> {
        // 1. Load config
        let config = load_config(config_path)?;

        // Ensure directories exist
        if let Some(parent) = Path::new(&config.audit.log_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = Path::new(&config.secrets.vault_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(Path::new(&config.tools.core_tools_dir))?;
        std::fs::create_dir_all(Path::new(&config.tools.user_tools_dir))?;
        if let Some(parent) = Path::new(&config.bus.socket_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Install bundled core tool manifests if not already present
        Self::install_core_manifests(Path::new(&config.tools.core_tools_dir))?;

        // 2. Open audit log
        let audit = Arc::new(AuditLog::open(Path::new(&config.audit.log_path))?);

        // 3. Open or initialize secrets vault
        let vault_path = Path::new(&config.secrets.vault_path);
        let vault = if SecretsVault::is_initialized(vault_path) {
            Arc::new(SecretsVault::open(
                vault_path,
                vault_passphrase,
                audit.clone(),
            )?)
        } else {
            Arc::new(SecretsVault::initialize(
                vault_path,
                vault_passphrase,
                audit.clone(),
            )?)
        };

        // 4. Initialize capability engine
        let capability_engine = Arc::new(CapabilityEngine::new());

        // 4.5 Initialize HardwareAbstractionLayer
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(SystemDriver::new()));
        hal.register(Box::new(ProcessDriver::new()));
        hal.register(Box::new(NetworkDriver::new()));

        // Register log reader with app logs only - audit log is not exposed to agents
        let app_logs = HashMap::new();
        let mut system_logs = HashMap::new();
        system_logs.insert(
            "syslog".to_string(),
            Path::new("/var/log/syslog").to_path_buf(),
        );
        hal.register(Box::new(LogReaderDriver::new(app_logs, system_logs)));

        let hal = Arc::new(hal);

        // 5. Load tools
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::load_from_dirs(
            Path::new(&config.tools.core_tools_dir),
            Path::new(&config.tools.user_tools_dir),
        )?));

        // 6. Initialize other subsystems
        let data_dir = PathBuf::from(&config.tools.data_dir);
        std::fs::create_dir_all(&data_dir)?;
        let model_cache_dir = {
            let configured = PathBuf::from(&config.memory.model_cache_dir);
            if configured.is_absolute() {
                configured
            } else {
                data_dir.join(configured)
            }
        };
        std::fs::create_dir_all(&model_cache_dir)?;
        let mut tool_runner = ToolRunner::new_with_model_cache_dir(&data_dir, &model_cache_dir);

        // Register WASM tools from manifests that specify executor = wasm
        let wasm_executor = WasmToolExecutor::new(&data_dir);
        match wasm_executor {
            Ok(executor) => {
                let registry_read = tool_registry.read().await;
                for loaded in &registry_read.loaded {
                    if loaded.manifest.executor.executor_type == agentos_types::ExecutorType::Wasm {
                        if let Some(ref rel_path) = loaded.manifest.executor.wasm_path {
                            let abs_path = loaded.manifest_dir.join(rel_path);
                            match executor.load(&loaded.manifest, &abs_path) {
                                Ok(wasm_tool) => {
                                    tracing::info!(
                                        tool = %loaded.manifest.manifest.name,
                                        "Registered WASM tool"
                                    );
                                    tool_runner.register(Box::new(wasm_tool));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        tool = %loaded.manifest.manifest.name,
                                        error = %e,
                                        "Failed to load WASM tool"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "WASM executor initialization failed; WASM tools disabled");
            }
        }

        let tool_runner = Arc::new(tool_runner);
        let sandbox = Arc::new(SandboxExecutor::new(data_dir.clone()));
        let scheduler = Arc::new(TaskScheduler::new(config.kernel.max_concurrent_tasks));
        let context_manager = Arc::new(ContextManager::new(
            config.kernel.context_window_max_entries,
        ));
        let agent_registry = Arc::new(RwLock::new(AgentRegistry::with_persistence(
            data_dir.clone(),
        )));
        let router = Arc::new(crate::router::TaskRouter::new(
            config.routing.strategy.clone(),
            config.routing.rules.clone(),
        ));
        let active_llms = Arc::new(RwLock::new(HashMap::new()));
        let message_bus = Arc::new(crate::agent_message_bus::AgentMessageBus::new());
        let profile_manager = Arc::new(ProfileManager::new());
        let episodic_memory = Arc::new(agentos_memory::EpisodicStore::open(&data_dir)?);
        let schedule_manager = Arc::new(ScheduleManager::new());
        let background_pool = Arc::new(BackgroundPool::new());

        // 6.5 Initialize pipeline engine
        let pipeline_store = Arc::new(
            PipelineStore::open(&data_dir.join("pipelines.db"))
                .map_err(|e| anyhow::anyhow!("Pipeline store init failed: {}", e))?,
        );
        let pipeline_engine = Arc::new(PipelineEngine::new(pipeline_store));

        // 7. Start bus server
        let bus = Arc::new(BusServer::bind(Path::new(&config.bus.socket_path)).await?);

        let kernel = Kernel {
            config,
            audit,
            vault,
            capability_engine,
            scheduler,
            context_manager,
            tool_registry,
            agent_registry,
            bus,
            tool_runner,
            sandbox,
            router,
            active_llms,
            message_bus,
            profile_manager,
            episodic_memory,
            schedule_manager,
            background_pool,
            hal,
            pipeline_engine,
            data_dir,
            started_at: chrono::Utc::now(),
        };

        // Emit KernelStarted audit event
        kernel
            .audit
            .append(agentos_audit::AuditEntry {
                timestamp: kernel.started_at,
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::KernelStarted,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "bus_socket": kernel.config.bus.socket_path,
                    "max_concurrent_tasks": kernel.config.kernel.max_concurrent_tasks
                }),
                severity: agentos_audit::AuditSeverity::Info,
            })
            .ok(); // Ignore if it fails during boot

        Ok(kernel)
    }

    /// The main run loop.
    pub async fn run(self: Arc<Self>) -> Result<(), anyhow::Error> {
        let kernel = self.clone();

        // Spawn connection acceptor
        let acceptor = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                loop {
                    match kernel.bus.accept().await {
                        Ok(conn) => {
                            let kernel = kernel.clone();
                            tokio::spawn(async move {
                                kernel.handle_connection(conn).await;
                            });
                        }
                        Err(e) => {
                            tracing::error!("Bus accept error: {}", e);
                        }
                    }
                }
            }
        });

        // Spawn task executor
        let executor = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                kernel.task_executor_loop().await;
            }
        });

        // Spawn timeout checker (every 10 seconds)
        let timeout_checker = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    kernel.scheduler.check_timeouts().await;
                }
            }
        });

        // Spawn agentd scheduler loop
        let agentd = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                kernel.agentd_loop().await;
            }
        });

        // Wait for any task to finish (shouldn't happen unless shutdown)
        tokio::select! {
            _ = acceptor => {},
            _ = executor => {},
            _ = timeout_checker => {},
            _ = agentd => {},
        }

        Ok(())
    }

    /// Handle a single CLI connection.
    async fn handle_connection(self: &Arc<Self>, mut conn: BusConnection) {
        loop {
            match conn.read().await {
                Ok(BusMessage::Command(cmd)) => {
                    let response = self.handle_command(cmd).await;
                    if conn
                        .write(&BusMessage::CommandResponse(response))
                        .await
                        .is_err()
                    {
                        break; // connection closed
                    }
                }
                Err(_) => break, // connection closed
                _ => {}          // ignore unexpected message types
            }
        }
    }

    /// Route a KernelCommand to the appropriate handler.
    async fn handle_command(&self, cmd: KernelCommand) -> KernelResponse {
        match cmd {
            KernelCommand::ConnectAgent {
                name,
                provider,
                model,
                base_url,
            } => {
                self.cmd_connect_agent(name, provider, model, base_url)
                    .await
            }
            KernelCommand::ListAgents => self.cmd_list_agents().await,
            KernelCommand::DisconnectAgent { agent_id } => {
                self.cmd_disconnect_agent(agent_id).await
            }
            KernelCommand::RunTask {
                agent_name, prompt, ..
            } => self.cmd_run_task(agent_name, prompt).await,
            KernelCommand::ListTasks => self.cmd_list_tasks().await,
            KernelCommand::SetSecret { name, value, scope } => {
                self.cmd_set_secret(name, value, scope).await
            }
            KernelCommand::ListSecrets => self.cmd_list_secrets().await,
            KernelCommand::RotateSecret { name, new_value } => {
                self.cmd_rotate_secret(name, new_value).await
            }
            KernelCommand::RevokeSecret { name } => self.cmd_revoke_secret(name).await,
            KernelCommand::GetTaskLogs { task_id } => self.cmd_get_task_logs(task_id).await,
            KernelCommand::CancelTask { task_id } => self.cmd_cancel_task(task_id).await,
            KernelCommand::ListTools => self.cmd_list_tools().await,
            KernelCommand::InstallTool { manifest_path } => {
                self.cmd_install_tool(manifest_path).await
            }
            KernelCommand::RemoveTool { tool_name } => self.cmd_remove_tool(tool_name).await,
            KernelCommand::GrantPermission {
                agent_name,
                permission,
            } => self.cmd_grant_permission(agent_name, permission).await,
            KernelCommand::RevokePermission {
                agent_name,
                permission,
            } => self.cmd_revoke_permission(agent_name, permission).await,
            KernelCommand::ShowPermissions { agent_name } => {
                self.cmd_show_permissions(agent_name).await
            }
            KernelCommand::CreateRole {
                role_name,
                description,
            } => self.cmd_create_role(role_name, description).await,
            KernelCommand::DeleteRole { role_name } => self.cmd_delete_role(role_name).await,
            KernelCommand::ListRoles => self.cmd_list_roles().await,
            KernelCommand::RoleGrant {
                role_name,
                permission,
            } => self.cmd_role_grant(role_name, permission).await,
            KernelCommand::RoleRevoke {
                role_name,
                permission,
            } => self.cmd_role_revoke(role_name, permission).await,
            KernelCommand::AssignRole {
                agent_name,
                role_name,
            } => self.cmd_assign_role(agent_name, role_name).await,
            KernelCommand::RemoveRole {
                agent_name,
                role_name,
            } => self.cmd_remove_role(agent_name, role_name).await,
            KernelCommand::GetStatus => self.cmd_get_status().await,
            KernelCommand::GetAuditLogs { limit } => self.cmd_get_audit_logs(limit).await,
            KernelCommand::SendAgentMessage {
                from_name,
                to_name,
                content,
            } => {
                self.cmd_send_agent_message(from_name, to_name, content)
                    .await
            }
            KernelCommand::ListAgentMessages { agent_name, limit } => {
                self.cmd_list_agent_messages(agent_name, limit).await
            }
            KernelCommand::CreateAgentGroup {
                group_name,
                members,
            } => self.cmd_create_agent_group(group_name, members).await,
            KernelCommand::BroadcastToGroup {
                group_name,
                content,
            } => self.cmd_broadcast_to_group(group_name, content).await,
            KernelCommand::CreatePermProfile {
                name,
                description,
                permissions,
            } => {
                self.cmd_create_perm_profile(name, description, permissions)
                    .await
            }
            KernelCommand::DeletePermProfile { name } => self.cmd_delete_perm_profile(name).await,
            KernelCommand::ListPermProfiles => self.cmd_list_perm_profiles().await,
            KernelCommand::AssignPermProfile {
                agent_name,
                profile_name,
            } => self.cmd_assign_perm_profile(agent_name, profile_name).await,
            KernelCommand::GrantPermissionTimed {
                agent_name,
                permission,
                expires_secs,
            } => {
                self.cmd_grant_permission_timed(agent_name, permission, expires_secs)
                    .await
            }

            // agentd
            KernelCommand::CreateSchedule {
                name,
                cron,
                agent_name,
                task,
                permissions,
            } => {
                self.cmd_create_schedule(name, cron, agent_name, task, permissions)
                    .await
            }
            KernelCommand::ListSchedules => self.cmd_list_schedules().await,
            KernelCommand::PauseSchedule { name } => self.cmd_pause_schedule(name).await,
            KernelCommand::ResumeSchedule { name } => self.cmd_resume_schedule(name).await,
            KernelCommand::DeleteSchedule { name } => self.cmd_delete_schedule(name).await,
            KernelCommand::RunBackground {
                name,
                agent_name,
                task,
                detach,
            } => {
                self.cmd_run_background(name, agent_name, task, detach)
                    .await
            }
            KernelCommand::ListBackground => self.cmd_list_background().await,
            KernelCommand::GetBackgroundLogs { name, follow } => {
                self.cmd_get_background_logs(name, follow).await
            }
            KernelCommand::KillBackground { name } => self.cmd_kill_background(name).await,

            // Pipeline management
            KernelCommand::InstallPipeline { yaml } => self.cmd_install_pipeline(yaml).await,
            KernelCommand::RunPipeline {
                name,
                input,
                detach,
            } => self.cmd_run_pipeline(name, input, detach).await,
            KernelCommand::PipelineStatus { name: _, run_id } => {
                self.cmd_pipeline_status(run_id).await
            }
            KernelCommand::PipelineList => self.cmd_pipeline_list().await,
            KernelCommand::PipelineLogs {
                name: _,
                run_id,
                step_id,
            } => self.cmd_pipeline_logs(run_id, step_id).await,
            KernelCommand::RemovePipeline { name } => self.cmd_remove_pipeline(name).await,

            KernelCommand::Shutdown => {
                std::process::exit(0);
            }
        }
    }

    // --- Command Handlers ---

    async fn cmd_connect_agent(
        &self,
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
    ) -> KernelResponse {
        let now = chrono::Utc::now();
        let agent_id = AgentID::new();

        // Instantiate LLMCore based on provider
        let core: Result<Arc<dyn LLMCore>, String> = match &provider {
            LLMProvider::Ollama => {
                let host = base_url.unwrap_or_else(|| self.config.ollama.host.clone());
                Ok(Arc::new(OllamaCore::new(&host, &model)))
            }
            LLMProvider::OpenAI => {
                match self
                    .vault
                    .get(&format!("{}_openai_api_key", name))
                    .or_else(|_| self.vault.get("openai_api_key"))
                {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        if let Some(url) = base_url {
                            Ok(Arc::new(OpenAICore::with_base_url(sec, model.clone(), url)))
                        } else {
                            Ok(Arc::new(OpenAICore::new(sec, model.clone())))
                        }
                    }
                    _ => {
                        Err("Missing 'openai_api_key' in vault. Please store it first.".to_string())
                    }
                }
            }
            LLMProvider::Anthropic => {
                match self
                    .vault
                    .get(&format!("{}_anthropic_api_key", name))
                    .or_else(|_| self.vault.get("anthropic_api_key"))
                {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        Ok(Arc::new(AnthropicCore::new(sec, model.clone())))
                    }
                    _ => Err(
                        "Missing 'anthropic_api_key' in vault. Please store it first.".to_string(),
                    ),
                }
            }
            LLMProvider::Gemini => {
                match self
                    .vault
                    .get(&format!("{}_gemini_api_key", name))
                    .or_else(|_| self.vault.get("gemini_api_key"))
                {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        Ok(Arc::new(GeminiCore::new(sec, model.clone())))
                    }
                    _ => {
                        Err("Missing 'gemini_api_key' in vault. Please store it first.".to_string())
                    }
                }
            }
            LLMProvider::Custom(_) => {
                let sec = match self
                    .vault
                    .get(&format!("{}_custom_api_key", name))
                    .or_else(|_| self.vault.get("custom_api_key"))
                {
                    Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                    _ => None,
                };
                let url = base_url.unwrap_or_else(|| "http://localhost:8000/v1".to_string());
                Ok(Arc::new(CustomCore::new(sec, model.clone(), url)))
            }
        };

        let llm_adapter = match core {
            Ok(adapter) => adapter,
            Err(e) => {
                return KernelResponse::Error { message: e };
            }
        };

        let profile = AgentProfile {
            id: agent_id,
            name,
            provider,
            model,
            status: AgentStatus::Online,
            permissions: PermissionSet::new(),
            roles: vec!["base".to_string()],
            current_task: None,
            description: String::new(),
            created_at: now,
            last_active: now,
        };

        let agent_name = profile.name.clone();
        let agent_model = profile.model.clone();

        {
            let mut registry = self.agent_registry.write().await;
            registry.register(profile.clone());
        }

        {
            let mut active = self.active_llms.write().await;
            active.insert(agent_id, llm_adapter);
        }

        self.audit
            .append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::AgentConnected,
                agent_id: Some(agent_id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "name": agent_name, "model": agent_model }),
                severity: agentos_audit::AuditSeverity::Info,
            })
            .ok();

        KernelResponse::Success {
            data: Some(serde_json::json!({ "agent_id": agent_id.to_string() })),
        }
    }

    async fn cmd_list_agents(&self) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agents: Vec<AgentProfile> = registry.list_all().into_iter().cloned().collect();
        KernelResponse::AgentList(agents)
    }

    async fn cmd_disconnect_agent(&self, agent_id: AgentID) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        if registry.get_by_id(&agent_id).is_none() {
            return KernelResponse::Error {
                message: format!("Agent '{}' not found", agent_id),
            };
        }
        registry.remove(&agent_id);
        drop(registry);

        self.audit
            .append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::AgentDisconnected,
                agent_id: Some(agent_id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({}),
                severity: agentos_audit::AuditSeverity::Info,
            })
            .ok();

        KernelResponse::Success { data: None }
    }

    async fn cmd_run_task(&self, agent_name: Option<String>, prompt: String) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent_id = match agent_name {
            Some(name) => match registry.get_by_name(&name) {
                Some(a) => a.id,
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' not found", name),
                    }
                }
            },
            None => {
                let agents: Vec<AgentProfile> = registry.list_all().into_iter().cloned().collect();
                match self.router.route(&prompt, &agents).await {
                    Ok(id) => id,
                    Err(e) => {
                        return KernelResponse::Error {
                            message: format!("Failed to route task: {}", e),
                        }
                    }
                }
            }
        };

        let agent = registry.get_by_id(&agent_id).unwrap().clone();
        let effective_permissions = registry.compute_effective_permissions(&agent_id);
        drop(registry);

        let task_id = TaskID::new();
        let capability_token = match self.capability_engine.issue_token(
            task_id,
            agent.id,
            std::collections::BTreeSet::new(), // all tools allowed by default; permissions gate access
            std::collections::BTreeSet::from([
                IntentTypeFlag::Read,
                IntentTypeFlag::Write,
                IntentTypeFlag::Execute,
                IntentTypeFlag::Query,
            ]),
            effective_permissions,
            Duration::from_secs(self.config.kernel.default_task_timeout_secs),
        ) {
            Ok(token) => token,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to issue capability token: {}", e),
                };
            }
        };

        let task = AgentTask {
            id: task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token,
            assigned_llm: Some(agent.id),
            priority: 5,
            created_at: chrono::Utc::now(),
            timeout: Duration::from_secs(self.config.kernel.default_task_timeout_secs),
            original_prompt: prompt,
            history: Vec::new(),
            parent_task: None,
        };

        // Execute task synchronously so the CLI gets the result
        let result = self.execute_task_sync(&task).await;
        match result {
            Ok(answer) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "task_id": task.id.to_string(),
                    "result": answer,
                })),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_list_tasks(&self) -> KernelResponse {
        let tasks = self.scheduler.list_tasks().await;
        KernelResponse::TaskList(tasks)
    }

    async fn cmd_set_secret(
        &self,
        name: String,
        value: String,
        scope: SecretScope,
    ) -> KernelResponse {
        match self.vault.set(&name, &value, SecretOwner::Kernel, scope) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_list_secrets(&self) -> KernelResponse {
        match self.vault.list() {
            Ok(list) => KernelResponse::SecretList(list),
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_rotate_secret(&self, name: String, new_value: String) -> KernelResponse {
        match self.vault.rotate(&name, &new_value) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_revoke_secret(&self, name: String) -> KernelResponse {
        match self.vault.revoke(&name) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_list_tools(&self) -> KernelResponse {
        let registry = self.tool_registry.read().await;
        let tools: Vec<ToolManifest> = registry
            .list_all()
            .into_iter()
            .map(|t| t.manifest.clone())
            .collect();
        KernelResponse::ToolList(tools)
    }

    async fn cmd_get_status(&self) -> KernelResponse {
        let uptime = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .num_seconds() as u64;
        let connected_agents = self.agent_registry.read().await.list_all().len() as u32;
        let active_tasks = self.scheduler.running_count().await as u32;
        let installed_tools = self.tool_registry.read().await.list_all().len() as u32;

        KernelResponse::Status(SystemStatus {
            uptime_secs: uptime,
            connected_agents,
            active_tasks,
            installed_tools,
            total_audit_entries: 0, // simplified for now
        })
    }

    async fn cmd_get_audit_logs(&self, limit: u32) -> KernelResponse {
        match self.audit.query_recent(limit) {
            Ok(logs) => KernelResponse::AuditLogs(logs),
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_get_task_logs(&self, task_id: TaskID) -> KernelResponse {
        match self.scheduler.get_task(&task_id).await {
            Some(task) => {
                let logs: Vec<String> = task
                    .history
                    .iter()
                    .map(|entry| {
                        format!(
                            "[{}] {:?} -> {:?}: {}",
                            entry.timestamp.format("%H:%M:%S"),
                            entry.intent_type,
                            entry.target,
                            entry.payload.schema
                        )
                    })
                    .collect();
                KernelResponse::TaskLogs(logs)
            }
            None => KernelResponse::Error {
                message: format!("Task '{}' not found", task_id),
            },
        }
    }

    async fn cmd_cancel_task(&self, task_id: TaskID) -> KernelResponse {
        match self
            .scheduler
            .update_state(&task_id, TaskState::Cancelled)
            .await
        {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_install_tool(&self, manifest_path: String) -> KernelResponse {
        let path = std::path::Path::new(&manifest_path);
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Cannot read manifest '{}': {}", manifest_path, e),
                }
            }
        };
        match toml::from_str::<ToolManifest>(&content) {
            Ok(manifest) => {
                self.tool_registry.write().await.register(manifest);
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Invalid manifest: {}", e),
            },
        }
    }

    async fn cmd_remove_tool(&self, tool_name: String) -> KernelResponse {
        match self.tool_registry.write().await.remove(&tool_name) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_grant_permission(&self, agent_name: String, permission: String) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let (resource, read, write, execute) = match Self::parse_permission(&permission) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!(
                    "Invalid permission '{}'. Expected format: resource:rwx (e.g. fs.user_data:rw)",
                    permission
                ),
                }
            }
        };

        let mut perms = self
            .capability_engine
            .get_permissions(&agent.id)
            .unwrap_or_default();
        perms.grant(resource, read, write, execute, None);
        self.capability_engine
            .update_permissions(&agent.id, perms)
            .ok();

        self.audit
            .append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::PermissionGranted,
                agent_id: Some(agent.id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "permission": permission, "agent_name": agent_name }),
                severity: agentos_audit::AuditSeverity::Info,
            })
            .ok();

        KernelResponse::Success { data: None }
    }

    async fn cmd_revoke_permission(
        &self,
        agent_name: String,
        permission: String,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let (resource, read, write, execute) = match Self::parse_permission(&permission) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!(
                    "Invalid permission '{}'. Expected format: resource:rwx (e.g. fs.user_data:rw)",
                    permission
                ),
                }
            }
        };

        let mut perms = self
            .capability_engine
            .get_permissions(&agent.id)
            .unwrap_or_default();
        perms.revoke(&resource, read, write, execute);
        self.capability_engine
            .update_permissions(&agent.id, perms)
            .ok();

        self.audit
            .append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::PermissionRevoked,
                agent_id: Some(agent.id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "permission": permission, "agent_name": agent_name }),
                severity: agentos_audit::AuditSeverity::Info,
            })
            .ok();

        KernelResponse::Success { data: None }
    }

    async fn cmd_show_permissions(&self, agent_name: String) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let perms = self
            .capability_engine
            .get_permissions(&agent.id)
            .unwrap_or_default();
        KernelResponse::Permissions(perms)
    }

    // --- Permission Profiles ---

    async fn cmd_create_perm_profile(
        &self,
        name: String,
        description: String,
        permissions_strs: Vec<String>,
    ) -> KernelResponse {
        let mut perms = PermissionSet::new();
        for p in permissions_strs {
            if let Some((res, r, w, x)) = Self::parse_permission(&p) {
                perms.grant(res, r, w, x, None);
            } else {
                return KernelResponse::Error {
                    message: format!("Invalid permission '{}'", p),
                };
            }
        }
        match self.profile_manager.create(&name, &description, perms) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_delete_perm_profile(&self, name: String) -> KernelResponse {
        match self.profile_manager.delete(&name) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_list_perm_profiles(&self) -> KernelResponse {
        let profiles = self.profile_manager.list_all();
        KernelResponse::PermProfileList(profiles)
    }

    async fn cmd_assign_perm_profile(
        &self,
        agent_name: String,
        profile_name: String,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent_id = match registry.get_by_name(&agent_name) {
            Some(a) => a.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let profile = match self.profile_manager.get(&profile_name) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Profile '{}' not found", profile_name),
                }
            }
        };

        let mut current_perms = self
            .capability_engine
            .get_permissions(&agent_id)
            .unwrap_or_default();
        for entry in profile.permissions.entries() {
            current_perms.grant(
                entry.resource.clone(),
                entry.read,
                entry.write,
                entry.execute,
                entry.expires_at,
            );
        }

        self.capability_engine
            .update_permissions(&agent_id, current_perms)
            .ok();
        KernelResponse::Success { data: None }
    }

    async fn cmd_grant_permission_timed(
        &self,
        agent_name: String,
        permission: String,
        expires_secs: u64,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let (resource, read, write, execute) = match Self::parse_permission(&permission) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Invalid permission"),
                }
            }
        };

        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_secs as i64);

        let mut perms = self
            .capability_engine
            .get_permissions(&agent.id)
            .unwrap_or_default();
        perms.grant(resource.clone(), read, write, execute, Some(expires_at));
        self.capability_engine
            .update_permissions(&agent.id, perms)
            .ok();

        self.audit.append(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionGranted,
            agent_id: Some(agent.id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "permission": permission, "expires_at": expires_at.to_rfc3339() }),
            severity: agentos_audit::AuditSeverity::Info,
        }).ok();

        KernelResponse::Success { data: None }
    }

    // --- Role Management ---

    async fn cmd_create_role(&self, role_name: String, description: String) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        if registry.get_role_by_name(&role_name).is_some() {
            return KernelResponse::Error {
                message: format!("Role '{}' already exists", role_name),
            };
        }
        let role = Role::new(role_name.clone(), description);
        registry.register_role(role);

        self.audit
            .append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::PermissionGranted, // Using existing type
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "action": "create_role", "role_name": role_name }),
                severity: agentos_audit::AuditSeverity::Info,
            })
            .ok();

        KernelResponse::Success { data: None }
    }

    async fn cmd_delete_role(&self, role_name: String) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let role_id = match registry.get_role_by_name(&role_name) {
            Some(r) => r.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Role '{}' not found", role_name),
                }
            }
        };

        match registry.unregister_role(&role_id) {
            Ok(_) => {
                self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::PermissionRevoked, // Using existing type
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "action": "delete_role", "role_name": role_name }),
                    severity: agentos_audit::AuditSeverity::Info,
                }).ok();
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error { message: e },
        }
    }

    async fn cmd_list_roles(&self) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let roles: Vec<Role> = registry.list_roles().into_iter().cloned().collect();
        KernelResponse::RoleList(roles)
    }

    async fn cmd_role_grant(&self, role_name: String, permission: String) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let mut perms = match registry.get_role_by_name(&role_name) {
            Some(r) => r.permissions.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Role '{}' not found", role_name),
                }
            }
        };

        let (resource, read, write, execute) = match Self::parse_permission(&permission) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Invalid permission '{}'", permission),
                }
            }
        };

        perms.grant(resource, read, write, execute, None);
        if let Err(e) = registry.update_role_permissions(&role_name, perms) {
            return KernelResponse::Error { message: e };
        }

        self.audit.append(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionGranted,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "action": "role_grant", "role_name": role_name, "permission": permission }),
            severity: agentos_audit::AuditSeverity::Info,
        }).ok();

        KernelResponse::Success { data: None }
    }

    // --- Agent Communication ---

    async fn cmd_send_agent_message(
        &self,
        from_name: String,
        to_name: String,
        content: String,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let from_agent = match registry.get_by_name(&from_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Sender agent '{}' not found", from_name),
                }
            }
        };
        let to_agent = match registry.get_by_name(&to_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Target agent '{}' not found", to_name),
                }
            }
        };
        drop(registry);

        let msg = AgentMessage {
            id: MessageID::new(),
            from: from_agent.id,
            to: agentos_types::MessageTarget::Direct(to_agent.id),
            content: agentos_types::MessageContent::Text(content),
            reply_to: None,
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
        };

        match self.message_bus.send_direct(msg).await {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_list_agent_messages(&self, agent_name: String, limit: u32) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let history = self
            .message_bus
            .get_history(&agent.id, limit as usize)
            .await;
        KernelResponse::AgentMessageList(history)
    }

    async fn cmd_create_agent_group(
        &self,
        group_name: String,
        members: Vec<String>,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let mut member_ids = Vec::new();
        for m in members {
            if let Some(a) = registry.get_by_name(&m) {
                member_ids.push(a.id);
            } else {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", m),
                };
            }
        }
        drop(registry);

        let group_id = GroupID::new();
        self.message_bus.create_group(group_id, member_ids).await;

        KernelResponse::Success {
            data: Some(
                serde_json::json!({ "group_id": group_id.to_string(), "group_name": group_name }),
            ),
        }
    }

    async fn cmd_broadcast_to_group(&self, _group_name: String, content: String) -> KernelResponse {
        let msg = AgentMessage {
            id: MessageID::new(),
            from: AgentID::new(),
            to: agentos_types::MessageTarget::Broadcast,
            content: agentos_types::MessageContent::Text(content),
            reply_to: None,
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
        };

        match self.message_bus.broadcast(msg).await {
            Ok(count) => KernelResponse::Success {
                data: Some(serde_json::json!({ "sent_to": count })),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    // --- Task Delegation ---

    pub async fn handle_task_delegation(
        &self,
        parent_task: &AgentTask,
        target_agent_name: &str,
        prompt: &str,
        priority: u8,
        timeout_secs: u64,
    ) -> Result<serde_json::Value, AgentOSError> {
        let registry = self.agent_registry.read().await;
        let target = registry
            .get_by_name(target_agent_name)
            .ok_or_else(|| AgentOSError::AgentNotFound(target_agent_name.to_string()))?
            .clone();

        let target_permissions = registry.compute_effective_permissions(&target.id);
        drop(registry);

        let child_permissions = parent_task.capability_token.permissions.clone();
        let effective_permissions = child_permissions.intersect(&target_permissions);

        let child_token = self.capability_engine.issue_token(
            TaskID::new(),
            target.id,
            parent_task.capability_token.allowed_tools.clone(),
            parent_task.capability_token.allowed_intents.clone(),
            effective_permissions,
            Duration::from_secs(timeout_secs),
        )?;

        let child_task = AgentTask {
            id: child_token.task_id,
            state: TaskState::Queued,
            agent_id: target.id,
            capability_token: child_token,
            assigned_llm: None,
            priority,
            created_at: chrono::Utc::now(),
            timeout: Duration::from_secs(timeout_secs),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(parent_task.id),
        };

        // Note: Scheduler enqueue logic depends on actual definition. Assuming this succeeds.
        let _ = self.scheduler.enqueue(child_task.clone()).await;

        Ok(serde_json::json!({
            "delegated_to": target_agent_name,
            "child_task_id": child_task.id.to_string(),
            "status": "queued",
        }))
    }

    async fn cmd_role_revoke(&self, role_name: String, permission: String) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let mut perms = match registry.get_role_by_name(&role_name) {
            Some(r) => r.permissions.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Role '{}' not found", role_name),
                }
            }
        };

        let (resource, read, write, execute) = match Self::parse_permission(&permission) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Invalid permission '{}'", permission),
                }
            }
        };

        perms.revoke(&resource, read, write, execute);
        if let Err(e) = registry.update_role_permissions(&role_name, perms) {
            return KernelResponse::Error { message: e };
        }

        self.audit
            .append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::PermissionRevoked,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "role_name": role_name, "permission": permission }),
                severity: agentos_audit::AuditSeverity::Info,
            })
            .ok();

        KernelResponse::Success { data: None }
    }

    async fn cmd_assign_role(&self, agent_name: String, role_name: String) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let agent_id = match registry.get_by_name(&agent_name) {
            Some(a) => a.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };

        match registry.assign_role(&agent_id, role_name.clone()) {
            Ok(_) => {
                self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::PermissionGranted, // Abstract enough
                    agent_id: Some(agent_id),
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "action": "assign_role", "role_name": role_name, "agent_name": agent_name }),
                    severity: agentos_audit::AuditSeverity::Info,
                }).ok();
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error { message: e },
        }
    }

    async fn cmd_remove_role(&self, agent_name: String, role_name: String) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let agent_id = match registry.get_by_name(&agent_name) {
            Some(a) => a.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };

        match registry.remove_role(&agent_id, &role_name) {
            Ok(_) => {
                self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::PermissionRevoked, // Abstract enough
                    agent_id: Some(agent_id),
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "action": "remove_role", "role_name": role_name, "agent_name": agent_name }),
                    severity: agentos_audit::AuditSeverity::Info,
                }).ok();
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error { message: e },
        }
    }
    fn parse_permission(perm: &str) -> Option<(String, bool, bool, bool)> {
        let parts: Vec<&str> = perm.splitn(2, ':').collect();
        if parts.len() != 2 {
            return None;
        }
        let resource = parts[0].to_string();
        let flags = parts[1];
        let read = flags.contains('r');
        let write = flags.contains('w');
        let execute = flags.contains('x');
        if !read && !write && !execute {
            return None;
        }
        Some((resource, read, write, execute))
    }

    // --- Task Execution ---

    async fn task_executor_loop(self: &Arc<Self>) {
        loop {
            if self.scheduler.running_count().await >= self.config.kernel.max_concurrent_tasks {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            if let Some(task) = self.scheduler.dequeue().await {
                let kernel = self.clone();
                tokio::spawn(async move {
                    kernel.execute_task(&task).await;
                });
            } else {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    /// Validate a tool call against the capability token and permission system.
    /// Returns Ok(()) if authorized, or the denial error message if not.
    fn validate_tool_call(
        &self,
        task: &AgentTask,
        tool_call: &crate::tool_call::ParsedToolCall,
        trace_id: TraceID,
    ) -> Result<(), String> {
        // 1. Build an IntentMessage for validate_intent
        let intent = IntentMessage {
            id: MessageID::new(),
            sender_token: task.capability_token.clone(),
            intent_type: tool_call.intent_type,
            target: IntentTarget::Kernel, // Use Kernel target; tool-level gating is done via required_permissions below
            payload: SemanticPayload {
                schema: tool_call.tool_name.clone(),
                data: tool_call.payload.clone(),
            },
            context_ref: ContextID::new(),
            priority: task.priority,
            timeout_ms: task.timeout.as_millis() as u32,
            trace_id,
            timestamp: chrono::Utc::now(),
        };

        // 2. Get the tool's required permissions
        let required_perms = self
            .tool_runner
            .get_required_permissions(&tool_call.tool_name)
            .unwrap_or_default();

        let required_for_validate: Vec<(String, PermissionOp)> = required_perms;

        // 3. Run full capability validation: HMAC signature, expiry, intent type, and permissions
        self.capability_engine
            .validate_intent(&task.capability_token, &intent, &required_for_validate)
            .map_err(|e| format!("{}", e))
    }

    /// Execute a single task synchronously: assemble context, call LLM, process tool calls, repeat.
    /// Returns the final answer text.
    async fn execute_task_sync(&self, task: &AgentTask) -> Result<String, anyhow::Error> {
        // Resolve the agent's LLM provider
        let agent = {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&task.agent_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", task.agent_id))?
        };

        // Look up the active LLM core
        let llm = {
            let active = self.active_llms.read().await;
            active.get(&agent.id).cloned()
        };

        let llm = match llm {
            Some(adapter) => adapter,
            None => {
                return Err(anyhow::anyhow!(
                    "LLM adapter for agent {} not connected",
                    agent.name
                ));
            }
        };

        // 1. Create context with system prompt
        let tools_desc = self.tool_registry.read().await.tools_for_prompt();
        let system_prompt = format!(
            "You are an AI agent operating inside AgentOS.\n\
             Available tools:\n{}\n\
             To use a tool, respond with a JSON block:\n\
             ```json\n{{\"tool\": \"tool-name\", \"intent_type\": \"read|write\", \"payload\": {{...}}}}\n```\n\
             When done, provide your final answer as plain text without any tool call blocks.",
            tools_desc
        );

        let mut agent_directory = String::from("\n\n[AGENT_DIRECTORY]\nYou are operating inside AgentOS. The following agents are available:\n");
        let agents = self
            .agent_registry
            .read()
            .await
            .list_all()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        for opt_agent in agents {
            if opt_agent.id == task.agent_id {
                continue;
            } // Don't list self
            let status = match opt_agent.current_task {
                Some(tid) => format!("Busy ({})", tid),
                None => "Idle".to_string(),
            };

            let perms = self
                .capability_engine
                .get_permissions(&opt_agent.id)
                .unwrap_or_default();
            let mut perm_strs = Vec::new();
            for e in perms.entries {
                let r = if e.read { "r" } else { "" };
                let w = if e.write { "w" } else { "" };
                let x = if e.execute { "x" } else { "" };
                perm_strs.push(format!("{}:{}{}{}", e.resource, r, w, x));
            }
            let perm_str = if perm_strs.is_empty() {
                "None".to_string()
            } else {
                perm_strs.join(", ")
            };

            let provider_str = match opt_agent.provider {
                agentos_types::LLMProvider::Anthropic => "anthropic",
                agentos_types::LLMProvider::OpenAI => "openai",
                agentos_types::LLMProvider::Ollama => "ollama",
                agentos_types::LLMProvider::Gemini => "gemini",
                agentos_types::LLMProvider::Custom(_) => "custom",
            };

            agent_directory.push_str(&format!(
                "\n- {} ({}/{}) — Status: {}\n  Permissions: {}",
                opt_agent.name, provider_str, opt_agent.model, status, perm_str
            ));
        }
        agent_directory.push_str("\n\nTo message an agent: use the agent-message tool\nTo delegate a subtask: use the task-delegate tool\n[/AGENT_DIRECTORY]");

        let system_prompt = format!("{}{}", system_prompt, agent_directory);
        self.context_manager
            .create_context(task.id, &system_prompt)
            .await;

        // 2. Push the user's prompt into context
        self.context_manager
            .push_entry(
                &task.id,
                ContextEntry {
                    role: ContextRole::User,
                    content: task.original_prompt.clone(),
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                },
            )
            .await
            .ok();

        self.episodic_memory
            .record(
                &task.id,
                &task.agent_id,
                agentos_memory::EpisodeType::UserPrompt,
                &task.original_prompt,
                Some("User prompt received"),
                None,
                &TraceID::new(),
            )
            .ok();

        // 3. Agent loop: LLM → parse → tool call → push result → repeat
        let max_iterations = 10;
        let mut final_answer = String::new();

        for iteration in 0..max_iterations {
            let context = match self.context_manager.get_context(&task.id).await {
                Ok(ctx) => ctx,
                Err(_) => break,
            };

            tracing::info!("Task {} iteration {}: calling LLM", task.id, iteration);

            let inference = match llm.infer(&context).await {
                Ok(result) => result,
                Err(e) => {
                    self.context_manager.remove_context(&task.id).await;
                    anyhow::bail!("LLM error: {}", e);
                }
            };

            tracing::info!(
                "Task {} LLM responded ({} tokens, {}ms)",
                task.id,
                inference.tokens_used.total_tokens,
                inference.duration_ms
            );

            // Push assistant response into context
            self.context_manager
                .push_entry(
                    &task.id,
                    ContextEntry {
                        role: ContextRole::Assistant,
                        content: inference.text.clone(),
                        timestamp: chrono::Utc::now(),
                        metadata: None,
                    },
                )
                .await
                .ok();

            self.episodic_memory
                .record(
                    &task.id,
                    &task.agent_id,
                    agentos_memory::EpisodeType::LLMResponse,
                    &inference.text,
                    Some(&format!(
                        "LLM response ({} tokens)",
                        inference.tokens_used.total_tokens
                    )),
                    None,
                    &TraceID::new(),
                )
                .ok();

            // Check for tool call
            match parse_tool_call(&inference.text) {
                Some(tool_call) => {
                    tracing::info!(
                        "Task {} tool call: {} ({:?})",
                        task.id,
                        tool_call.tool_name,
                        tool_call.intent_type
                    );

                    // --- Permission enforcement via capability token ---
                    // Validates HMAC signature, token expiry, intent type, and resource permissions
                    let trace_id = TraceID::new();
                    if let Err(denial_reason) = self.validate_tool_call(task, &tool_call, trace_id)
                    {
                        tracing::warn!(
                            "Task {} permission denied for tool {}: {}",
                            task.id,
                            tool_call.tool_name,
                            denial_reason
                        );
                        self.audit
                            .append(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::PermissionDenied,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({
                                    "tool": tool_call.tool_name,
                                    "intent_type": format!("{:?}", tool_call.intent_type),
                                    "reason": denial_reason,
                                }),
                                severity: agentos_audit::AuditSeverity::Security,
                            })
                            .ok();

                        let error_result = serde_json::json!({
                            "error": format!(
                                "Permission denied: {}",
                                denial_reason
                            )
                        });
                        self.context_manager
                            .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                            .await
                            .ok();
                        continue; // Skip tool execution, let LLM see the error
                    }

                    self.audit
                        .append(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id,
                            event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
                            agent_id: Some(task.agent_id),
                            task_id: Some(task.id),
                            tool_id: None,
                            details: serde_json::json!({ "tool": tool_call.tool_name }),
                            severity: agentos_audit::AuditSeverity::Info,
                        })
                        .ok();

                    self.episodic_memory
                        .record(
                            &task.id,
                            &task.agent_id,
                            agentos_memory::EpisodeType::ToolCall,
                            &format!(
                                "Tool: {} Payload: {}",
                                tool_call.tool_name, tool_call.payload
                            ),
                            Some(&format!("Called tool '{}'", tool_call.tool_name)),
                            None,
                            &trace_id,
                        )
                        .ok();

                    let exec_context = ToolExecutionContext {
                        data_dir: self.data_dir.clone(),
                        task_id: task.id,
                        agent_id: task.agent_id,
                        trace_id,
                        permissions: task.capability_token.permissions.clone(),
                        vault: Some(self.vault.clone()),
                        hal: Some(self.hal.clone()),
                    };

                    // V2: Try sandboxed execution first, fall back to in-process
                    let tool_result = {
                        // Derive sandbox config from tool manifest if available
                        let sandbox_config = {
                            let registry = self.tool_registry.read().await;
                            registry
                                .get_by_name(&tool_call.tool_name)
                                .map(|t| SandboxConfig::from_manifest(&t.manifest.sandbox))
                        };

                        if let Some(config) = sandbox_config {
                            let timeout = Duration::from_millis(config.max_cpu_ms.max(5000));
                            match self
                                .sandbox
                                .spawn(
                                    &tool_call.tool_name,
                                    tool_call.payload.clone(),
                                    &config,
                                    timeout,
                                )
                                .await
                            {
                                Ok(sandbox_result) => {
                                    SandboxExecutor::parse_result(&sandbox_result)
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        tool = %tool_call.tool_name,
                                        error = %e,
                                        "Sandbox spawn failed, falling back to in-process execution"
                                    );
                                    // Fallback to in-process execution
                                    self.tool_runner
                                        .execute(
                                            &tool_call.tool_name,
                                            tool_call.payload,
                                            exec_context,
                                        )
                                        .await
                                }
                            }
                        } else {
                            // No manifest found — run in-process (V1 behavior)
                            self.tool_runner
                                .execute(&tool_call.tool_name, tool_call.payload, exec_context)
                                .await
                        }
                    };

                    match tool_result {
                        Ok(result) => {
                            self.audit
                                .append(agentos_audit::AuditEntry {
                                    timestamp: chrono::Utc::now(),
                                    trace_id,
                                    event_type:
                                        agentos_audit::AuditEventType::ToolExecutionCompleted,
                                    agent_id: Some(task.agent_id),
                                    task_id: Some(task.id),
                                    tool_id: None,
                                    details: serde_json::json!({ "tool": tool_call.tool_name }),
                                    severity: agentos_audit::AuditSeverity::Info,
                                })
                                .ok();

                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &result)
                                .await
                                .ok();

                            self.episodic_memory
                                .record(
                                    &task.id,
                                    &task.agent_id,
                                    agentos_memory::EpisodeType::ToolResult,
                                    &result.to_string(),
                                    Some(&format!("Tool '{}' succeeded", tool_call.tool_name)),
                                    None,
                                    &trace_id,
                                )
                                .ok();
                        }
                        Err(e) => {
                            self.audit.append(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::ToolExecutionFailed,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({ "tool": tool_call.tool_name, "error": e.to_string() }),
                                severity: agentos_audit::AuditSeverity::Error,
                            }).ok();

                            let error_result = serde_json::json!({
                                "error": e.to_string()
                            });
                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                .await
                                .ok();

                            self.episodic_memory
                                .record(
                                    &task.id,
                                    &task.agent_id,
                                    agentos_memory::EpisodeType::ToolResult,
                                    &error_result.to_string(),
                                    Some(&format!("Tool '{}' failed: {}", tool_call.tool_name, e)),
                                    None,
                                    &trace_id,
                                )
                                .ok();
                        }
                    }
                    // Continue loop — LLM will see the tool result next iteration
                }
                None => {
                    // No tool call — this is the final answer
                    final_answer = inference.text;
                    break;
                }
            }
        }

        self.context_manager.remove_context(&task.id).await;
        Ok(final_answer)
    }

    /// Execute a task from the background executor loop.
    async fn execute_task(&self, task: &AgentTask) {
        self.scheduler
            .update_state(&task.id, TaskState::Running)
            .await
            .ok();

        match self.execute_task_sync(task).await {
            Ok(answer) => {
                tracing::info!(
                    "Task {} complete: {}",
                    task.id,
                    &answer[..answer.len().min(100)]
                );
                self.scheduler
                    .update_state(&task.id, TaskState::Complete)
                    .await
                    .ok();
                self.background_pool
                    .complete(&task.id, serde_json::json!({ "result": answer }))
                    .await;
            }
            Err(e) => {
                tracing::error!("Task {} failed: {}", task.id, e);
                self.scheduler
                    .update_state(&task.id, TaskState::Failed)
                    .await
                    .ok();
                self.background_pool.fail(&task.id, e.to_string()).await;
            }
        }
    }

    // --- agentd & Background Tasks ---

    async fn agentd_loop(&self) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;

            let due_jobs = self.schedule_manager.check_due_jobs().await;
            for job in due_jobs {
                tracing::info!(job_name = %job.name, "Firing scheduled job");

                self.audit
                    .append(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::ScheduledJobFired,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "job_name": job.name }),
                        severity: agentos_audit::AuditSeverity::Info,
                    })
                    .ok();

                let _ = self
                    .create_background_task(
                        job.name.clone(),
                        job.agent_name.clone(),
                        job.task_prompt.clone(),
                        true,
                    )
                    .await;
            }
        }
    }

    async fn create_background_task(
        &self,
        name: String,
        agent_name: String,
        prompt: String,
        detached: bool,
    ) -> Result<TaskID, AgentOSError> {
        let registry = self.agent_registry.read().await;
        let agent = registry
            .get_by_name(&agent_name)
            .ok_or_else(|| AgentOSError::AgentNotFound(agent_name.clone()))?
            .clone();

        let target_permissions = registry.compute_effective_permissions(&agent.id);
        drop(registry);

        let task_id = TaskID::new();
        let capability_token = self
            .capability_engine
            .issue_token(
                task_id,
                agent.id,
                std::collections::BTreeSet::new(),
                std::collections::BTreeSet::from([
                    IntentTypeFlag::Read,
                    IntentTypeFlag::Write,
                    IntentTypeFlag::Execute,
                    IntentTypeFlag::Query,
                ]),
                target_permissions,
                Duration::from_secs(self.config.kernel.default_task_timeout_secs),
            )
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        let task = AgentTask {
            id: task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token,
            assigned_llm: Some(agent.id),
            priority: 5,
            created_at: chrono::Utc::now(),
            timeout: Duration::from_secs(self.config.kernel.default_task_timeout_secs),
            original_prompt: prompt.clone(),
            history: Vec::new(),
            parent_task: None,
        };

        self.background_pool
            .register(BackgroundTask {
                id: task_id,
                name,
                agent_name,
                task_prompt: prompt,
                state: TaskState::Queued,
                started_at: None,
                completed_at: None,
                result: None,
                detached,
            })
            .await;

        let _ = self.scheduler.enqueue(task).await;

        Ok(task_id)
    }

    async fn cmd_create_schedule(
        &self,
        name: String,
        cron: String,
        agent_name: String,
        task: String,
        permissions: Vec<String>,
    ) -> KernelResponse {
        match self
            .schedule_manager
            .create_job(name.clone(), cron, agent_name, task, permissions)
            .await
        {
            Ok(id) => {
                self.audit
                    .append(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::ScheduledJobCreated,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "job_name": name, "schedule_id": id }),
                        severity: agentos_audit::AuditSeverity::Info,
                    })
                    .ok();
                KernelResponse::ScheduleId(id)
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_list_schedules(&self) -> KernelResponse {
        KernelResponse::ScheduleList(self.schedule_manager.list_jobs().await)
    }

    async fn cmd_pause_schedule(&self, name: String) -> KernelResponse {
        if let Some(job) = self.schedule_manager.get_by_name(&name).await {
            match self.schedule_manager.pause(&job.id).await {
                Ok(_) => {
                    self.audit
                        .append(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: TraceID::new(),
                            event_type: agentos_audit::AuditEventType::ScheduledJobPaused,
                            agent_id: None,
                            task_id: None,
                            tool_id: None,
                            details: serde_json::json!({ "job_name": name }),
                            severity: agentos_audit::AuditSeverity::Info,
                        })
                        .ok();
                    KernelResponse::Success { data: None }
                }
                Err(e) => KernelResponse::Error {
                    message: e.to_string(),
                },
            }
        } else {
            KernelResponse::Error {
                message: format!("Schedule {} not found", name),
            }
        }
    }

    async fn cmd_resume_schedule(&self, name: String) -> KernelResponse {
        if let Some(job) = self.schedule_manager.get_by_name(&name).await {
            match self.schedule_manager.resume(&job.id).await {
                Ok(_) => {
                    self.audit
                        .append(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: TraceID::new(),
                            event_type: agentos_audit::AuditEventType::ScheduledJobResumed,
                            agent_id: None,
                            task_id: None,
                            tool_id: None,
                            details: serde_json::json!({ "job_name": name }),
                            severity: agentos_audit::AuditSeverity::Info,
                        })
                        .ok();
                    KernelResponse::Success { data: None }
                }
                Err(e) => KernelResponse::Error {
                    message: e.to_string(),
                },
            }
        } else {
            KernelResponse::Error {
                message: format!("Schedule {} not found", name),
            }
        }
    }

    async fn cmd_delete_schedule(&self, name: String) -> KernelResponse {
        if let Some(job) = self.schedule_manager.get_by_name(&name).await {
            match self.schedule_manager.delete(&job.id).await {
                Ok(_) => {
                    self.audit
                        .append(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: TraceID::new(),
                            event_type: agentos_audit::AuditEventType::ScheduledJobDeleted,
                            agent_id: None,
                            task_id: None,
                            tool_id: None,
                            details: serde_json::json!({ "job_name": name }),
                            severity: agentos_audit::AuditSeverity::Info,
                        })
                        .ok();
                    KernelResponse::Success { data: None }
                }
                Err(e) => KernelResponse::Error {
                    message: e.to_string(),
                },
            }
        } else {
            KernelResponse::Error {
                message: format!("Schedule {} not found", name),
            }
        }
    }

    async fn cmd_run_background(
        &self,
        name: String,
        agent_name: String,
        task: String,
        detach: bool,
    ) -> KernelResponse {
        match self
            .create_background_task(name.clone(), agent_name, task, detach)
            .await
        {
            Ok(id) => {
                self.audit
                    .append(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::BackgroundTaskStarted,
                        agent_id: None,
                        task_id: Some(id),
                        tool_id: None,
                        details: serde_json::json!({ "bg_name": name }),
                        severity: agentos_audit::AuditSeverity::Info,
                    })
                    .ok();
                KernelResponse::Success {
                    data: Some(serde_json::json!({ "task_id": id.to_string() })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    async fn cmd_list_background(&self) -> KernelResponse {
        KernelResponse::BackgroundPoolList(self.background_pool.list_all().await)
    }

    async fn cmd_get_background_logs(&self, name: String, _follow: bool) -> KernelResponse {
        if let Some(task) = self.background_pool.get_by_name(&name).await {
            // Reusing get_task_logs logic
            self.cmd_get_task_logs(task.id).await
        } else {
            KernelResponse::Error {
                message: format!("Background task '{}' not found", name),
            }
        }
    }

    async fn cmd_kill_background(&self, name: String) -> KernelResponse {
        if let Some(task) = self.background_pool.get_by_name(&name).await {
            match self
                .scheduler
                .update_state(&task.id, TaskState::Cancelled)
                .await
            {
                Ok(_) => {
                    self.background_pool
                        .fail(&task.id, "Killed by user".to_string())
                        .await;
                    self.audit
                        .append(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: TraceID::new(),
                            event_type: agentos_audit::AuditEventType::BackgroundTaskKilled,
                            agent_id: None,
                            task_id: Some(task.id),
                            tool_id: None,
                            details: serde_json::json!({ "bg_name": name }),
                            severity: agentos_audit::AuditSeverity::Info,
                        })
                        .ok();
                    KernelResponse::Success { data: None }
                }
                Err(e) => KernelResponse::Error {
                    message: e.to_string(),
                },
            }
        } else {
            KernelResponse::Error {
                message: format!("Background task '{}' not found", name),
            }
        }
    }

    // --- Pipeline Command Handlers ---

    async fn cmd_install_pipeline(&self, yaml: String) -> KernelResponse {
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

    async fn cmd_run_pipeline(&self, name: String, input: String, _detach: bool) -> KernelResponse {
        // Load the pipeline definition from store
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

        // Create an executor that bridges to the kernel
        let executor = KernelPipelineExecutor { kernel: self };

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

    async fn cmd_pipeline_status(&self, run_id: String) -> KernelResponse {
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

    async fn cmd_pipeline_list(&self) -> KernelResponse {
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

    async fn cmd_pipeline_logs(&self, run_id: String, step_id: String) -> KernelResponse {
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

    async fn cmd_remove_pipeline(&self, name: String) -> KernelResponse {
        match self.pipeline_engine.store().remove_pipeline(&name) {
            Ok(()) => {
                tracing::info!(pipeline = %name, "Pipeline removed");
                KernelResponse::Success {
                    data: Some(serde_json::json!({ "removed": name })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    // --- Bundled Core Tool Manifests ---

    const CORE_MANIFESTS: &[(&'static str, &'static str)] = &[
        (
            "file-reader.toml",
            include_str!("../../../tools/core/file-reader.toml"),
        ),
        (
            "file-writer.toml",
            include_str!("../../../tools/core/file-writer.toml"),
        ),
        (
            "memory-search.toml",
            include_str!("../../../tools/core/memory-search.toml"),
        ),
        (
            "memory-write.toml",
            include_str!("../../../tools/core/memory-write.toml"),
        ),
        (
            "data-parser.toml",
            include_str!("../../../tools/core/data-parser.toml"),
        ),
    ];

    /// Install bundled core tool manifests into the runtime directory if not already present.
    fn install_core_manifests(core_dir: &Path) -> Result<(), anyhow::Error> {
        for (filename, content) in Self::CORE_MANIFESTS {
            let dest = core_dir.join(filename);
            if !dest.exists()
                || std::fs::metadata(&dest)
                    .map(|m| m.len() == 0)
                    .unwrap_or(false)
            {
                std::fs::write(&dest, content)?;
            }
        }
        Ok(())
    }
}

/// Bridges the pipeline engine to kernel subsystems for executing agent tasks and tools.
struct KernelPipelineExecutor<'a> {
    kernel: &'a Kernel,
}

#[async_trait::async_trait]
impl<'a> agentos_pipeline::PipelineExecutor for KernelPipelineExecutor<'a> {
    async fn run_agent_task(&self, agent_name: &str, prompt: &str) -> Result<String, AgentOSError> {
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
        let context = ToolExecutionContext {
            data_dir: self.kernel.data_dir.clone(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: Some(self.kernel.vault.clone()),
            hal: Some(self.kernel.hal.clone()),
        };

        let result = self
            .kernel
            .tool_runner
            .execute(tool_name, input, context)
            .await?;
        Ok(serde_json::to_string(&result).unwrap_or_default())
    }
}
