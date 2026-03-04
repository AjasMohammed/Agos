use crate::data_parser::DataParser;
use crate::file_reader::FileReader;
use crate::file_writer::FileWriter;
use crate::memory_search::MemorySearch;
use crate::memory_write::MemoryWrite;
use crate::shell_exec::ShellExec;
use crate::agent_message::AgentMessageTool;
use crate::task_delegate::TaskDelegate;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use std::collections::HashMap;
use std::path::Path;

pub struct ToolRunner {
    tools: HashMap<String, Box<dyn AgentTool>>,
}

impl ToolRunner {
    pub fn new(data_dir: &Path) -> Self {
        let mut runner = Self {
            tools: HashMap::new(),
        };
        // Register all built-in tools
        runner.register(Box::new(FileReader::new()));
        runner.register(Box::new(FileWriter::new()));
        runner.register(Box::new(MemorySearch::new(data_dir)));
        runner.register(Box::new(MemoryWrite::new(data_dir)));
        runner.register(Box::new(DataParser::new()));
        runner.register(Box::new(ShellExec::new()));
        runner.register(Box::new(AgentMessageTool::new()));
        runner.register(Box::new(TaskDelegate::new()));
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

        tracing::info!(tool = tool_name, duration_ms = duration.as_millis() as u64, "Tool execution completed");

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
