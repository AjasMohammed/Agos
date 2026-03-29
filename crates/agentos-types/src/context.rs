use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Strategy for handling context window overflow.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OverflowStrategy {
    /// Drop the oldest non-system entries (FIFO). Default behavior.
    #[default]
    FifoEviction,
    /// Summarize the oldest N entries into a single compressed entry before evicting.
    /// The `summary_prefix` is prepended to mark it as a summary.
    Summarize,
    /// Keep system prompt + most recent N entries, drop everything in between.
    SlidingWindow,
    /// Evict lowest-importance, non-pinned, least-referenced entries first.
    SemanticEviction,
}

/// A rolling context window for an agent task.
/// Implemented as a ring buffer with a max entry count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextWindow {
    pub id: ContextID,
    pub entries: Vec<ContextEntry>,
    pub max_entries: usize,
    #[serde(default)]
    pub overflow_strategy: OverflowStrategy,
    /// Set by the kernel when the estimated token count hits 95% of the budget.
    /// The caller should take a checkpoint and flush old entries to Tier 2 memory.
    #[serde(default)]
    pub needs_checkpoint: bool,
}

/// A single entry in the context window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub role: ContextRole,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub metadata: Option<ContextMetadata>,
    /// Importance score from 0.0 (evictable) to 1.0 (critical). Defaults to 0.5.
    #[serde(default = "default_importance")]
    pub importance: f32,
    /// If true, this entry is never evicted (system prompts, safety rules).
    #[serde(default)]
    pub pinned: bool,
    /// Incremented when the agent references this entry in subsequent turns.
    #[serde(default)]
    pub reference_count: u32,
    /// Which partition this entry belongs to. Only `Active` entries are sent to the LLM.
    #[serde(default)]
    pub partition: ContextPartition,
    /// Semantic category for budget allocation. Defaults to `History`
    /// for backward compatibility with existing push-based entries.
    #[serde(default)]
    pub category: ContextCategory,
    /// True for synthetic summary entries created by `OverflowStrategy::Summarize`
    /// or `compress_oldest()`. Summary entries may be evicted even when
    /// `role == System` so that they do not accumulate without bound.
    #[serde(default)]
    pub is_summary: bool,
}

fn default_importance() -> f32 {
    0.5
}

fn default_chars_per_token() -> f32 {
    4.0
}

/// Context partitions allow agents to maintain a scratchpad that isn't sent to the LLM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ContextPartition {
    /// Normal context — included in LLM prompts.
    #[default]
    Active,
    /// Scratchpad — excluded from LLM prompts, used for agent working memory.
    Scratchpad,
}

/// Semantic category of a context entry, used by `ContextCompiler`
/// to allocate token budgets and enforce position ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextCategory {
    /// System prompt, agent identity, standing safety instructions.
    System,
    /// Tool descriptions (from `ToolRegistry::tools_for_prompt()`).
    Tools,
    /// Retrieved memories: episodic recall, semantic search results, RAG content.
    Knowledge,
    /// Conversation history: prior user/assistant/tool-result turns.
    #[default]
    History,
    /// Current task description and user prompt.
    Task,
}

/// Per-category token budget for context compilation.
///
/// Percentages are of *usable* tokens (total minus output reserve).
/// They must sum to <= 1.0. Any remainder is slack for rounding.
///
/// Design decision: system 15%, tools 18%, knowledge 30%, history 25%, task 12%.
/// These sum to 100% of usable tokens; the reserve is taken from total first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Total context window size in tokens (from LLM's `ModelCapabilities`).
    pub total_tokens: usize,
    /// Fraction reserved for output generation (not allocated to any input category).
    /// Default: 0.25 (25% of total reserved for the model's response).
    pub reserve_pct: f32,
    /// Fraction of usable tokens for system prompt + identity + safety rules.
    pub system_pct: f32,
    /// Fraction of usable tokens for tool descriptions.
    pub tools_pct: f32,
    /// Fraction of usable tokens for retrieved knowledge (episodic, semantic, RAG).
    pub knowledge_pct: f32,
    /// Fraction of usable tokens for conversation history.
    pub history_pct: f32,
    /// Fraction of usable tokens for current task/user prompt.
    pub task_pct: f32,
    /// Characters per token ratio used for estimation.
    ///
    /// Default 4.0 is a good approximation for English/Latin text.
    /// For CJK (Chinese/Japanese/Korean) text, use 1.5–2.0 since each
    /// character typically encodes to 1–2 tokens, not 0.25.
    #[serde(default = "default_chars_per_token")]
    pub chars_per_token: f32,
}

impl TokenBudget {
    /// Tokens available for input categories (after reserving output space).
    pub fn usable_tokens(&self) -> usize {
        ((1.0 - self.reserve_pct) * self.total_tokens as f32) as usize
    }

    /// Token allowance for a specific category.
    pub fn tokens_for(&self, category: ContextCategory) -> usize {
        let usable = self.usable_tokens() as f32;
        let pct = match category {
            ContextCategory::System => self.system_pct,
            ContextCategory::Tools => self.tools_pct,
            ContextCategory::Knowledge => self.knowledge_pct,
            ContextCategory::History => self.history_pct,
            ContextCategory::Task => self.task_pct,
        };
        (usable * pct) as usize
    }

    /// Validate that category percentages are non-negative and do not exceed 1.0.
    pub fn validate(&self) -> Result<(), String> {
        for (name, pct) in [
            ("system_pct", self.system_pct),
            ("tools_pct", self.tools_pct),
            ("knowledge_pct", self.knowledge_pct),
            ("history_pct", self.history_pct),
            ("task_pct", self.task_pct),
        ] {
            if pct < 0.0 {
                return Err(format!("{name} is negative ({pct:.4}); must be >= 0.0"));
            }
        }
        let sum = self.system_pct
            + self.tools_pct
            + self.knowledge_pct
            + self.history_pct
            + self.task_pct;
        if sum > 1.001 {
            return Err(format!(
                "Category percentages sum to {:.4}, exceeding 1.0",
                sum
            ));
        }
        if self.reserve_pct < 0.0 || self.reserve_pct > 0.5 {
            return Err(format!(
                "Reserve percentage {:.2} out of range [0.0, 0.5]",
                self.reserve_pct
            ));
        }
        if self.chars_per_token < 0.5 || self.chars_per_token > 16.0 {
            return Err(format!(
                "chars_per_token {:.2} out of range [0.5, 16.0]",
                self.chars_per_token
            ));
        }
        Ok(())
    }
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            total_tokens: 128_000,
            reserve_pct: 0.25,
            system_pct: 0.15,
            tools_pct: 0.18,
            knowledge_pct: 0.30,
            history_pct: 0.25,
            task_pct: 0.12,
            chars_per_token: default_chars_per_token(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextRole {
    System,
    User,
    Assistant,
    ToolResult,
}

/// Optional metadata attached to a context entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMetadata {
    pub tool_name: Option<String>,
    pub tool_id: Option<ToolID>,
    pub intent_id: Option<MessageID>,
    pub tokens_estimated: Option<u32>,
    /// Provider-native tool call ID for tool result entries.
    /// Used by LLM adapters to format tool results in the correct provider protocol
    /// (e.g., OpenAI `tool_call_id`, Anthropic `tool_use_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// For assistant entries that made tool calls: serialized Vec<InferenceToolCall>
    /// (fields: id, tool_name, intent_type, payload). Adapters use this to reconstruct
    /// the provider-native assistant message format so multi-turn conversations remain
    /// valid (OpenAI requires `tool_calls` array; Anthropic requires `tool_use` content
    /// blocks; Gemini requires `functionCall` parts in the preceding model turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_tool_calls: Option<serde_json::Value>,
}

impl ContextWindow {
    pub fn new(max_entries: usize) -> Self {
        Self {
            id: ContextID::new(),
            entries: Vec::new(),
            max_entries,
            overflow_strategy: OverflowStrategy::default(),
            needs_checkpoint: false,
        }
    }

    /// Create a context window with a specific overflow strategy.
    pub fn with_strategy(max_entries: usize, strategy: OverflowStrategy) -> Self {
        Self {
            id: ContextID::new(),
            entries: Vec::new(),
            max_entries,
            overflow_strategy: strategy,
            needs_checkpoint: false,
        }
    }

    /// Compress the oldest non-system, non-pinned entries into a summary.
    /// Removes up to `count` entries and replaces them with a single summary entry.
    pub fn compress_oldest(&mut self, count: usize) {
        let mut summarized_parts = Vec::new();
        let mut removed = 0;
        let mut i = 0;
        while removed < count && i < self.entries.len() {
            let e = &self.entries[i];
            if e.role != ContextRole::System && !e.pinned {
                let label = match e.role {
                    ContextRole::User => "User",
                    ContextRole::Assistant => "Assistant",
                    ContextRole::ToolResult => "ToolResult",
                    ContextRole::System => unreachable!(),
                };
                // Use char-boundary-safe truncation to avoid panics on multi-byte UTF-8
                let snippet = if e.content.chars().count() > 150 {
                    format!("{}...", e.content.chars().take(150).collect::<String>())
                } else {
                    e.content.clone()
                };
                summarized_parts.push(format!("[{label}]: {snippet}"));
                self.entries.remove(i);
                removed += 1;
            } else {
                i += 1;
            }
        }

        if !summarized_parts.is_empty() {
            let insert_pos = self
                .entries
                .iter()
                .position(|e| e.role != ContextRole::System)
                .unwrap_or(self.entries.len());

            self.entries.insert(
                insert_pos,
                ContextEntry {
                    role: ContextRole::System,
                    content: format!(
                        "[TOKEN BUDGET SUMMARY — {} messages compressed]\n{}",
                        summarized_parts.len(),
                        summarized_parts.join("\n")
                    ),
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                    importance: 0.3,
                    pinned: false,
                    reference_count: 0,
                    partition: ContextPartition::Active,
                    category: ContextCategory::History,
                    is_summary: true,
                },
            );
        }
    }

    /// Extract up to `count` non-pinned, non-System, non-summary entries
    /// from oldest first. Returns the removed entries so the caller can
    /// summarize them (e.g., via LLM). Unlike `compress_oldest`, this
    /// skips existing summary entries to avoid re-summarization.
    pub fn extract_compressible(&mut self, count: usize) -> Vec<ContextEntry> {
        let mut extracted = Vec::new();
        let mut i = 0;
        while extracted.len() < count && i < self.entries.len() {
            let e = &self.entries[i];
            if e.role != ContextRole::System && !e.pinned && !e.is_summary {
                extracted.push(self.entries.remove(i));
            } else {
                i += 1;
            }
        }
        extracted
    }

    /// Insert a summary entry at the correct position (after System entries,
    /// before non-system content). Used by the kernel's LLM summarization
    /// path to insert the generated summary.
    pub fn insert_summary_entry(&mut self, content: String, compressed_count: usize) {
        let insert_pos = self
            .entries
            .iter()
            .position(|e| e.role != ContextRole::System)
            .unwrap_or(self.entries.len());

        self.entries.insert(
            insert_pos,
            ContextEntry {
                role: ContextRole::System,
                content: format!(
                    "[SUMMARY — {} messages compressed]\n{}",
                    compressed_count, content
                ),
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.3,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::Active,
                category: ContextCategory::History,
                is_summary: true,
            },
        );
    }

    /// Sentinel prefix used to identify context-loss notice entries.
    const CONTEXT_NOTICE_PREFIX: &'static str = "[CONTEXT NOTE]";

    /// Insert or update a context-loss notice telling the agent that entries
    /// were compressed and how to recover details via episodic memory.
    /// The count is cumulative: if a notice already exists, the new count is
    /// added to the previous total.
    pub fn upsert_context_notice(&mut self, additional_compressed: usize) {
        // If an existing notice exists, extract its count and remove it so we can
        // re-insert with the cumulative total.
        let cumulative = if let Some(idx) = self
            .entries
            .iter()
            .position(|e| e.content.starts_with(Self::CONTEXT_NOTICE_PREFIX))
        {
            let existing = &self.entries[idx].content;
            let prev_count: usize = existing
                .strip_prefix(Self::CONTEXT_NOTICE_PREFIX)
                .and_then(|s| s.trim_start().split_whitespace().next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            self.entries.remove(idx);
            prev_count + additional_compressed
        } else {
            additional_compressed
        };

        let notice_content = format!(
            "{} {} earlier messages were compressed into a summary. \
             To recall specific details, use memory-read with scope=episodic and your current task ID.",
            Self::CONTEXT_NOTICE_PREFIX,
            cumulative,
        );

        let insert_pos = self
            .entries
            .iter()
            .position(|e| e.role != ContextRole::System || e.is_summary)
            .unwrap_or(self.entries.len());

        self.entries.insert(
            insert_pos,
            ContextEntry {
                role: ContextRole::System,
                content: notice_content,
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.9,
                pinned: true,
                reference_count: 0,
                partition: ContextPartition::Active,
                category: ContextCategory::System,
                is_summary: false,
            },
        );
    }

    /// Increment `reference_count` on entries linked to the given tool call IDs.
    ///
    /// For each ID:
    /// - ToolResult entries with `metadata.tool_call_id == id` are incremented.
    /// - Assistant entries whose `metadata.assistant_tool_calls` JSON array
    ///   contains an object with `"id": id` are incremented.
    ///
    /// This makes actively-referenced entries resist SemanticEviction.
    pub fn increment_references_for_tool_call_ids(&mut self, ids: &[String]) {
        let unique_ids: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        for entry in &mut self.entries {
            let meta = match &entry.metadata {
                Some(m) => m,
                None => continue,
            };

            for id in &unique_ids {
                if entry.role == ContextRole::ToolResult {
                    if let Some(ref tc_id) = meta.tool_call_id {
                        if tc_id == *id {
                            entry.reference_count += 1;
                        }
                    }
                }

                if entry.role == ContextRole::Assistant {
                    if let Some(ref calls_json) = meta.assistant_tool_calls {
                        if let Some(arr) = calls_json.as_array() {
                            if arr
                                .iter()
                                .any(|call| call.get("id").and_then(|v| v.as_str()) == Some(*id))
                            {
                                entry.reference_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Push a new entry. Applies the configured overflow strategy when at capacity.
    pub fn push(&mut self, entry: ContextEntry) {
        if self.entries.len() >= self.max_entries {
            match &self.overflow_strategy {
                OverflowStrategy::FifoEviction => {
                    // Evict oldest non-System entry, or a summary entry (which may be System
                    // role but is evictable because it was synthetically generated).
                    // If all remaining entries are pinned system entries, evict the oldest.
                    if let Some(idx) = self
                        .entries
                        .iter()
                        .position(|e| e.role != ContextRole::System || e.is_summary)
                    {
                        self.entries.remove(idx);
                    } else {
                        self.entries.remove(0);
                    }
                }
                OverflowStrategy::Summarize => {
                    // Collect the oldest evictable entries (up to half) and summarize them.
                    // Evictable = non-System role OR is_summary (synthetic entries with System role
                    // that can be regenerated). Treating is_summary entries as evictable prevents
                    // the strategy from degrading to the safety net when summaries have accumulated.
                    let evictable_count = self
                        .entries
                        .iter()
                        .filter(|e| e.role != ContextRole::System || e.is_summary)
                        .count();
                    let to_summarize = (evictable_count / 2).max(1);

                    let mut summarized_parts = Vec::new();
                    let mut removed = 0;
                    let mut i = 0;
                    while removed < to_summarize && i < self.entries.len() {
                        if self.entries[i].role != ContextRole::System || self.entries[i].is_summary
                        {
                            let e = self.entries.remove(i);
                            let label = if e.is_summary {
                                "Summary"
                            } else {
                                match e.role {
                                    ContextRole::User => "User",
                                    ContextRole::Assistant => "Assistant",
                                    ContextRole::ToolResult => "ToolResult",
                                    ContextRole::System => "System",
                                }
                            };
                            // Use char-boundary-safe truncation to avoid panics on multi-byte UTF-8
                            let snippet = if e.content.chars().count() > 200 {
                                format!("{}...", e.content.chars().take(200).collect::<String>())
                            } else {
                                e.content
                            };
                            summarized_parts.push(format!("[{label}]: {snippet}"));
                            removed += 1;
                        } else {
                            i += 1;
                        }
                    }

                    // Insert the new summary immediately after all non-evictable System entries
                    // (i.e., after real System entries but before any remaining summaries or
                    // non-system entries). This ensures chronological ordering of summaries.
                    let insert_pos = self
                        .entries
                        .iter()
                        .position(|e| e.role != ContextRole::System || e.is_summary)
                        .unwrap_or(self.entries.len());

                    self.entries.insert(
                        insert_pos,
                        ContextEntry {
                            role: ContextRole::System,
                            content: format!(
                                "[CONTEXT SUMMARY - {} earlier messages condensed]\n{}",
                                summarized_parts.len(),
                                summarized_parts.join("\n")
                            ),
                            timestamp: chrono::Utc::now(),
                            metadata: None,
                            importance: 0.3,
                            pinned: false,
                            reference_count: 0,
                            partition: ContextPartition::default(),
                            category: ContextCategory::History,
                            is_summary: true,
                        },
                    );
                }
                OverflowStrategy::SlidingWindow => {
                    // Keep system entries + most recent entries, drop the middle
                    let system_entries: Vec<ContextEntry> = self
                        .entries
                        .iter()
                        .filter(|e| e.role == ContextRole::System)
                        .cloned()
                        .collect();
                    let keep_recent = self.max_entries.saturating_sub(system_entries.len() + 1);
                    let non_system: Vec<ContextEntry> = self
                        .entries
                        .iter()
                        .filter(|e| e.role != ContextRole::System)
                        .cloned()
                        .collect();
                    let recent_start = non_system.len().saturating_sub(keep_recent);

                    self.entries = system_entries;
                    self.entries
                        .extend(non_system[recent_start..].iter().cloned());
                }
                OverflowStrategy::SemanticEviction => {
                    self.evict_by_semantic_score();
                }
            }
        }
        // Safety net: The overflow strategy may not have freed space (e.g., Summarize
        // replaces 1 entry with 1 summary, net 0). Guards against any strategy that
        // fails to free a slot.
        //
        // Eviction priority:
        //   1. Oldest non-pinned non-system non-summary entry — real conversation turns
        //   2. Oldest summary — synthetic, can survive loss better than real turns
        //   3. Oldest any entry — last resort
        //
        // Summaries are deprioritised here so that freshly-created summaries are not
        // immediately discarded by this net before the new entry is pushed.
        if self.entries.len() >= self.max_entries {
            if let Some(idx) = self
                .entries
                .iter()
                .position(|e| e.role != ContextRole::System && !e.is_summary)
            {
                self.entries.remove(idx);
            } else if let Some(idx) = self.entries.iter().position(|e| e.is_summary) {
                self.entries.remove(idx);
            } else {
                self.entries.remove(0);
            }
        }
        self.entries.push(entry);
    }

    /// Push an entry with an explicit category tag.
    /// Used by `ContextCompiler` to build structured context windows.
    /// Routes through `push()` so overflow/eviction is applied correctly.
    pub fn push_categorized(
        &mut self,
        role: ContextRole,
        content: String,
        category: ContextCategory,
        importance: f32,
        pinned: bool,
    ) {
        self.push(ContextEntry {
            role,
            content,
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance,
            pinned,
            reference_count: 0,
            partition: ContextPartition::Active,
            category,
            is_summary: false,
        });
    }

    /// Semantic eviction: compute a composite score per entry and evict the lowest.
    ///
    /// Score = importance * 0.4 + recency * 0.3 + reference_weight * 0.3
    /// Pinned entries are never evicted.
    fn evict_by_semantic_score(&mut self) {
        if self.entries.is_empty() {
            return;
        }

        let now = chrono::Utc::now();
        let oldest = self
            .entries
            .iter()
            .map(|e| e.timestamp)
            .min()
            .unwrap_or(now);
        let time_range = now.signed_duration_since(oldest).num_seconds().max(1) as f32;

        let max_refs = self
            .entries
            .iter()
            .map(|e| e.reference_count)
            .max()
            .unwrap_or(1)
            .max(1) as f32;

        let mut worst_idx = None;
        let mut worst_score = f32::MAX;

        for (idx, entry) in self.entries.iter().enumerate() {
            // Never evict pinned entries
            if entry.pinned {
                continue;
            }

            let recency = now.signed_duration_since(entry.timestamp).num_seconds() as f32;
            let recency_score = 1.0 - (recency / time_range); // 1.0 = newest, 0.0 = oldest

            let ref_score = entry.reference_count as f32 / max_refs;

            let composite = entry.importance * 0.4 + recency_score * 0.3 + ref_score * 0.3;

            if composite < worst_score {
                worst_score = composite;
                worst_idx = Some(idx);
            }
        }

        if let Some(idx) = worst_idx {
            self.entries.remove(idx);
        } else {
            // All entries pinned — evict oldest as fallback
            self.entries.remove(0);
        }
    }

    /// Get all entries as a slice (includes all partitions).
    pub fn as_entries(&self) -> &[ContextEntry] {
        &self.entries
    }

    /// Get only active partition entries (for assembling LLM prompts).
    /// Scratchpad entries are excluded.
    pub fn active_entries(&self) -> Vec<&ContextEntry> {
        self.entries
            .iter()
            .filter(|e| e.partition == ContextPartition::Active)
            .collect()
    }

    /// Move entries between partitions.
    pub fn set_partition(&mut self, partition: ContextPartition) {
        // Set the partition for the most recent non-system entry
        if let Some(entry) = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.role != ContextRole::System)
        {
            entry.partition = partition;
        }
    }

    /// Clear all non-system entries.
    pub fn clear_history(&mut self) {
        self.entries.retain(|e| e.role == ContextRole::System);
    }

    /// Clear all non-pinned, non-system entries (used by rollback).
    pub fn clear_unpinned(&mut self) {
        self.entries
            .retain(|e| e.role == ContextRole::System || e.pinned);
    }

    /// Estimate total token count for all active entries using the default 4 chars ≈ 1 token
    /// heuristic. Uses Unicode scalar count (not byte length) so multi-byte UTF-8 chars are
    /// not over-counted.
    ///
    /// For CJK-heavy content, call `estimated_tokens_with_ratio(1.5)` instead.
    pub fn estimated_tokens(&self) -> usize {
        self.estimated_tokens_with_ratio(4.0)
    }

    /// Estimate total token count for all active entries using a configurable ratio.
    ///
    /// `chars_per_token` is clamped to `[0.5, 16.0]` to avoid division-by-zero and
    /// nonsensical values. The typical range:
    /// - 4.0 — English/Latin text (default)
    /// - 1.5–2.0 — Chinese/Japanese/Korean text
    pub fn estimated_tokens_with_ratio(&self, chars_per_token: f32) -> usize {
        let ratio = chars_per_token.clamp(0.5, 16.0);
        self.entries
            .iter()
            .filter(|e| e.partition == ContextPartition::Active)
            .map(|e| (e.content.chars().count() as f32 / ratio) as usize + 1)
            .sum()
    }

    /// Token usage per category for all active entries, using the given ratio.
    pub fn tokens_per_category(&self, chars_per_token: f32) -> HashMap<ContextCategory, usize> {
        let ratio = chars_per_token.clamp(0.5, 16.0);
        let mut map: HashMap<ContextCategory, usize> = HashMap::new();
        for entry in &self.entries {
            if entry.partition == ContextPartition::Active {
                let tokens = (entry.content.chars().count() as f32 / ratio) as usize + 1;
                *map.entry(entry.category).or_default() += tokens;
            }
        }
        map
    }

    /// Estimated tokens remaining for a specific category given the supplied budget.
    ///
    /// Returns `0` if the category is already over budget.
    pub fn estimated_tokens_remaining(
        &self,
        category: ContextCategory,
        budget: &TokenBudget,
    ) -> usize {
        let allocated = budget.tokens_for(category);
        let used = self
            .tokens_per_category(budget.chars_per_token)
            .get(&category)
            .copied()
            .unwrap_or(0);
        allocated.saturating_sub(used)
    }

    /// Remaining token budget for every category as a map.
    ///
    /// Useful for agents that want to know which categories still have headroom
    /// before the next compile pass.
    pub fn remaining_budget_summary(
        &self,
        budget: &TokenBudget,
    ) -> HashMap<ContextCategory, usize> {
        let usage = self.tokens_per_category(budget.chars_per_token);
        [
            ContextCategory::System,
            ContextCategory::Tools,
            ContextCategory::Knowledge,
            ContextCategory::History,
            ContextCategory::Task,
        ]
        .iter()
        .map(|&cat| {
            let allocated = budget.tokens_for(cat);
            let used = usage.get(&cat).copied().unwrap_or(0);
            (cat, allocated.saturating_sub(used))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(role: ContextRole, content: &str) -> ContextEntry {
        ContextEntry {
            role,
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: default_importance(),
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::default(),
            is_summary: false,
        }
    }

    fn make_entry_with_importance(
        role: ContextRole,
        content: &str,
        importance: f32,
        pinned: bool,
    ) -> ContextEntry {
        ContextEntry {
            role,
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance,
            pinned,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::default(),
            is_summary: false,
        }
    }

    #[test]
    fn test_context_window_push_and_evict() {
        let mut ctx = ContextWindow::new(3);
        ctx.push(make_entry(ContextRole::System, "You are an agent."));
        ctx.push(make_entry(ContextRole::User, "Hello"));
        ctx.push(make_entry(ContextRole::Assistant, "Hi!"));
        // At capacity — next push should evict oldest non-system entry ("Hello")
        ctx.push(make_entry(ContextRole::User, "Next message"));
        assert_eq!(ctx.entries.len(), 3);
        assert_eq!(ctx.entries[0].content, "You are an agent."); // system preserved
        assert_eq!(ctx.entries[1].content, "Hi!"); // second non-system kept
        assert_eq!(ctx.entries[2].content, "Next message"); // newest pushed
    }

    #[test]
    fn test_sliding_window_keeps_recent() {
        let mut ctx = ContextWindow::with_strategy(4, OverflowStrategy::SlidingWindow);
        ctx.push(make_entry(ContextRole::System, "System"));
        ctx.push(make_entry(ContextRole::User, "Msg1"));
        ctx.push(make_entry(ContextRole::Assistant, "Resp1"));
        ctx.push(make_entry(ContextRole::User, "Msg2"));

        // At capacity (4) — push should drop the middle, keep system + recent
        ctx.push(make_entry(ContextRole::Assistant, "Resp2"));

        assert!(ctx.entries.len() <= 4);
        assert_eq!(ctx.entries[0].content, "System"); // system preserved
        assert_eq!(ctx.entries.last().unwrap().content, "Resp2"); // newest
                                                                  // Middle old entries should be dropped
        assert!(!ctx.entries.iter().any(|e| e.content == "Msg1"));
    }

    #[test]
    fn test_summarize_condenses_old_entries() {
        let mut ctx = ContextWindow::with_strategy(4, OverflowStrategy::Summarize);
        ctx.push(make_entry(ContextRole::System, "System"));
        ctx.push(make_entry(ContextRole::User, "Msg1"));
        ctx.push(make_entry(ContextRole::Assistant, "Resp1"));
        ctx.push(make_entry(ContextRole::User, "Msg2"));

        // At capacity — push should summarize oldest non-system entries
        ctx.push(make_entry(ContextRole::Assistant, "Resp2"));

        // Should have a summary entry somewhere
        let has_summary = ctx
            .entries
            .iter()
            .any(|e| e.content.contains("CONTEXT SUMMARY"));
        assert!(has_summary, "Expected a summary entry after overflow");
        assert_eq!(ctx.entries.last().unwrap().content, "Resp2");
    }

    #[test]
    fn test_with_strategy_constructor() {
        let ctx = ContextWindow::with_strategy(10, OverflowStrategy::SlidingWindow);
        assert_eq!(ctx.overflow_strategy, OverflowStrategy::SlidingWindow);
        assert_eq!(ctx.max_entries, 10);
    }

    #[test]
    fn test_semantic_eviction_preserves_pinned() {
        let mut ctx = ContextWindow::with_strategy(3, OverflowStrategy::SemanticEviction);

        // Pinned system prompt
        ctx.push(make_entry_with_importance(
            ContextRole::System,
            "System prompt",
            1.0,
            true,
        ));
        // Pinned user prompt
        ctx.push(make_entry_with_importance(
            ContextRole::User,
            "Important task",
            0.95,
            true,
        ));
        // Low importance tool result
        ctx.push(make_entry_with_importance(
            ContextRole::ToolResult,
            "Old result",
            0.2,
            false,
        ));

        // This push should evict "Old result" (lowest importance, not pinned)
        ctx.push(make_entry_with_importance(
            ContextRole::Assistant,
            "New response",
            0.5,
            false,
        ));

        assert_eq!(ctx.entries.len(), 3);
        assert!(ctx.entries.iter().any(|e| e.content == "System prompt"));
        assert!(ctx.entries.iter().any(|e| e.content == "Important task"));
        assert!(ctx.entries.iter().any(|e| e.content == "New response"));
        assert!(!ctx.entries.iter().any(|e| e.content == "Old result"));
    }

    #[test]
    fn test_semantic_eviction_prefers_low_importance() {
        let mut ctx = ContextWindow::with_strategy(3, OverflowStrategy::SemanticEviction);

        ctx.push(make_entry_with_importance(
            ContextRole::System,
            "System",
            1.0,
            true,
        ));
        ctx.push(make_entry_with_importance(
            ContextRole::ToolResult,
            "High importance result",
            0.9,
            false,
        ));
        ctx.push(make_entry_with_importance(
            ContextRole::ToolResult,
            "Low importance result",
            0.1,
            false,
        ));

        ctx.push(make_entry_with_importance(
            ContextRole::Assistant,
            "Response",
            0.5,
            false,
        ));

        assert_eq!(ctx.entries.len(), 3);
        // Low importance should be evicted
        assert!(!ctx
            .entries
            .iter()
            .any(|e| e.content == "Low importance result"));
        assert!(ctx
            .entries
            .iter()
            .any(|e| e.content == "High importance result"));
    }

    #[test]
    fn test_summary_entries_are_evictable_by_fifo() {
        // FIFO should evict summary entries (role=System, is_summary=true) before
        // accumulating them without bound.
        let mut ctx = ContextWindow::new(3);
        ctx.push(make_entry(ContextRole::System, "Real system prompt"));

        // Manually push a summary entry (simulating what Summarize strategy creates)
        ctx.entries.push(ContextEntry {
            role: ContextRole::System,
            content: "[CONTEXT SUMMARY - 2 earlier messages condensed]\nUser: hi\nAssistant: hello"
                .to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.3,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: true,
        });

        ctx.push(make_entry(ContextRole::User, "New message"));

        // At capacity — FIFO push should evict the summary (is_summary=true), not the real system prompt
        ctx.push(make_entry(ContextRole::Assistant, "Response"));

        assert_eq!(ctx.entries.len(), 3);
        // Real system prompt must be preserved
        assert!(ctx
            .entries
            .iter()
            .any(|e| e.content == "Real system prompt"));
        // Summary should have been evicted
        assert!(!ctx.entries.iter().any(|e| e.is_summary));
    }

    #[test]
    fn test_estimated_tokens_with_ratio() {
        let mut ctx = ContextWindow::new(10);
        // 8 ASCII chars → with ratio 4.0: 8/4 + 1 = 3 tokens
        ctx.push(make_entry(ContextRole::User, "abcdefgh"));

        let tokens_4 = ctx.estimated_tokens_with_ratio(4.0);
        assert_eq!(tokens_4, 3, "8 chars / 4.0 + 1 = 3 tokens");

        // With ratio 2.0: 8/2 + 1 = 5 tokens
        let tokens_2 = ctx.estimated_tokens_with_ratio(2.0);
        assert_eq!(tokens_2, 5, "8 chars / 2.0 + 1 = 5 tokens");

        // estimated_tokens() defaults to 4.0
        assert_eq!(ctx.estimated_tokens(), tokens_4);
    }

    #[test]
    fn test_tokens_per_category() {
        let mut ctx = ContextWindow::new(10);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "abcdefgh".to_string(), // 8 chars
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
            content: "abcdefghijkl".to_string(), // 12 chars
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });

        let usage = ctx.tokens_per_category(4.0);
        // System: 8/4 + 1 = 3 tokens
        assert_eq!(usage.get(&ContextCategory::System).copied().unwrap_or(0), 3);
        // Task: 12/4 + 1 = 4 tokens
        assert_eq!(usage.get(&ContextCategory::Task).copied().unwrap_or(0), 4);
        // History: not present
        assert_eq!(
            usage.get(&ContextCategory::History).copied().unwrap_or(0),
            0
        );
    }

    #[test]
    fn test_estimated_tokens_remaining() {
        let budget = TokenBudget {
            total_tokens: 1000,
            reserve_pct: 0.0, // no reserve, usable = 1000
            system_pct: 0.10, // 100 tokens for system
            tools_pct: 0.0,
            knowledge_pct: 0.0,
            history_pct: 0.90,
            task_pct: 0.0,
            chars_per_token: 4.0,
        };

        let mut ctx = ContextWindow::new(100);
        // Push a system entry: 8 chars → 8/4 + 1 = 3 tokens
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "abcdefgh".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });

        let remaining = ctx.estimated_tokens_remaining(ContextCategory::System, &budget);
        // Budget for system: 100 tokens, used: 3, remaining: 97
        assert_eq!(remaining, 97);

        let history_remaining = ctx.estimated_tokens_remaining(ContextCategory::History, &budget);
        // Budget for history: 900 tokens, used: 0, remaining: 900
        assert_eq!(history_remaining, 900);
    }

    #[test]
    fn test_remaining_budget_summary_all_categories_present() {
        let ctx = ContextWindow::new(10);
        let budget = TokenBudget::default();
        let summary = ctx.remaining_budget_summary(&budget);

        // All 5 categories should be present
        assert!(summary.contains_key(&ContextCategory::System));
        assert!(summary.contains_key(&ContextCategory::Tools));
        assert!(summary.contains_key(&ContextCategory::Knowledge));
        assert!(summary.contains_key(&ContextCategory::History));
        assert!(summary.contains_key(&ContextCategory::Task));

        // Empty context → remaining == allocated for each category
        for cat in [
            ContextCategory::System,
            ContextCategory::Tools,
            ContextCategory::Knowledge,
            ContextCategory::History,
            ContextCategory::Task,
        ] {
            assert_eq!(
                summary[&cat],
                budget.tokens_for(cat),
                "Empty context should report full remaining budget for {:?}",
                cat
            );
        }
    }

    #[test]
    fn test_extract_compressible_returns_oldest_non_pinned() {
        let mut window = ContextWindow::new(100);
        window.push(ContextEntry {
            role: ContextRole::System,
            content: "system prompt".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });
        window.push(ContextEntry {
            role: ContextRole::User,
            content: "pinned task".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.95,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });
        for i in 0..3 {
            window.push(ContextEntry {
                role: ContextRole::Assistant,
                content: format!("response {}", i),
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.5,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::Active,
                category: ContextCategory::History,
                is_summary: false,
            });
        }
        assert_eq!(window.entries.len(), 5);
        let extracted = window.extract_compressible(2);
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted[0].content, "response 0");
        assert_eq!(extracted[1].content, "response 1");
        assert_eq!(window.entries.len(), 3);
        assert_eq!(window.entries[2].content, "response 2");
    }

    #[test]
    fn test_extract_compressible_skips_system_and_pinned() {
        let mut window = ContextWindow::new(100);
        window.push(ContextEntry {
            role: ContextRole::System,
            content: "sys".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });
        window.push(ContextEntry {
            role: ContextRole::User,
            content: "pinned".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });
        let extracted = window.extract_compressible(5);
        assert!(extracted.is_empty());
        assert_eq!(window.entries.len(), 2);
    }

    #[test]
    fn test_insert_summary_entry_positions_after_system() {
        let mut window = ContextWindow::new(100);
        window.push(ContextEntry {
            role: ContextRole::System,
            content: "system prompt".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });
        window.push(ContextEntry {
            role: ContextRole::User,
            content: "user msg".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });

        window.insert_summary_entry("LLM-generated summary of 3 messages".to_string(), 3);

        assert_eq!(window.entries.len(), 3);
        let summary = &window.entries[1];
        assert!(summary.is_summary);
        assert_eq!(summary.role, ContextRole::System);
        assert_eq!(summary.category, ContextCategory::History);
        assert!((summary.importance - 0.3).abs() < f32::EPSILON);
        assert!(!summary.pinned);
        assert!(summary.content.contains("LLM-generated summary"));
        assert!(summary.content.contains("3 messages"));
    }

    #[test]
    fn test_insert_summary_entry_is_evictable() {
        let mut window = ContextWindow::new(100);
        window.push(ContextEntry {
            role: ContextRole::System,
            content: "system".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });

        window.insert_summary_entry("Summary text".to_string(), 2);

        let summary = &window.entries[1];
        assert!(!summary.pinned, "Summary should not be pinned");
        assert!(summary.is_summary, "Should be marked as summary");
        assert_eq!(summary.role, ContextRole::System);
        // Verify it's evictable by checking importance is low
        assert!((summary.importance - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn test_upsert_context_notice_inserts_when_absent() {
        let mut window = ContextWindow::new(100);
        window.push(ContextEntry {
            role: ContextRole::System,
            content: "system prompt".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });

        window.upsert_context_notice(5);

        assert_eq!(window.entries.len(), 2);
        let notice = &window.entries[1];
        assert!(notice.content.starts_with("[CONTEXT NOTE]"));
        assert!(notice.content.contains("5"));
        assert!(notice.content.contains("memory-read"));
        assert!(notice.pinned);
        assert!((notice.importance - 0.9).abs() < f32::EPSILON);
        assert_eq!(notice.category, ContextCategory::System);
        assert!(!notice.is_summary);
    }

    #[test]
    fn test_upsert_context_notice_updates_existing() {
        let mut window = ContextWindow::new(100);
        window.push(ContextEntry {
            role: ContextRole::System,
            content: "system prompt".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });

        window.upsert_context_notice(3);
        window.upsert_context_notice(8);

        // Should still be 2 entries (system + notice), not 3
        assert_eq!(window.entries.len(), 2);
        let notice = &window.entries[1];
        // Cumulative: 3 + 8 = 11
        assert!(
            notice.content.contains("11"),
            "Should show cumulative count 11, got: {}",
            notice.content
        );
    }

    #[test]
    fn test_increment_references_for_tool_call_ids() {
        let mut window = ContextWindow::new(100);
        window.push(ContextEntry {
            role: ContextRole::Assistant,
            content: "I'll read that file.".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: Some(ContextMetadata {
                tool_name: None,
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: None,
                assistant_tool_calls: Some(serde_json::json!([
                    {"id": "call_123", "tool_name": "file-reader", "intent_type": "read", "payload": {}}
                ])),
            }),
            importance: 0.4,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        window.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "file contents here".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: Some(ContextMetadata {
                tool_name: Some("file-reader".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("call_123".to_string()),
                assistant_tool_calls: None,
            }),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        window.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "other result".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: Some(ContextMetadata {
                tool_name: Some("shell-exec".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("call_999".to_string()),
                assistant_tool_calls: None,
            }),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });

        window.increment_references_for_tool_call_ids(&["call_123".to_string()]);

        assert_eq!(
            window.entries[0].reference_count, 1,
            "Assistant entry should be incremented"
        );
        assert_eq!(
            window.entries[1].reference_count, 1,
            "Matching ToolResult should be incremented"
        );
        assert_eq!(
            window.entries[2].reference_count, 0,
            "Unrelated entry should stay at 0"
        );
    }

    #[test]
    fn test_increment_references_makes_entry_survive_eviction() {
        let mut window = ContextWindow::with_strategy(4, OverflowStrategy::SemanticEviction);
        window.push(ContextEntry {
            role: ContextRole::System,
            content: "sys".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });
        window.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "referenced result".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::minutes(10),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 2,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        window.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "unreferenced result".to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::minutes(5),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        assert_eq!(window.entries.len(), 3);
        window.push(ContextEntry {
            role: ContextRole::Assistant,
            content: "filler".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.4,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        window.push(ContextEntry {
            role: ContextRole::User,
            content: "new entry".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        let remaining_contents: Vec<&str> =
            window.entries.iter().map(|e| e.content.as_str()).collect();
        assert!(
            remaining_contents.contains(&"referenced result"),
            "Referenced entry should survive eviction. Remaining: {:?}",
            remaining_contents
        );
    }

    #[test]
    fn test_token_budget_chars_per_token_validation() {
        let valid = TokenBudget {
            chars_per_token: 2.0,
            ..Default::default()
        };
        assert!(valid.validate().is_ok());

        let too_low = TokenBudget {
            chars_per_token: 0.1,
            ..Default::default()
        };
        assert!(too_low.validate().is_err());

        let too_high = TokenBudget {
            chars_per_token: 20.0,
            ..Default::default()
        };
        assert!(too_high.validate().is_err());
    }

    #[test]
    fn test_extract_then_insert_summary_round_trip() {
        let mut window = ContextWindow::new(100);
        window.push(ContextEntry {
            role: ContextRole::System,
            content: "system".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });
        for i in 0..5 {
            window.push(ContextEntry {
                role: if i % 2 == 0 {
                    ContextRole::User
                } else {
                    ContextRole::Assistant
                },
                content: format!("message {}", i),
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.5,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::Active,
                category: ContextCategory::History,
                is_summary: false,
            });
        }

        assert_eq!(window.entries.len(), 6);

        // Extract 3 oldest compressible
        let extracted = window.extract_compressible(3);
        assert_eq!(extracted.len(), 3);
        assert_eq!(window.entries.len(), 3); // system + 2 remaining

        // Insert a summary
        window.insert_summary_entry("Summary of messages 0-2".to_string(), 3);
        assert_eq!(window.entries.len(), 4); // system + summary + 2 remaining

        // Insert notice
        window.upsert_context_notice(3);
        assert_eq!(window.entries.len(), 5); // system + notice + summary + 2 remaining

        // Verify ordering: system, notice, summary, msg3, msg4
        assert_eq!(window.entries[0].role, ContextRole::System);
        assert!(!window.entries[0].is_summary);
        assert!(window.entries[1].content.starts_with("[CONTEXT NOTE]"));
        assert!(window.entries[2].is_summary);
        assert_eq!(window.entries[3].content, "message 3");
        assert_eq!(window.entries[4].content, "message 4");
    }
}
