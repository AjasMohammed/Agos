use agentos_types::*;
use std::collections::HashMap;
use tokio::sync::RwLock;

pub struct ContextManager {
    /// Per-task context windows.
    windows: RwLock<HashMap<TaskID, ContextWindow>>,
    max_entries: usize,
    /// Token budget per context window. 0 = no budget enforcement.
    /// Spec §11: compress at 80%, checkpoint+flush at 95%.
    token_budget: usize,
}

impl ContextManager {
    pub fn new(max_entries: usize) -> Self {
        Self {
            windows: RwLock::new(HashMap::new()),
            max_entries,
            token_budget: 0,
        }
    }

    pub fn with_token_budget(max_entries: usize, token_budget: usize) -> Self {
        Self {
            windows: RwLock::new(HashMap::new()),
            max_entries,
            token_budget,
        }
    }

    /// Create a new context window for a task with the system prompt.
    /// The system prompt is pinned with maximum importance.
    pub async fn create_context(&self, task_id: TaskID, system_prompt: &str) -> ContextID {
        let mut window =
            ContextWindow::with_strategy(self.max_entries, OverflowStrategy::SemanticEviction);
        let context_id = window.id;

        window.push(ContextEntry {
            role: ContextRole::System,
            content: system_prompt.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::System,
        });

        self.windows.write().await.insert(task_id, window);
        context_id
    }

    /// Push an entry into a task's context window, then apply token budget
    /// enforcement (Spec §11):
    ///   - ≥80% of `token_budget`: compress oldest entries with a summary
    ///   - ≥95% of `token_budget`: compress + set `window.needs_checkpoint = true`
    ///
    /// Callers can check `drain_checkpoint_flag()` after pushing to learn if a
    /// snapshot should be taken before continuing.
    /// Push an entry into a task's context window.
    ///
    /// Returns `Ok(evicted)` where `evicted` is the number of entries compressed/evicted
    /// by token budget enforcement (0 if no eviction occurred).
    pub async fn push_entry(
        &self,
        task_id: &TaskID,
        entry: ContextEntry,
    ) -> Result<usize, AgentOSError> {
        let mut windows = self.windows.write().await;
        match windows.get_mut(task_id) {
            Some(window) => {
                let pre_count = window.entries.len();
                window.push(entry);
                let mut evicted = 0usize;

                // Token budget enforcement
                if self.token_budget > 0 {
                    let tokens = window.estimated_tokens();
                    let pct = tokens * 100 / self.token_budget;

                    if pct >= 95 {
                        // Critical pressure: compress aggressively + flag for checkpoint
                        let compress_count = window.entries.len() / 3;
                        window.compress_oldest(compress_count.max(1));
                        window.needs_checkpoint = true;
                        tracing::warn!(
                            task_id = %task_id,
                            tokens,
                            budget = self.token_budget,
                            "Context at 95% token budget — checkpoint flagged"
                        );
                    } else if pct >= 80 {
                        // Moderate pressure: compress oldest quarter
                        let compress_count = window.entries.len() / 4;
                        window.compress_oldest(compress_count.max(1));
                        tracing::info!(
                            task_id = %task_id,
                            tokens,
                            budget = self.token_budget,
                            "Context at 80% token budget — compressing oldest entries"
                        );
                    }
                }

                // Eviction happened if final entry count is less than pre_count + 1
                let expected = pre_count + 1;
                if window.entries.len() < expected {
                    evicted = expected - window.entries.len();
                }

                Ok(evicted)
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }

    /// Check if the token budget is fully exhausted (100%) for a task.
    pub async fn is_budget_exhausted(&self, task_id: &TaskID) -> bool {
        if self.token_budget == 0 {
            return false;
        }
        let windows = self.windows.read().await;
        if let Some(window) = windows.get(task_id) {
            let estimated = window.estimated_tokens();
            estimated >= self.token_budget
        } else {
            false
        }
    }

    /// Get the entry count for a task's context window.
    pub async fn entry_count(&self, task_id: &TaskID) -> usize {
        let windows = self.windows.read().await;
        windows.get(task_id).map(|w| w.entries.len()).unwrap_or(0)
    }

    /// Returns `true` and clears the `needs_checkpoint` flag if the context
    /// window for `task_id` has flagged a checkpoint. Call this after pushing
    /// entries to decide whether to take a snapshot.
    pub async fn drain_checkpoint_flag(&self, task_id: &TaskID) -> bool {
        let mut windows = self.windows.write().await;
        if let Some(window) = windows.get_mut(task_id) {
            if window.needs_checkpoint {
                window.needs_checkpoint = false;
                return true;
            }
        }
        false
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
    ///
    /// Tool outputs are treated as untrusted data: delimiter-like sequences are
    /// escaped to prevent prompt injection, and the result is wrapped in typed
    /// delimiters so the LLM can distinguish tool output from system instructions.
    ///
    /// Error results get higher importance (0.8) since the agent needs to know
    /// what failed. Success results get moderate importance (0.5) that decays.
    /// Returns `Ok(evicted_count)` where evicted_count is the number of entries
    /// compressed/evicted by token budget enforcement (0 if none).
    pub async fn push_tool_result(
        &self,
        task_id: &TaskID,
        tool_name: &str,
        result: &serde_json::Value,
    ) -> Result<usize, AgentOSError> {
        use agentos_tools::sanitize;

        let sanitized = sanitize::sanitize_tool_output(tool_name, result);
        let content = sanitize::truncate_if_needed(&sanitized, sanitize::DEFAULT_MAX_OUTPUT_CHARS);

        let is_error = result.get("error").is_some();
        let importance = if is_error { 0.8 } else { 0.5 };

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
                importance,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::default(),
                category: ContextCategory::History,
            },
        )
        .await
    }

    /// Set the partition for the most recent non-system entry in a task's context.
    /// Unlike `get_context()` + `set_partition()`, this writes through to the
    /// internal storage so the change is actually persisted.
    pub async fn set_partition_for_task(
        &self,
        task_id: &TaskID,
        partition: ContextPartition,
    ) -> Result<(), AgentOSError> {
        let mut windows = self.windows.write().await;
        match windows.get_mut(task_id) {
            Some(window) => {
                window.set_partition(partition);
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }

    /// Replace an entire context window (used by rollback).
    pub async fn replace_context(
        &self,
        task_id: &TaskID,
        window: ContextWindow,
    ) -> Result<(), AgentOSError> {
        let mut windows = self.windows.write().await;
        match windows.get_mut(task_id) {
            Some(existing) => {
                *existing = window;
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }

    /// Remove a task's context (on completion/failure).
    pub async fn remove_context(&self, task_id: &TaskID) {
        self.windows.write().await.remove(task_id);
    }
}
