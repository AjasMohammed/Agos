use crate::config::{ContextConfig, SummarizationMode};
use crate::cost_tracker::CostTracker;
use agentos_types::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Per-task context state: the context window and the agent that owns it.
struct TaskContext {
    window: ContextWindow,
    /// The agent that owns this task's context. Used by LLM summarization.
    agent_id: AgentID,
}

pub struct ContextManager {
    tasks: RwLock<HashMap<TaskID, TaskContext>>,
    max_entries: usize,
    /// Token budget per context window. 0 = no budget enforcement.
    /// Spec §11: compress at 80%, checkpoint+flush at 95%.
    token_budget: usize,
    /// Per-agent LLM adapters for summarization.
    active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn agentos_llm::LLMCore>>>>,
    /// Cost tracker for attributing summarization inference costs.
    cost_tracker: Arc<CostTracker>,
    /// Context configuration (summarization mode, etc.).
    config: ContextConfig,
}

impl ContextManager {
    pub fn new(max_entries: usize) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            max_entries,
            token_budget: 0,
            active_llms: Arc::new(RwLock::new(HashMap::new())),
            cost_tracker: Arc::new(CostTracker::new()),
            config: ContextConfig::default(),
        }
    }

    pub fn with_token_budget(max_entries: usize, token_budget: usize) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            max_entries,
            token_budget,
            active_llms: Arc::new(RwLock::new(HashMap::new())),
            cost_tracker: Arc::new(CostTracker::new()),
            config: ContextConfig::default(),
        }
    }

    pub fn with_full_config(
        max_entries: usize,
        token_budget: usize,
        active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn agentos_llm::LLMCore>>>>,
        cost_tracker: Arc<CostTracker>,
        config: ContextConfig,
    ) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            max_entries,
            token_budget,
            active_llms,
            cost_tracker,
            config,
        }
    }

    /// Create a new context window for a task with the system prompt.
    /// The system prompt is pinned with maximum importance.
    pub async fn create_context(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        system_prompt: &str,
    ) -> ContextID {
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
            is_summary: false,
        });

        self.tasks
            .write()
            .await
            .insert(task_id, TaskContext { window, agent_id });
        context_id
    }

    /// Concat fallback: format entries as truncated snippets (matches legacy compress_oldest format).
    fn summarize_entries_concat(entries: &[ContextEntry]) -> String {
        let parts: Vec<String> = entries
            .iter()
            .map(|e| {
                let label = match e.role {
                    ContextRole::User => "User",
                    ContextRole::Assistant => "Assistant",
                    ContextRole::ToolResult => "ToolResult",
                    ContextRole::System => "System",
                };
                let snippet = if e.content.chars().count() > 150 {
                    format!("{}...", e.content.chars().take(150).collect::<String>())
                } else {
                    e.content.clone()
                };
                format!("[{label}]: {snippet}")
            })
            .collect();
        parts.join("\n")
    }

    /// Attempt LLM-generated summarization. Returns `Ok((summary_text, inference_result))`
    /// on success, `Err` on any failure (no adapter, LLM error, empty response).
    async fn summarize_entries_llm(
        entries: &[ContextEntry],
        llm: &dyn agentos_llm::LLMCore,
        max_input_chars: usize,
    ) -> Result<(String, agentos_llm::InferenceResult), anyhow::Error> {
        let mut text_parts = Vec::new();
        let mut total_chars = 0usize;
        for e in entries {
            let label = match e.role {
                ContextRole::User => "User",
                ContextRole::Assistant => "Assistant",
                ContextRole::ToolResult => "ToolResult",
                ContextRole::System => "System",
            };
            let part = format!("[{}]: {}", label, e.content);
            let part_chars = part.chars().count();
            if total_chars + part_chars > max_input_chars {
                let remaining = max_input_chars.saturating_sub(total_chars);
                if remaining > 20 {
                    text_parts.push(format!(
                        "[{}]: {}...",
                        label,
                        e.content.chars().take(remaining).collect::<String>()
                    ));
                }
                break;
            }
            total_chars += part_chars;
            text_parts.push(part);
        }
        let messages_text = text_parts.join("\n");

        let system_prompt =
            "Summarize the following conversation messages into a concise paragraph. \
            Preserve: key decisions, tool outputs that produced important results, error messages, \
            and any facts the agent discovered. \
            Discard: routine acknowledgments, redundant tool calls, and verbose formatting. \
            Keep the summary under 300 words.";

        let mut ctx = ContextWindow::new(16);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: system_prompt.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: format!("Messages:\n{}", messages_text),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });

        let result = llm.infer(&ctx).await?;
        let summary = result.text.trim().to_string();
        if summary.is_empty() {
            anyhow::bail!("LLM returned empty summary");
        }
        Ok((summary, result))
    }

    /// Push an entry into a task's context window, then apply token budget
    /// enforcement (Spec §11) with 3-phase lock management:
    ///
    /// **Phase 1 (lock held):** Push entry, check token budget, extract
    /// compressible entries if LLM summarization is needed, release lock.
    ///
    /// **Phase 2 (no lock):** Call LLM for summarization (or concat fallback).
    ///
    /// **Phase 3 (lock re-acquired):** Insert summary entry + context notice.
    ///
    /// Returns `Ok(0)` on success.
    pub async fn push_entry(
        &self,
        task_id: &TaskID,
        entry: ContextEntry,
    ) -> Result<usize, AgentOSError> {
        // Phase 1: Push the entry and check if compression is needed.
        let compression_needed: Option<(Vec<ContextEntry>, AgentID, bool)> = {
            let mut tasks = self.tasks.write().await;
            match tasks.get_mut(task_id) {
                Some(tc) => {
                    tc.window.push(entry);

                    if self.token_budget > 0 {
                        let tokens = tc.window.estimated_tokens();
                        let pct = tokens * 100 / self.token_budget;

                        if pct >= 80 {
                            let is_critical = pct >= 95;
                            let compress_count = if is_critical {
                                tc.window.entries.len() / 3
                            } else {
                                tc.window.entries.len() / 4
                            };

                            match self.config.summarization_mode {
                                SummarizationMode::Off => {
                                    let extracted =
                                        tc.window.extract_compressible(compress_count.max(1));
                                    if !extracted.is_empty() {
                                        tc.window.upsert_context_notice(extracted.len());
                                    }
                                    if is_critical {
                                        tc.window.needs_checkpoint = true;
                                    }
                                    None
                                }
                                SummarizationMode::Concat => {
                                    let count = compress_count.max(1);
                                    tc.window.compress_oldest(count);
                                    tc.window.upsert_context_notice(count);
                                    if is_critical {
                                        tc.window.needs_checkpoint = true;
                                    }
                                    tracing::info!(
                                        task_id = %task_id,
                                        tokens,
                                        budget = self.token_budget,
                                        "Context at {}% token budget — compressed (concat)",
                                        pct
                                    );
                                    None
                                }
                                SummarizationMode::Llm => {
                                    let extracted =
                                        tc.window.extract_compressible(compress_count.max(1));
                                    if is_critical {
                                        tc.window.needs_checkpoint = true;
                                    }
                                    if extracted.is_empty() {
                                        None
                                    } else {
                                        tracing::info!(
                                            task_id = %task_id,
                                            tokens,
                                            budget = self.token_budget,
                                            extracted = extracted.len(),
                                            "Context at {}% token budget — attempting LLM summarization",
                                            pct
                                        );
                                        Some((extracted, tc.agent_id, is_critical))
                                    }
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                None => return Err(AgentOSError::TaskNotFound(*task_id)),
            }
            // Write lock released here
        };

        // Phase 2: If entries were extracted for LLM summarization, do it without holding the lock.
        if let Some((extracted, agent_id, _is_critical)) = compression_needed {
            let extracted_count = extracted.len();

            let summary_text = {
                let llm_opt = {
                    let llms = self.active_llms.read().await;
                    llms.get(&agent_id).cloned()
                };

                match llm_opt {
                    Some(llm) => {
                        match Self::summarize_entries_llm(
                            &extracted,
                            llm.as_ref(),
                            self.config.summarization_max_input_chars,
                        )
                        .await
                        {
                            Ok((summary, inference_result)) => {
                                // Record summarization cost against agent budget
                                let _budget_result = self
                                    .cost_tracker
                                    .record_inference_with_cost(
                                        &agent_id,
                                        &inference_result.tokens_used,
                                        llm.provider_name(),
                                        llm.model_name(),
                                        inference_result.cost.as_ref(),
                                    )
                                    .await;
                                tracing::info!(
                                    task_id = %task_id,
                                    entries = extracted_count,
                                    "LLM summarization succeeded"
                                );
                                summary
                            }
                            Err(e) => {
                                tracing::warn!(
                                    task_id = %task_id,
                                    error = %e,
                                    "LLM summarization failed — falling back to concat"
                                );
                                Self::summarize_entries_concat(&extracted)
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            task_id = %task_id,
                            "No LLM adapter available for summarization — falling back to concat"
                        );
                        Self::summarize_entries_concat(&extracted)
                    }
                }
            };

            // Phase 3: Re-acquire lock and insert summary + notice
            {
                let mut tasks = self.tasks.write().await;
                if let Some(tc) = tasks.get_mut(task_id) {
                    tc.window
                        .insert_summary_entry(summary_text, extracted_count);
                    tc.window.upsert_context_notice(extracted_count);
                }
            }
        }

        Ok(0)
    }

    /// Check if the token budget is fully exhausted (100%) for a task.
    pub async fn is_budget_exhausted(&self, task_id: &TaskID) -> bool {
        if self.token_budget == 0 {
            return false;
        }
        let tasks = self.tasks.read().await;
        if let Some(tc) = tasks.get(task_id) {
            let estimated = tc.window.estimated_tokens();
            estimated >= self.token_budget
        } else {
            false
        }
    }

    /// Get the entry count for a task's context window.
    pub async fn entry_count(&self, task_id: &TaskID) -> usize {
        let tasks = self.tasks.read().await;
        tasks
            .get(task_id)
            .map(|tc| tc.window.entries.len())
            .unwrap_or(0)
    }

    /// Returns `true` and clears the `needs_checkpoint` flag if the context
    /// window for `task_id` has flagged a checkpoint. Call this after pushing
    /// entries to decide whether to take a snapshot.
    pub async fn drain_checkpoint_flag(&self, task_id: &TaskID) -> bool {
        let mut tasks = self.tasks.write().await;
        if let Some(tc) = tasks.get_mut(task_id) {
            if tc.window.needs_checkpoint {
                tc.window.needs_checkpoint = false;
                return true;
            }
        }
        false
    }

    /// Get the full context for assembling an LLM prompt.
    pub async fn get_context(&self, task_id: &TaskID) -> Result<ContextWindow, AgentOSError> {
        let tasks = self.tasks.read().await;
        tasks
            .get(task_id)
            .map(|tc| tc.window.clone())
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
        tool_call_id: Option<String>,
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
                    tool_call_id,
                    assistant_tool_calls: None,
                }),
                importance,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::default(),
                category: ContextCategory::History,
                is_summary: false,
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
        let mut tasks = self.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(tc) => {
                tc.window.set_partition(partition);
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
        let mut tasks = self.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(tc) => {
                tc.window = window;
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }

    /// Remove a task's context (on completion/failure).
    pub async fn remove_context(&self, task_id: &TaskID) {
        self.tasks.write().await.remove(task_id);
    }

    /// Increment reference counts for entries whose `tool_call_id` matches any
    /// of the provided IDs. This marks those entries as actively referenced so
    /// they are preserved longer during eviction.
    pub async fn increment_references(
        &self,
        task_id: &TaskID,
        tool_call_ids: &[String],
    ) -> Result<(), AgentOSError> {
        let mut tasks = self.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(tc) => {
                tc.window
                    .increment_references_for_tool_call_ids(tool_call_ids);
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }
}
