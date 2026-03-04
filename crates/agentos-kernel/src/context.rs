use agentos_types::*;
use std::collections::HashMap;
use tokio::sync::RwLock;

pub struct ContextManager {
    /// Per-task context windows.
    windows: RwLock<HashMap<TaskID, ContextWindow>>,
    max_entries: usize,
}

impl ContextManager {
    pub fn new(max_entries: usize) -> Self {
        Self {
            windows: RwLock::new(HashMap::new()),
            max_entries,
        }
    }

    /// Create a new context window for a task with the system prompt.
    pub async fn create_context(&self, task_id: TaskID, system_prompt: &str) -> ContextID {
        let mut window = ContextWindow::new(self.max_entries);
        let context_id = window.id;

        window.push(ContextEntry {
            role: ContextRole::System,
            content: system_prompt.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });

        self.windows.write().await.insert(task_id, window);
        context_id
    }

    /// Push an entry into a task's context window.
    pub async fn push_entry(
        &self,
        task_id: &TaskID,
        entry: ContextEntry,
    ) -> Result<(), AgentOSError> {
        let mut windows = self.windows.write().await;
        match windows.get_mut(task_id) {
            Some(window) => {
                window.push(entry);
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }

    /// Get the full context for assembling an LLM prompt.
    pub async fn get_context(&self, task_id: &TaskID) -> Result<ContextWindow, AgentOSError> {
        let windows = self.windows.read().await;
        windows
            .get(task_id)
            .cloned()
            .ok_or(AgentOSError::TaskNotFound(*task_id))
    }

    /// Push a tool result into context with sanitization wrappers.
    pub async fn push_tool_result(
        &self,
        task_id: &TaskID,
        tool_name: &str,
        result: &serde_json::Value,
    ) -> Result<(), AgentOSError> {
        let content = format!(
            "[Tool Result: {}]\n{}",
            tool_name,
            serde_json::to_string_pretty(result).unwrap_or_default()
        );
        self.push_entry(
            task_id,
            ContextEntry {
                role: ContextRole::ToolResult,
                content,
                timestamp: chrono::Utc::now(),
                metadata: Some(ContextMetadata {
                    tool_name: Some(tool_name.to_string()),
                    tool_id: None,
                    intent_id: None,
                    tokens_estimated: None,
                }),
            },
        )
        .await
    }

    /// Remove a task's context (on completion/failure).
    pub async fn remove_context(&self, task_id: &TaskID) {
        self.windows.write().await.remove(task_id);
    }
}
