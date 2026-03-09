use crate::agent_message::AgentMessageTool;
use crate::data_parser::DataParser;
use crate::file_reader::FileReader;
use crate::file_writer::FileWriter;
use crate::hardware_info::HardwareInfoTool;
use crate::http_client::HttpClientTool;
use crate::log_reader::LogReaderTool;
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
}

impl ToolRunner {
    pub fn new(data_dir: &Path) -> Self {
        Self::new_with_model_cache_dir(data_dir, &data_dir.join("models"))
    }

    pub fn new_with_model_cache_dir(data_dir: &Path, model_cache_dir: &Path) -> Self {
        let mut runner = Self {
            tools: HashMap::new(),
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

        // Register all built-in tools
        runner.register(Box::new(FileReader::new()));
        runner.register(Box::new(FileWriter::new()));
        runner.register(Box::new(MemorySearch::new(
            semantic.clone(),
            episodic.clone(),
        )));
        runner.register(Box::new(MemoryWrite::new(semantic, episodic)));
        runner.register(Box::new(DataParser::new()));
        runner.register(Box::new(ShellExec::new()));
        runner.register(Box::new(AgentMessageTool::new()));
        runner.register(Box::new(TaskDelegate::new()));
        runner.register(Box::new(HttpClientTool::new()));
        runner.register(Box::new(SysMonitorTool::new()));
        runner.register(Box::new(ProcessManagerTool::new()));
        runner.register(Box::new(LogReaderTool::new()));
        runner.register(Box::new(NetworkMonitorTool::new()));
        runner.register(Box::new(HardwareInfoTool::new()));
        runner
    }

    pub fn register(&mut self, tool: Box<dyn AgentTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Execute a tool by name. Returns the JSON result.
    pub async fn execute(
        &self,
        tool_name: &str,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let tool = self
            .tools
            .get(tool_name)
            .ok_or_else(|| AgentOSError::ToolNotFound(tool_name.to_string()))?;

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
