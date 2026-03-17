use crate::agent_registry::AgentRegistry;
use crate::background_pool::BackgroundPool;
use crate::config::{load_config, KernelConfig};
use crate::context::ContextManager;
use crate::schedule_manager::ScheduleManager;
use crate::scheduler::TaskScheduler;
use crate::tool_registry::ToolRegistry;
use agentos_audit::AuditLog;
use agentos_bus::BusServer;
use agentos_capability::profiles::ProfileManager;
use agentos_capability::CapabilityEngine;
use agentos_hal::{
    drivers::{
        gpu::GpuDriver, log_reader::LogReaderDriver, network::NetworkDriver,
        process::ProcessDriver, sensor::SensorDriver, storage::StorageDriver, system::SystemDriver,
    },
    HardwareAbstractionLayer, HardwareRegistry,
};
use agentos_llm::LLMCore;
use agentos_memory::Embedder;
use agentos_pipeline::{PipelineEngine, PipelineStore};
use agentos_sandbox::SandboxExecutor;
use agentos_tools::runner::ToolRunner;
use agentos_types::*;
use agentos_vault::{SecretsVault, ZeroizingString};
use agentos_wasm::WasmToolExecutor;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub struct Kernel {
    pub config: KernelConfig,
    pub audit: Arc<AuditLog>,
    pub vault: Arc<SecretsVault>,
    pub capability_engine: Arc<CapabilityEngine>,
    pub scheduler: Arc<TaskScheduler>,
    pub context_manager: Arc<ContextManager>,
    pub context_compiler: Arc<crate::context_compiler::ContextCompiler>,
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
    pub semantic_memory: Arc<agentos_memory::SemanticStore>,
    pub procedural_memory: Arc<agentos_memory::ProceduralStore>,
    pub retrieval_gate: Arc<crate::retrieval_gate::RetrievalGate>,
    pub retrieval_executor: Arc<crate::retrieval_gate::RetrievalExecutor>,
    pub memory_extraction: Arc<crate::memory_extraction::MemoryExtractionEngine>,
    pub consolidation_engine: Arc<crate::consolidation::ConsolidationEngine>,
    pub memory_blocks: Arc<crate::memory_blocks::MemoryBlockStore>,
    pub schedule_manager: Arc<ScheduleManager>,
    pub background_pool: Arc<BackgroundPool>,
    pub hal: Arc<HardwareAbstractionLayer>,
    pub hardware_registry: Arc<HardwareRegistry>,
    pub schema_registry: Arc<crate::schema_registry::SchemaRegistry>,
    pub pipeline_engine: Arc<PipelineEngine>,
    pub intent_validator: Arc<crate::intent_validator::IntentValidator>,
    pub escalation_manager: Arc<crate::escalation::EscalationManager>,
    pub cost_tracker: Arc<crate::cost_tracker::CostTracker>,
    pub risk_classifier: Arc<crate::risk_classifier::RiskClassifier>,
    pub identity_manager: Arc<crate::identity::IdentityManager>,
    pub injection_scanner: Arc<crate::injection_scanner::InjectionScanner>,
    pub resource_arbiter: Arc<crate::resource_arbiter::ResourceArbiter>,
    pub snapshot_manager: Arc<crate::snapshot::SnapshotManager>,
    pub event_bus: Arc<crate::event_bus::EventBus>,
    /// Task-scoped subscriptions that should be removed when a task reaches terminal state.
    pub(crate) task_scoped_subscriptions: Arc<RwLock<HashMap<TaskID, Vec<SubscriptionID>>>>,
    pub(crate) event_sender: tokio::sync::mpsc::Sender<agentos_types::EventMessage>,
    /// Receiver for event channel — owned behind a mutex so EventDispatcher can be restarted.
    pub(crate) event_receiver:
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<agentos_types::EventMessage>>>,
    /// Receiver for tool lifecycle notifications from ToolRegistry.
    pub(crate) tool_lifecycle_receiver: Arc<
        tokio::sync::Mutex<tokio::sync::mpsc::Receiver<crate::tool_registry::ToolLifecycleEvent>>,
    >,
    /// Receiver for communication notifications from AgentMessageBus.
    pub(crate) comm_notification_receiver: Arc<
        tokio::sync::Mutex<tokio::sync::mpsc::Receiver<crate::agent_message_bus::CommNotification>>,
    >,
    /// Receiver for schedule notifications from ScheduleManager.
    pub(crate) schedule_notification_receiver: Arc<
        tokio::sync::Mutex<
            tokio::sync::mpsc::Receiver<crate::schedule_manager::ScheduleNotification>,
        >,
    >,
    /// Receiver for resource arbiter notifications (preemption/deadlock events).
    pub(crate) arbiter_notification_receiver: Arc<
        tokio::sync::Mutex<
            tokio::sync::mpsc::Receiver<crate::resource_arbiter::ArbiterNotification>,
        >,
    >,
    /// Per-agent rate limiter: enforces command-rate limits across all connections per agent.
    pub(crate) per_agent_rate_limiter:
        Arc<tokio::sync::Mutex<crate::rate_limit::PerAgentRateLimiter>>,
    pub(crate) data_dir: PathBuf,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Token used to signal graceful shutdown to all kernel loops.
    pub cancellation_token: CancellationToken,
}

impl Kernel {
    /// Log an audit entry, emitting a tracing error if the write fails.
    /// Replaces bare `.ok()` calls that silently swallow audit write failures.
    pub(crate) fn audit_log(&self, entry: agentos_audit::AuditEntry) {
        if let Err(e) = self.audit.append(entry) {
            tracing::error!(error = %e, "Failed to write audit log entry");
        }
    }

    /// Boot the kernel: load config, open subsystems, start bus, begin accepting.
    pub async fn boot(
        config_path: &Path,
        vault_passphrase: &ZeroizingString,
    ) -> Result<Self, anyhow::Error> {
        // 1. Load config
        let config = load_config(config_path)?;
        tracing::info!(
            config_path = %config_path.display(),
            ollama_host = %config.ollama.host,
            custom_llm_url = ?config.llm.custom_base_url,
            openai_base_url = ?config.llm.openai_base_url,
            "Kernel configuration loaded"
        );

        // Ensure directories exist. The vault directory is created with 0o700 on Unix
        // so other users on the same host cannot list or access the vault parent directory.
        if let Some(parent) = Path::new(&config.audit.log_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = Path::new(&config.secrets.vault_path).parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, path = ?parent, "Failed to set vault directory permissions to 0o700");
                    });
            }
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

        // 4. Initialize capability engine (loads or generates HMAC signing key from vault)
        let capability_engine = Arc::new(CapabilityEngine::boot(&vault).await);

        // 4.5 Initialize HardwareAbstractionLayer
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(SystemDriver::new()));
        hal.register(Box::new(ProcessDriver::new()));
        hal.register(Box::new(NetworkDriver::new()));
        hal.register(Box::new(SensorDriver::new()));
        hal.register(Box::new(GpuDriver::new()));
        hal.register(Box::new(StorageDriver::new()));

        // Register log reader with app logs only - audit log is not exposed to agents
        let app_logs = HashMap::new();
        let mut system_logs = HashMap::new();
        system_logs.insert(
            "syslog".to_string(),
            Path::new("/var/log/syslog").to_path_buf(),
        );
        hal.register(Box::new(LogReaderDriver::new(app_logs, system_logs)));

        let hal = Arc::new(hal);
        let hardware_registry = Arc::new(HardwareRegistry::new());

        // 5. Load tools (with optional CRL enforcement)
        // NOTE: Tools are loaded before the event channel exists, so boot-time
        // registrations do not emit ToolInstalled events. This is intentional --
        // the initial tool inventory can be queried via `cmd_list_tools`.
        let crl = if let Some(ref crl_path) = config.tools.crl_path {
            let crl_file = Path::new(crl_path);
            if crl_file.exists() {
                match agentos_tools::signing::RevocationList::load_from_file(crl_file) {
                    Ok(loaded) => {
                        tracing::info!(
                            path = %crl_path,
                            revoked = loaded.revoked_pubkeys.len(),
                            "Loaded certificate revocation list"
                        );
                        loaded
                    }
                    Err(e) => {
                        tracing::warn!(path = %crl_path, error = %e, "Failed to load CRL, proceeding without it");
                        agentos_tools::signing::RevocationList::new()
                    }
                }
            } else {
                tracing::warn!(path = %crl_path, "CRL path configured but file not found");
                agentos_tools::signing::RevocationList::new()
            }
        } else {
            agentos_tools::signing::RevocationList::new()
        };
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::load_from_dirs_with_crl(
            Path::new(&config.tools.core_tools_dir),
            Path::new(&config.tools.user_tools_dir),
            crl,
        )?));

        // 5.5 Build schema registry from tool manifests
        let mut schema_registry = crate::schema_registry::SchemaRegistry::new();
        {
            let registry = tool_registry.read().await;
            for loaded in &registry.loaded {
                if let Some(ref schema) = loaded.manifest.input_schema {
                    schema_registry.register(&loaded.manifest.manifest.name, schema.clone());
                    tracing::debug!(
                        tool = %loaded.manifest.manifest.name,
                        "Registered input schema for tool"
                    );
                }
            }
        }
        let schema_registry = Arc::new(schema_registry);

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
        let shared_embedder = Arc::new(
            Embedder::with_cache_dir(&model_cache_dir)
                .map_err(|e| anyhow::anyhow!("Failed to initialize shared embedder: {}", e))?,
        );
        let episodic_memory = Arc::new(agentos_memory::EpisodicStore::open(&data_dir)?);
        let semantic_memory = Arc::new(agentos_memory::SemanticStore::open_with_embedder(
            &data_dir,
            shared_embedder.clone(),
        )?);
        let procedural_memory = Arc::new(agentos_memory::ProceduralStore::open_with_embedder(
            &data_dir,
            shared_embedder,
        )?);
        let mut tool_runner =
            ToolRunner::new_with_shared_memory(semantic_memory.clone(), episodic_memory.clone());

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
        let context_manager = Arc::new(ContextManager::with_token_budget(
            config.kernel.context_window_max_entries,
            config.kernel.context_window_token_budget,
        ));
        let mut context_budget = config.context_budget.clone();
        if let Err(e) = context_budget.validate() {
            tracing::warn!("Invalid context budget config: {} — using defaults", e);
            context_budget = TokenBudget::default();
        }
        let context_compiler = Arc::new(crate::context_compiler::ContextCompiler::new(
            context_budget,
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
        let retrieval_gate = Arc::new(crate::retrieval_gate::RetrievalGate::new(5));
        let retrieval_executor = Arc::new(crate::retrieval_gate::RetrievalExecutor::new(
            semantic_memory.clone(),
            episodic_memory.clone(),
            procedural_memory.clone(),
            tool_registry.clone(),
        ));
        let mut extraction_registry = crate::memory_extraction::ExtractionRegistry::new();
        extraction_registry.register_defaults();
        let memory_extraction = Arc::new(crate::memory_extraction::MemoryExtractionEngine::new(
            extraction_registry,
            semantic_memory.clone(),
            config.memory.extraction.clone(),
        ));
        let consolidation_engine = Arc::new(crate::consolidation::ConsolidationEngine::new(
            episodic_memory.clone(),
            procedural_memory.clone(),
            config.memory.consolidation.clone(),
        ));
        let memory_blocks = Arc::new(crate::memory_blocks::MemoryBlockStore::open(&data_dir)?);
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

        let identity_manager = Arc::new(crate::identity::IdentityManager::new(vault.clone()));

        let snapshot_manager = Arc::new(crate::snapshot::SnapshotManager::new(
            data_dir.join("snapshots"),
            data_dir.clone(), // allowed_root: only paths within data_dir may be snapshotted
            72,               // hours
        ));

        let event_bus = Arc::new(crate::event_bus::EventBus::new());

        // Bounded channel capacity for internal event/notification channels.
        // Provides backpressure to prevent unbounded memory growth under load.
        const CHANNEL_CAPACITY: usize = 1024;

        let (event_sender, event_receiver) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);

        // Create tool lifecycle notification channel and inject sender into registry.
        // The kernel receives these lightweight notifications and converts them into
        // properly HMAC-signed EventMessages with audit trail entries.
        let (tool_lifecycle_sender, tool_lifecycle_receiver) =
            tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        tool_registry
            .write()
            .await
            .set_lifecycle_sender(tool_lifecycle_sender);

        // Create notification channels for communication and schedule subsystems.
        // These subsystems send lightweight notifications; the kernel converts them
        // into properly HMAC-signed EventMessages with audit trail entries.
        let (comm_notif_sender, comm_notif_receiver) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        message_bus.set_notification_sender(comm_notif_sender).await;

        let (schedule_notif_sender, schedule_notif_receiver) =
            tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        schedule_manager
            .set_notification_sender(schedule_notif_sender)
            .await;

        // Create notification channel for resource arbiter (preemption/deadlock events).
        let (arbiter_notif_sender, arbiter_notif_receiver) =
            tokio::sync::mpsc::channel(CHANNEL_CAPACITY);

        let per_agent_rate_limit = config.kernel.per_agent_rate_limit;

        let kernel = Kernel {
            config,
            audit,
            vault,
            capability_engine,
            scheduler,
            context_manager,
            context_compiler,
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
            semantic_memory,
            procedural_memory,
            retrieval_gate,
            retrieval_executor,
            memory_extraction,
            consolidation_engine,
            memory_blocks,
            schedule_manager,
            background_pool,
            hal,
            hardware_registry,
            schema_registry,
            pipeline_engine,
            intent_validator: Arc::new(crate::intent_validator::IntentValidator::new()),
            escalation_manager: Arc::new(crate::escalation::EscalationManager::new()),
            cost_tracker: Arc::new(crate::cost_tracker::CostTracker::new()),
            risk_classifier: Arc::new(crate::risk_classifier::RiskClassifier::new()),
            identity_manager,
            injection_scanner: Arc::new(crate::injection_scanner::InjectionScanner::new()),
            resource_arbiter: {
                let mut arbiter = crate::resource_arbiter::ResourceArbiter::new();
                arbiter.set_arbiter_sender(arbiter_notif_sender);
                Arc::new(arbiter)
            },
            snapshot_manager,
            event_bus,
            task_scoped_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            event_receiver: Arc::new(tokio::sync::Mutex::new(event_receiver)),
            tool_lifecycle_receiver: Arc::new(tokio::sync::Mutex::new(tool_lifecycle_receiver)),
            comm_notification_receiver: Arc::new(tokio::sync::Mutex::new(comm_notif_receiver)),
            schedule_notification_receiver: Arc::new(tokio::sync::Mutex::new(
                schedule_notif_receiver,
            )),
            arbiter_notification_receiver: Arc::new(tokio::sync::Mutex::new(
                arbiter_notif_receiver,
            )),
            per_agent_rate_limiter: Arc::new(tokio::sync::Mutex::new(
                crate::rate_limit::PerAgentRateLimiter::new(per_agent_rate_limit),
            )),
            data_dir,
            started_at: chrono::Utc::now(),
            cancellation_token: CancellationToken::new(),
        };

        // Emit KernelStarted audit event
        kernel.audit_log(agentos_audit::AuditEntry {
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
            reversible: false,
            rollback_ref: None,
        });

        Ok(kernel)
    }

    /// Signal all kernel loops to stop gracefully.
    pub fn shutdown(&self) {
        self.cancellation_token.cancel();
    }

    /// Number of agents currently tracked by the per-agent rate limiter.
    /// Exposed for integration testing; 0 means no rate-limit state is retained.
    pub async fn rate_limiter_tracked_count(&self) -> usize {
        self.per_agent_rate_limiter.lock().await.tracked_count()
    }

    /// Public API: Connect a new agent through the kernel command dispatch path.
    pub async fn api_connect_agent(
        &self,
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
        roles: Vec<String>,
    ) -> Result<(), String> {
        match self
            .cmd_connect_agent(name, provider, model, base_url, roles)
            .await
        {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Disconnect an agent by ID through the kernel command dispatch path.
    pub async fn api_disconnect_agent(&self, agent_id: AgentID) -> Result<(), String> {
        match self.cmd_disconnect_agent(agent_id).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Install a tool from a manifest path through the kernel command dispatch path.
    pub async fn api_install_tool(&self, manifest_path: String) -> Result<(), String> {
        match self.cmd_install_tool(manifest_path).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Remove a tool by name through the kernel command dispatch path.
    pub async fn api_remove_tool(&self, tool_name: String) -> Result<(), String> {
        match self.cmd_remove_tool(tool_name).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Set a secret through the kernel command dispatch path.
    ///
    /// NOTE: `value` is a plain `String`. The caller should zero any `ZeroizingString`
    /// source before this frame is dropped. A future improvement is to accept
    /// `ZeroizingString` here and propagate it through `cmd_set_secret`.
    pub async fn api_set_secret(
        &self,
        name: String,
        value: String,
        scope: SecretScope,
    ) -> Result<(), String> {
        match self.cmd_set_secret(name, value, scope, None).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Revoke a secret through the kernel command dispatch path.
    pub async fn api_revoke_secret(&self, name: String) -> Result<(), String> {
        match self.cmd_revoke_secret(name).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Execute a pipeline with full security enforcement (agent resolution, permission
    /// enforcement, injection scanning, audit logging).
    ///
    /// Public entry point for non-kernel callers such as the web server. Internally
    /// delegates to `cmd_run_pipeline` so all security checks are applied identically
    /// to CLI-initiated runs.
    pub async fn run_pipeline(
        &self,
        name: String,
        input: String,
        detach: bool,
        agent_name: Option<String>,
    ) -> Result<serde_json::Value, String> {
        match self.cmd_run_pipeline(name, input, detach, agent_name).await {
            agentos_bus::KernelResponse::Success { data } => {
                Ok(data.unwrap_or(serde_json::Value::Null))
            }
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }
}
