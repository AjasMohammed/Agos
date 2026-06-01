use crate::a2a_tools::A2ADelegateTool;
use crate::agent_call::AgentCallTool;
use crate::agent_inbox_dismiss::AgentInboxDismissTool;
use crate::agent_inbox_list::AgentInboxListTool;
use crate::agent_inbox_read::AgentInboxReadTool;
use crate::agent_list::AgentListTool;
use crate::agent_message::AgentMessageTool;
use crate::agent_messages_dismiss::AgentMessagesDismissTool;
use crate::agent_messages_list::AgentMessagesListTool;
use crate::agent_messages_read::AgentMessagesReadTool;
use crate::archival_insert::ArchivalInsert;
use crate::archival_search::ArchivalSearch;
use crate::ask_user::AskUserTool;
use crate::audio::AudioTool;
use crate::bluetooth::BluetoothTool;
use crate::cancel_agent::CancelAgentTool;
use crate::channel_send::ChannelSendTool;
use crate::context_memory_read::ContextMemoryReadTool;
use crate::context_memory_update::ContextMemoryUpdateTool;
use crate::coordination::{AwaitAgentsTool, SpawnAgentTool, VerifyOutputTool};
use crate::data_parser::DataParser;
use crate::datetime::DatetimeTool;
use crate::display::DisplayConfigTool;
use crate::episodic_list::EpisodicList;
use crate::escalation_status::EscalationStatusTool;
use crate::event_list_available::EventListAvailableTool;
use crate::event_list_subscriptions::EventListSubscriptionsTool;
use crate::event_subscribe::EventSubscribeTool;
use crate::event_unsubscribe::EventUnsubscribeTool;
use crate::file_delete::FileDelete;
use crate::file_diff::FileDiff;
use crate::file_editor::FileEditor;
use crate::file_glob::FileGlob;
use crate::file_grep::FileGrep;
use crate::file_lock::FileLockRegistry;
use crate::file_move::FileMove;
use crate::file_reader::FileReader;
use crate::file_writer::FileWriter;
use crate::get_schedule_runs::GetScheduleRunsTool;
use crate::get_task_logs::GetTaskLogsTool;
use crate::hardware_info::HardwareInfoTool;
use crate::host_package::{resolve_escalator, EscalatorPolicy, HostPackageInstallTool};
use crate::http_client::HttpClientTool;
use crate::list_my_schedules::ListMySchedulesTool;
use crate::log_reader::LogReaderTool;
use crate::memory_block_delete::MemoryBlockDeleteTool;
use crate::memory_block_list::MemoryBlockListTool;
use crate::memory_block_read::MemoryBlockReadTool;
use crate::memory_block_write::MemoryBlockWriteTool;
use crate::memory_delete::MemoryDelete;
use crate::memory_read::MemoryRead;
use crate::memory_search::MemorySearch;
use crate::memory_stats::MemoryStats;
use crate::memory_write::MemoryWrite;
use crate::network_monitor::NetworkMonitorTool;
use crate::network_sockets::NetworkSocketsTool;
use crate::notify_user::NotifyUserTool;
use crate::poll_agent::PollAgentTool;
use crate::printer::PrinterTool;
use crate::procedure_create::ProcedureCreate;
use crate::procedure_delete::ProcedureDelete;
use crate::procedure_list::ProcedureList;
use crate::procedure_search::ProcedureSearch;
use crate::process_manager::ProcessManagerTool;
use crate::raw_usb::RawUsbTool;
use crate::schedule_control::ScheduleControlTool;
use crate::schedule_once::{CancelOnceJobTool, ListOnceJobsTool, ScheduleOnceTool};
use crate::schedule_recurring::ScheduleRecurringTool;
use crate::set_timer::{CancelTimerTool, ListTimersTool, SetTimerTool};
use crate::shell_exec::ShellExec;
use crate::sys_monitor::SysMonitorTool;
use crate::system_mounts::SystemMountsTool;
use crate::system_open_files::SystemOpenFilesTool;
use crate::system_services::SystemServicesTool;
use crate::task_delegate::TaskDelegate;
use crate::task_list::TaskListTool;
use crate::task_spawn_async::TaskSpawnAsyncTool;
use crate::task_status::TaskStatusTool;
use crate::think::ThinkTool;
use crate::traits::{AgentTool, ToolExecutionContext};
use crate::usb_storage::UsbStorageTool;
use crate::user_file_reader::UserFileReader;
use crate::web_fetch::WebFetch;
use crate::web_search::WebSearchTool;
use crate::webcam::WebcamTool;
use agentos_memory::{Embedder, EpisodicStore, ProceduralStore, SemanticStore};
use agentos_types::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::warn;

pub struct ToolRunner {
    tools: HashMap<String, Box<dyn AgentTool>>,
    file_lock_registry: Arc<FileLockRegistry>,
    /// Tools registered at runtime (e.g. via `agentos mcp attach`).
    dynamic_tools: std::sync::RwLock<HashMap<String, Arc<dyn AgentTool>>>,
    /// Monotonic counter bumped on every dynamic registration or removal.
    /// `&self` callers use an atomic so no lock is needed. Consumers detect
    /// stale cached state cheaply. Ordering with the map contents is provided
    /// by `dynamic_tools`'s own RwLock, not by this atomic — always take the
    /// read lock before reading the revision if you need a consistent snapshot.
    dynamic_revision: std::sync::atomic::AtomicU64,
}

impl ToolRunner {
    pub fn new(data_dir: &Path) -> Result<Self, AgentOSError> {
        Self::new_with_model_cache_dir(data_dir, &data_dir.join("models"))
    }

    pub fn new_with_model_cache_dir(
        data_dir: &Path,
        model_cache_dir: &Path,
    ) -> Result<Self, AgentOSError> {
        let mut runner = Self {
            tools: HashMap::new(),
            file_lock_registry: Arc::new(FileLockRegistry::new()),
            dynamic_tools: std::sync::RwLock::new(HashMap::new()),
            dynamic_revision: std::sync::atomic::AtomicU64::new(0),
        };

        // Initialize shared memory stores
        let embedder = Arc::new(match Embedder::with_cache_dir(model_cache_dir) {
            Ok(embedder) => embedder,
            Err(cache_err) => {
                warn!(
                    error = %cache_err,
                    cache_dir = %model_cache_dir.display(),
                    "Failed to initialize embedder with configured cache dir; falling back to default cache"
                );
                Embedder::new().map_err(|e| {
                    AgentOSError::StorageError(format!(
                        "Failed to initialize embedding model: {}",
                        e
                    ))
                })?
            }
        });
        let semantic = Arc::new(SemanticStore::open_with_embedder(
            data_dir,
            embedder.clone(),
        )?);
        let episodic = Arc::new(EpisodicStore::open(data_dir)?);
        let procedural = Arc::new(ProceduralStore::open_with_embedder(data_dir, embedder)?);

        runner.register_memory_tools(semantic, episodic, procedural);
        Ok(runner)
    }

    pub fn new_with_shared_memory(
        semantic: Arc<SemanticStore>,
        episodic: Arc<EpisodicStore>,
        procedural: Arc<ProceduralStore>,
    ) -> Self {
        let mut runner = Self {
            tools: HashMap::new(),
            file_lock_registry: Arc::new(FileLockRegistry::new()),
            dynamic_tools: std::sync::RwLock::new(HashMap::new()),
            dynamic_revision: std::sync::atomic::AtomicU64::new(0),
        };
        runner.register_memory_tools(semantic, episodic, procedural);
        runner
    }

    fn register_memory_tools(
        &mut self,
        semantic: Arc<SemanticStore>,
        episodic: Arc<EpisodicStore>,
        procedural: Arc<ProceduralStore>,
    ) {
        // Register all built-in tools
        self.register(Box::new(FileReader::new()));
        self.register(Box::new(FileWriter::new()));
        self.register(Box::new(FileEditor::new()));
        self.register(Box::new(FileGlob::new()));
        self.register(Box::new(FileGrep::new()));
        self.register(Box::new(FileDelete::new()));
        self.register(Box::new(FileMove::new()));
        self.register(Box::new(MemorySearch::new(
            semantic.clone(),
            episodic.clone(),
        )));
        self.register(Box::new(MemoryWrite::new(
            semantic.clone(),
            episodic.clone(),
        )));
        self.register(Box::new(ArchivalInsert::new(semantic.clone())));
        self.register(Box::new(ArchivalSearch::new(semantic.clone())));
        self.register(Box::new(MemoryDelete::new(
            semantic.clone(),
            episodic.clone(),
        )));
        self.register(Box::new(MemoryStats::new(
            semantic.clone(),
            episodic.clone(),
            procedural.clone(),
        )));
        self.register(Box::new(ProcedureCreate::new(procedural.clone())));
        self.register(Box::new(ProcedureDelete::new(procedural.clone())));
        self.register(Box::new(ProcedureList::new(procedural.clone())));
        self.register(Box::new(ProcedureSearch::new(procedural.clone())));
        self.register(Box::new(MemoryRead::new(
            semantic.clone(),
            episodic.clone(),
        )));
        self.register(Box::new(EpisodicList::new(episodic.clone())));
        self.register(Box::new(MemoryBlockWriteTool::new()));
        self.register(Box::new(MemoryBlockReadTool::new()));
        self.register(Box::new(MemoryBlockListTool::new()));
        self.register(Box::new(MemoryBlockDeleteTool::new()));
        self.register(Box::new(ContextMemoryReadTool::new()));
        self.register(Box::new(ContextMemoryUpdateTool::new()));
        self.register(Box::new(DataParser::new()));
        self.register(Box::new(ShellExec::new()));
        // host-package-install is registered with an empty allowlist + no
        // escalator by default. Operator must opt in by setting
        // [tools.host_package].enabled = true and rebuilding the runner with
        // a real config. Until then any call returns an explanatory error.
        self.register(Box::new(HostPackageInstallTool::new(
            Vec::new(),
            Vec::new(),
            resolve_escalator(&EscalatorPolicy::None),
        )));
        self.register(Box::new(AgentMessageTool::new()));
        self.register(Box::new(TaskDelegate::new()));
        self.register(Box::new(TaskSpawnAsyncTool::new()));
        match HttpClientTool::new() {
            Ok(tool) => self.register(Box::new(tool)),
            Err(e) => tracing::error!("Failed to initialize http-client tool: {}", e),
        }
        self.register(Box::new(SysMonitorTool::new()));
        self.register(Box::new(ProcessManagerTool::new()));
        self.register(Box::new(LogReaderTool::new()));
        self.register(Box::new(NetworkMonitorTool::new()));
        self.register(Box::new(HardwareInfoTool::new()));
        self.register(Box::new(AudioTool::new()));
        self.register(Box::new(BluetoothTool::new()));
        self.register(Box::new(DisplayConfigTool::new()));
        self.register(Box::new(PrinterTool::new()));
        self.register(Box::new(RawUsbTool::new()));
        self.register(Box::new(UsbStorageTool::new()));
        self.register(Box::new(WebcamTool::new()));
        // Host-introspection tools — read real host state via HAL (procfs / D-Bus),
        // since shell-exec runs in a sandboxed PID/network namespace.
        self.register(Box::new(NetworkSocketsTool::new()));
        self.register(Box::new(SystemMountsTool::new()));
        self.register(Box::new(SystemOpenFilesTool::new()));
        self.register(Box::new(SystemServicesTool::new()));
        self.register(Box::new(ThinkTool::new()));
        self.register(Box::new(DatetimeTool::new()));
        match WebFetch::new() {
            Ok(tool) => self.register(Box::new(tool)),
            Err(e) => tracing::error!("Failed to initialize web-fetch tool: {}", e),
        }
        self.register(Box::new(WebSearchTool::new()));
        self.register(Box::new(UserFileReader::new()));
        self.register(Box::new(FileDiff::new()));
        self.register(Box::new(EscalationStatusTool::new()));
        self.register(Box::new(AgentListTool::new()));
        self.register(Box::new(AgentInboxListTool::new()));
        self.register(Box::new(AgentInboxReadTool::new()));
        self.register(Box::new(AgentInboxDismissTool::new()));
        self.register(Box::new(AgentMessagesListTool::new()));
        self.register(Box::new(AgentMessagesReadTool::new()));
        self.register(Box::new(AgentMessagesDismissTool::new()));
        self.register(Box::new(NotifyUserTool::new()));
        self.register(Box::new(ChannelSendTool::new()));
        self.register(Box::new(AskUserTool::new()));
        self.register(Box::new(TaskStatusTool::new()));
        self.register(Box::new(TaskListTool::new()));
        self.register(Box::new(AgentCallTool::new()));
        self.register(Box::new(SpawnAgentTool::new()));
        self.register(Box::new(AwaitAgentsTool::new()));
        self.register(Box::new(VerifyOutputTool::new()));
        self.register(Box::new(PollAgentTool::new()));
        self.register(Box::new(CancelAgentTool::new()));
        self.register(Box::new(A2ADelegateTool::new()));
        self.register(Box::new(EventSubscribeTool::new()));
        self.register(Box::new(EventUnsubscribeTool::new()));
        self.register(Box::new(EventListSubscriptionsTool::new()));
        self.register(Box::new(EventListAvailableTool::new()));

        // Scheduling tools — emit _kernel_action, dispatched by kernel.
        self.register(Box::new(SetTimerTool::new()));
        self.register(Box::new(CancelTimerTool::new()));
        self.register(Box::new(ListTimersTool::new()));
        self.register(Box::new(ScheduleOnceTool::new()));
        self.register(Box::new(ScheduleRecurringTool::new()));
        self.register(Box::new(ScheduleControlTool::new()));
        self.register(Box::new(CancelOnceJobTool::new()));
        self.register(Box::new(ListOnceJobsTool::new()));
        // Schedule self-inspection — agents query their own schedules + run
        // history; ownership enforced kernel-side via creator_agent_id.
        self.register(Box::new(ListMySchedulesTool::new()));
        self.register(Box::new(GetScheduleRunsTool::new()));
        self.register(Box::new(GetTaskLogsTool::new()));

        // KMC bridge tools — route to kernel capability providers.
        for name in crate::kmc_tools::KMC_TOOL_NAMES {
            if let Some(tool) = crate::kmc_tools::build_kmc_tool(name) {
                self.register(tool);
            }
        }
    }

    pub fn register(&mut self, tool: Box<dyn AgentTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Register a tool at runtime without requiring `&mut self`.
    ///
    /// Used by `agentos mcp attach` to add MCP tools to a running kernel.
    /// Dynamic tools are consulted after static tools — if a name conflicts,
    /// the static tool takes precedence. Dynamic registrations are lost on
    /// kernel restart (they are not persisted to config).
    pub fn register_dynamic(&self, tool: Box<dyn AgentTool>) {
        let name = tool.name().to_string();
        self.dynamic_tools
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, Arc::from(tool));
        self.dynamic_revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Remove a dynamically registered tool by name. Returns `true` if removed.
    pub fn unregister_dynamic(&self, name: &str) -> bool {
        let removed = self
            .dynamic_tools
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .is_some();
        if removed {
            self.dynamic_revision
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        removed
    }

    /// Monotonic counter bumped on every `register_dynamic`/`unregister_dynamic`.
    /// Callers can detect stale state without holding any lock.
    pub fn dynamic_revision(&self) -> u64 {
        self.dynamic_revision
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Register scratchpad tools with a shared `ScratchpadStore`.
    /// Called by the kernel after the scratchpad store is initialised.
    pub fn register_scratchpad_tools(
        &mut self,
        store: std::sync::Arc<agentos_scratch::ScratchpadStore>,
    ) {
        self.register(Box::new(crate::scratch_write::ScratchWriteTool::new(
            store.clone(),
        )));
        self.register(Box::new(crate::scratch_read::ScratchReadTool::new(
            store.clone(),
        )));
        self.register(Box::new(crate::scratch_search::ScratchSearchTool::new(
            store.clone(),
        )));
        self.register(Box::new(crate::scratch_links::ScratchLinksTool::new(
            store.clone(),
        )));
        self.register(Box::new(crate::scratch_graph::ScratchGraphTool::new(
            store.clone(),
        )));
        self.register(Box::new(crate::scratch_delete::ScratchDeleteTool::new(
            store,
        )));
    }

    /// Register the agent-manual tool with a shared tool catalogue.
    /// Called by the kernel after the tool registry is fully loaded, so the
    /// manual has an accurate view of all available tools.
    pub fn register_agent_manual(
        &mut self,
        tool_summaries: crate::agent_manual::SharedToolSummaries,
    ) {
        self.register(Box::new(crate::agent_manual::AgentManualTool::new(
            tool_summaries,
        )));
    }

    /// Register the agent-manual tool with both the tool catalogue and a live
    /// view of connected channels. The manual filters per-platform sections to
    /// match what the operator has actually wired up.
    pub fn register_agent_manual_with_channels(
        &mut self,
        tool_summaries: crate::agent_manual::SharedToolSummaries,
        connected_channels: crate::agent_manual::SharedConnectedChannels,
    ) {
        self.register(Box::new(
            crate::agent_manual::AgentManualTool::new_with_channels(
                tool_summaries,
                connected_channels,
            ),
        ));
    }

    /// Register the agent-manual tool with the full set of live snapshots:
    /// tool catalogue, connected channels, and installed skills. Mirrors the
    /// MCP server inventory pattern so the `skills` section can render the
    /// real inventory + per-skill drill-down.
    pub fn register_agent_manual_full(
        &mut self,
        tool_summaries: crate::agent_manual::SharedToolSummaries,
        connected_channels: crate::agent_manual::SharedConnectedChannels,
        installed_skills: crate::agent_manual::SharedInstalledSkills,
    ) {
        self.register(Box::new(crate::agent_manual::AgentManualTool::new_full(
            tool_summaries,
            connected_channels,
            installed_skills,
        )));
    }

    /// Register the skill-prompt tool. Reads the same `installed_skills`
    /// snapshot the agent-manual `skills` section uses, so a chat agent can
    /// fetch a skill's full system prompt + tool allowlist on demand.
    pub fn register_skill_prompt(
        &mut self,
        installed_skills: crate::agent_manual::SharedInstalledSkills,
    ) {
        self.register(Box::new(crate::skill_prompt::SkillPromptTool::new(
            installed_skills,
        )));
    }

    /// Register the skill-create tool. Lets an agent author and install a
    /// skill at runtime; gated by `risk_class = control_plane` in the
    /// manifest so every call requires explicit human approval.
    pub fn register_skill_create(
        &mut self,
        user_skills_dir: std::path::PathBuf,
        installer: crate::skill_create::SharedSkillInstaller,
        installed_skills: crate::agent_manual::SharedInstalledSkills,
    ) {
        self.register(Box::new(crate::skill_create::SkillCreateTool::new(
            user_skills_dir,
            installer,
            installed_skills,
        )));
    }

    /// Register the list-tools tool with a shared tool catalogue.
    pub fn register_list_tools(
        &mut self,
        tool_summaries: crate::agent_manual::SharedToolSummaries,
    ) {
        self.register(Box::new(crate::list_tools::ListToolsTool::new(
            tool_summaries,
        )));
    }

    /// Register the describe-tool tool with a shared tool catalogue.
    pub fn register_describe_tool(
        &mut self,
        tool_summaries: crate::agent_manual::SharedToolSummaries,
    ) {
        self.register(Box::new(crate::describe_tool::DescribeToolTool::new(
            tool_summaries,
        )));
    }

    /// Register the search-tools tool with a shared tool catalogue and a shared
    /// embedder for semantic ranking. Pass a no-op embedder to force the
    /// substring-only fallback.
    pub fn register_search_tools(
        &mut self,
        tool_summaries: crate::agent_manual::SharedToolSummaries,
        embedder: Arc<Embedder>,
    ) {
        self.register(Box::new(crate::search_tools::SearchToolsTool::new(
            tool_summaries,
            embedder,
        )));
    }

    /// Register the agent-self tool with a snapshot of all available tool names.
    ///
    /// Call this after the tool runner is fully initialised so that `agent-self`
    /// can report the complete tool list to the calling agent.  The list of
    /// available names can be obtained from `self.list_tools()` before calling
    /// this method.
    pub fn register_agent_self(&mut self, tool_count: usize) {
        self.register(Box::new(crate::agent_self::AgentSelfTool::new(tool_count)));
    }

    /// Suggest up to `limit` registered tool names closest to `name` by
    /// case-insensitive Levenshtein distance, with a substring tie-breaker.
    /// Used to enrich `ToolNotFound` errors so a model that hallucinates
    /// a tool name (`user-file-reader` vs `file-reader`) gets back a
    /// usable hint in one round-trip instead of looping.
    fn suggest_close_tool_names(&self, name: &str, limit: usize) -> Vec<String> {
        fn levenshtein(a: &str, b: &str) -> usize {
            let a: Vec<char> = a.chars().collect();
            let b: Vec<char> = b.chars().collect();
            let (n, m) = (a.len(), b.len());
            if n == 0 {
                return m;
            }
            if m == 0 {
                return n;
            }
            let mut prev: Vec<usize> = (0..=m).collect();
            let mut curr = vec![0usize; m + 1];
            for i in 1..=n {
                curr[0] = i;
                for j in 1..=m {
                    let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
                    curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
                }
                std::mem::swap(&mut prev, &mut curr);
            }
            prev[m]
        }

        let needle = name.to_lowercase();
        let mut all: Vec<String> = self.tools.keys().cloned().collect();
        if let Ok(dynamic) = self.dynamic_tools.read() {
            all.extend(dynamic.keys().cloned());
        }

        let mut scored: Vec<(usize, bool, &String)> = all
            .iter()
            .map(|cand| {
                let cand_lower = cand.to_lowercase();
                let dist = levenshtein(&needle, &cand_lower);
                let contains = cand_lower.contains(&needle) || needle.contains(&cand_lower);
                (dist, !contains, cand)
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        // Reject suggestions that are wildly different (more than half the chars).
        let max_dist = (needle.len() / 2).max(3);
        scored
            .into_iter()
            .filter(|(d, _, _)| *d <= max_dist)
            .take(limit)
            .map(|(_, _, s)| s.clone())
            .collect()
    }

    /// Execute a tool by name. Returns the JSON result.
    ///
    /// Defense-in-depth: verifies permissions even if the kernel already checked,
    /// so that any code path that bypasses the kernel's pre-check (e.g. pipeline
    /// step execution, background tasks) is still gated.
    pub async fn execute(
        &self,
        tool_name: &str,
        payload: serde_json::Value,
        mut context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        // Inject the shared file lock registry so file tools can coordinate
        // exclusive access across concurrent agents.
        context.file_lock_registry = Some(self.file_lock_registry.clone());

        // Auto-correct `_` ↔ `-` typos before lookup. Small models often emit
        // `describe_tool` for `describe-tool` (Python-naming bias). Re-resolve
        // against the actual registry; only takes effect if the alternate
        // spelling exists. Saves a wasted iteration for every typo.
        let resolved_name: String = if !self.tools.contains_key(tool_name)
            && !self
                .dynamic_tools
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(tool_name)
        {
            let alternates = [tool_name.replace('_', "-"), tool_name.replace('-', "_")];
            alternates
                .into_iter()
                .find(|alt| {
                    alt != tool_name
                        && (self.tools.contains_key(alt.as_str())
                            || self
                                .dynamic_tools
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .contains_key(alt.as_str()))
                })
                .unwrap_or_else(|| tool_name.to_string())
        } else {
            tool_name.to_string()
        };
        if resolved_name != tool_name {
            tracing::info!(
                requested = tool_name,
                resolved = %resolved_name,
                "Tool name auto-corrected from `_`/`-` typo"
            );
        }
        let tool_name = resolved_name.as_str();

        // Check static tools first; fall back to dynamic (runtime-registered) tools.
        // The Arc clone releases the RwLock guard before any await point.
        let dynamic: Option<Arc<dyn AgentTool>> = if !self.tools.contains_key(tool_name) {
            self.dynamic_tools
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(tool_name)
                .cloned()
        } else {
            None
        };
        let tool: &dyn AgentTool = match self.tools.get(tool_name) {
            Some(t) => t.as_ref(),
            None => match dynamic.as_deref() {
                Some(t) => t,
                None => {
                    let suggestions = self.suggest_close_tool_names(tool_name, 3);
                    let label = if suggestions.is_empty() {
                        tool_name.to_string()
                    } else {
                        format!("{tool_name} (did you mean: {}?)", suggestions.join(", "))
                    };
                    return Err(AgentOSError::ToolNotFound(label));
                }
            },
        };

        // Defense-in-depth: verify permissions at the tool layer
        let required = tool.required_permissions_for(&payload);
        for (resource, op) in &required {
            if !context.permissions.check(resource, *op) {
                tracing::warn!(
                    tool = tool_name,
                    resource = resource.as_str(),
                    operation = ?op,
                    agent = %context.agent_id,
                    "Tool runner permission denied (defense-in-depth)"
                );
                return Err(AgentOSError::PermissionDenied {
                    resource: resource.clone(),
                    operation: format!("{:?}", op),
                });
            }
        }

        tracing::info!(tool = tool_name, task_id = %context.task_id, "Executing tool");

        let start = std::time::Instant::now();
        let result = tool.execute(payload, context).await;
        let duration = start.elapsed();

        match &result {
            Ok(_) => tracing::info!(
                tool = tool_name,
                duration_ms = duration.as_millis() as u64,
                "Tool execution completed"
            ),
            Err(e) => tracing::warn!(
                tool = tool_name,
                duration_ms = duration.as_millis() as u64,
                error = %e,
                "Tool execution failed"
            ),
        }

        result
    }

    /// Get the list of all registered tools (for system prompt).
    pub fn list_tools(&self) -> Vec<String> {
        let dynamic = self.dynamic_tools.read().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        // Extend with dynamic tools, skipping any name already in static map.
        names.extend(
            dynamic
                .keys()
                .filter(|n| !self.tools.contains_key(*n))
                .cloned(),
        );
        names
    }

    /// Get the required permissions for a given tool.
    pub fn get_required_permissions(&self, tool_name: &str) -> Option<Vec<(String, PermissionOp)>> {
        if let Some(t) = self.tools.get(tool_name) {
            return Some(t.required_permissions());
        }
        self.dynamic_tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(tool_name)
            .map(|t| t.required_permissions())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct NoopTool(String);

    #[async_trait]
    impl crate::traits::AgentTool for NoopTool {
        fn name(&self) -> &str {
            &self.0
        }
        fn required_permissions(&self) -> Vec<(String, agentos_types::PermissionOp)> {
            vec![]
        }
        async fn execute(
            &self,
            _payload: serde_json::Value,
            _ctx: crate::traits::ToolExecutionContext,
        ) -> Result<serde_json::Value, agentos_types::AgentOSError> {
            Ok(serde_json::json!({}))
        }
    }

    #[test]
    fn dynamic_revision_bumps_on_register_and_unregister() {
        // Build a ToolRunner without touching the file system or embedding model.
        let runner = ToolRunner {
            tools: std::collections::HashMap::new(),
            file_lock_registry: Arc::new(crate::file_lock::FileLockRegistry::new()),
            dynamic_tools: std::sync::RwLock::new(std::collections::HashMap::new()),
            dynamic_revision: std::sync::atomic::AtomicU64::new(0),
        };
        assert_eq!(runner.dynamic_revision(), 0);
        runner.register_dynamic(Box::new(NoopTool("dyn-a".into())));
        assert_eq!(runner.dynamic_revision(), 1);
        runner.register_dynamic(Box::new(NoopTool("dyn-b".into())));
        assert_eq!(runner.dynamic_revision(), 2);
        let removed = runner.unregister_dynamic("dyn-a");
        assert!(removed);
        assert_eq!(runner.dynamic_revision(), 3);
        // Removing a non-existent name doesn't bump.
        let not_removed = runner.unregister_dynamic("nonexistent");
        assert!(!not_removed);
        assert_eq!(runner.dynamic_revision(), 3);
    }
}
