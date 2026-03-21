use crate::agent_list::AgentListTool;
use crate::agent_message::AgentMessageTool;
use crate::archival_insert::ArchivalInsert;
use crate::archival_search::ArchivalSearch;
use crate::data_parser::DataParser;
use crate::datetime::DatetimeTool;
use crate::episodic_list::EpisodicList;
use crate::escalation_status::EscalationStatusTool;
use crate::file_delete::FileDelete;
use crate::file_diff::FileDiff;
use crate::file_editor::FileEditor;
use crate::file_glob::FileGlob;
use crate::file_grep::FileGrep;
use crate::file_lock::FileLockRegistry;
use crate::file_move::FileMove;
use crate::file_reader::FileReader;
use crate::file_writer::FileWriter;
use crate::hardware_info::HardwareInfoTool;
use crate::http_client::HttpClientTool;
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
use crate::procedure_create::ProcedureCreate;
use crate::procedure_delete::ProcedureDelete;
use crate::procedure_list::ProcedureList;
use crate::procedure_search::ProcedureSearch;
use crate::process_manager::ProcessManagerTool;
use crate::shell_exec::ShellExec;
use crate::sys_monitor::SysMonitorTool;
use crate::task_delegate::TaskDelegate;
use crate::task_list::TaskListTool;
use crate::task_status::TaskStatusTool;
use crate::think::ThinkTool;
use crate::traits::{AgentTool, ToolExecutionContext};
use crate::web_fetch::WebFetch;
use agentos_memory::{Embedder, EpisodicStore, ProceduralStore, SemanticStore};
use agentos_types::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::warn;

pub struct ToolRunner {
    tools: HashMap<String, Box<dyn AgentTool>>,
    file_lock_registry: Arc<FileLockRegistry>,
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
        self.register(Box::new(DataParser::new()));
        self.register(Box::new(ShellExec::new()));
        self.register(Box::new(AgentMessageTool::new()));
        self.register(Box::new(TaskDelegate::new()));
        match HttpClientTool::new() {
            Ok(tool) => self.register(Box::new(tool)),
            Err(e) => tracing::error!("Failed to initialize http-client tool: {}", e),
        }
        self.register(Box::new(SysMonitorTool::new()));
        self.register(Box::new(ProcessManagerTool::new()));
        self.register(Box::new(LogReaderTool::new()));
        self.register(Box::new(NetworkMonitorTool::new()));
        self.register(Box::new(HardwareInfoTool::new()));
        self.register(Box::new(ThinkTool::new()));
        self.register(Box::new(DatetimeTool::new()));
        match WebFetch::new() {
            Ok(tool) => self.register(Box::new(tool)),
            Err(e) => tracing::error!("Failed to initialize web-fetch tool: {}", e),
        }
        self.register(Box::new(FileDiff::new()));
        self.register(Box::new(EscalationStatusTool::new()));
        self.register(Box::new(AgentListTool::new()));
        self.register(Box::new(TaskStatusTool::new()));
        self.register(Box::new(TaskListTool::new()));
    }

    pub fn register(&mut self, tool: Box<dyn AgentTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Register the agent-manual tool with a snapshot of tool summaries.
    /// Called by the kernel after the tool registry is fully loaded, so the
    /// manual has an accurate view of all available tools.
    pub fn register_agent_manual(&mut self, tool_summaries: Vec<crate::agent_manual::ToolSummary>) {
        self.register(Box::new(crate::agent_manual::AgentManualTool::new(
            tool_summaries,
        )));
    }

    /// Register the agent-self tool with a snapshot of all available tool names.
    ///
    /// Call this after the tool runner is fully initialised so that `agent-self`
    /// can report the complete tool list to the calling agent.  The list of
    /// available names can be obtained from `self.list_tools()` before calling
    /// this method.
    pub fn register_agent_self(&mut self, tool_names: Vec<String>) {
        self.register(Box::new(crate::agent_self::AgentSelfTool::new(tool_names)));
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

        let tool = self
            .tools
            .get(tool_name)
            .ok_or_else(|| AgentOSError::ToolNotFound(tool_name.to_string()))?;

        // Defense-in-depth: verify permissions at the tool layer
        let required = tool.required_permissions();
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
        self.tools.keys().cloned().collect()
    }

    /// Get the required permissions for a given tool.
    pub fn get_required_permissions(&self, tool_name: &str) -> Option<Vec<(String, PermissionOp)>> {
        self.tools.get(tool_name).map(|t| t.required_permissions())
    }
}
