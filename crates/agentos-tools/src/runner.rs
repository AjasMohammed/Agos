use crate::agent_message::AgentMessageTool;
use crate::archival_insert::ArchivalInsert;
use crate::archival_search::ArchivalSearch;
use crate::data_parser::DataParser;
use crate::file_lock::FileLockRegistry;
use crate::file_reader::FileReader;
use crate::file_writer::FileWriter;
use crate::hardware_info::HardwareInfoTool;
use crate::http_client::HttpClientTool;
use crate::log_reader::LogReaderTool;
use crate::memory_block_delete::MemoryBlockDeleteTool;
use crate::memory_block_list::MemoryBlockListTool;
use crate::memory_block_read::MemoryBlockReadTool;
use crate::memory_block_write::MemoryBlockWriteTool;
use crate::memory_search::MemorySearch;
use crate::memory_write::MemoryWrite;
use crate::network_monitor::NetworkMonitorTool;
use crate::process_manager::ProcessManagerTool;
use crate::shell_exec::ShellExec;
use crate::sys_monitor::SysMonitorTool;
use crate::task_delegate::TaskDelegate;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::{Embedder, EpisodicStore, SemanticStore};
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
    pub fn new(data_dir: &Path) -> Self {
        Self::new_with_model_cache_dir(data_dir, &data_dir.join("models"))
    }

    pub fn new_with_model_cache_dir(data_dir: &Path, model_cache_dir: &Path) -> Self {
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
                Embedder::new().expect("Failed to initialize embedding model")
            }
        });
        let semantic = Arc::new(
            SemanticStore::open_with_embedder(data_dir, embedder)
                .expect("Failed to open semantic memory store"),
        );
        let episodic =
            Arc::new(EpisodicStore::open(data_dir).expect("Failed to open episodic memory store"));

        runner.register_memory_tools(semantic, episodic);
        runner
    }

    pub fn new_with_shared_memory(
        semantic: Arc<SemanticStore>,
        episodic: Arc<EpisodicStore>,
    ) -> Self {
        let mut runner = Self {
            tools: HashMap::new(),
            file_lock_registry: Arc::new(FileLockRegistry::new()),
        };
        runner.register_memory_tools(semantic, episodic);
        runner
    }

    fn register_memory_tools(
        &mut self,
        semantic: Arc<SemanticStore>,
        episodic: Arc<EpisodicStore>,
    ) {
        // Register all built-in tools
        self.register(Box::new(FileReader::new()));
        self.register(Box::new(FileWriter::new()));
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
        self.register(Box::new(MemoryBlockWriteTool::new()));
        self.register(Box::new(MemoryBlockReadTool::new()));
        self.register(Box::new(MemoryBlockListTool::new()));
        self.register(Box::new(MemoryBlockDeleteTool::new()));
        self.register(Box::new(DataParser::new()));
        self.register(Box::new(ShellExec::new()));
        self.register(Box::new(AgentMessageTool::new()));
        self.register(Box::new(TaskDelegate::new()));
        self.register(Box::new(HttpClientTool::new()));
        self.register(Box::new(SysMonitorTool::new()));
        self.register(Box::new(ProcessManagerTool::new()));
        self.register(Box::new(LogReaderTool::new()));
        self.register(Box::new(NetworkMonitorTool::new()));
        self.register(Box::new(HardwareInfoTool::new()));
    }

    pub fn register(&mut self, tool: Box<dyn AgentTool>) {
        self.tools.insert(tool.name().to_string(), tool);
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

        tracing::info!(
            tool = tool_name,
            duration_ms = duration.as_millis() as u64,
            "Tool execution completed"
        );

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
