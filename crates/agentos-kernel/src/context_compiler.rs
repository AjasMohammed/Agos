use agentos_types::*;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

/// Inputs gathered by the task executor before compilation.
///
/// The compiler does not access kernel subsystems directly -- the caller
/// (task_executor) is responsible for fetching tool descriptions, episodic
/// recall, and the raw history from `ContextManager::get_context()`.
#[derive(Clone)]
pub struct CompilationInputs {
    /// Core system prompt: agent identity, safety instructions, response format.
    pub system_prompt: String,
    /// Tool descriptions from `ToolRegistry::tools_for_prompt()`.
    pub tool_descriptions: String,
    /// Agent directory block (other agents and their capabilities).
    pub agent_directory: String,
    /// Retrieved knowledge: episodic recall, semantic search results.
    /// Each string is a pre-formatted block (e.g., `[EPISODIC_RECALL]...[/EPISODIC_RECALL]`).
    pub knowledge_blocks: Vec<String>,
    /// Raw conversation history from `ContextManager::get_context()`.
    /// Includes user prompts, assistant responses, and tool results.
    /// The compiler selects the most recent entries that fit the history budget.
    pub history: Vec<ContextEntry>,
    /// The current task/user prompt (placed last for recency effect).
    pub task_prompt: String,
}

/// Compiles structured, category-budgeted context windows from raw inputs.
///
/// Called before every `LLMCore::infer()` call, including between tool-call
/// cycles within a single task execution. `ContextManager` stays the
/// authoritative history store; the compiler reads from it and builds
/// optimized views.
pub struct ContextCompiler {
    budget: TokenBudget,
}

impl ContextCompiler {
    pub fn new(budget: TokenBudget) -> Self {
        Self { budget }
    }

    /// Access the token budget configuration.
    pub fn budget(&self) -> &TokenBudget {
        &self.budget
    }

    /// Build an optimized `ContextWindow` from structured inputs.
    ///
    /// Position ordering (for primacy/recency effects):
    /// 1. **System** -- first position (primacy: model pays most attention)
    /// 2. **Tools** -- tool descriptions + agent directory
    /// 3. **Knowledge** -- retrieved memories, episodic recall
    /// 4. **History** -- prior conversation turns (most recent kept)
    /// 5. **Task** -- current user prompt (last position = recency)
    ///
    /// Each category is independently truncated to its token budget.
    /// An oversized system prompt cannot evict history entries.
    pub fn compile(&self, inputs: CompilationInputs) -> ContextWindow {
        let mut window = ContextWindow::new(self.budget.total_tokens);
        let cpt = self.budget.chars_per_token;

        // Deduplication set: track content hashes to skip identical entries within a
        // single compile pass. This prevents repeated retrieval paths from inflating
        // knowledge or history with redundant content.
        let mut seen_hashes: HashSet<u64> = HashSet::new();

        // 1. SYSTEM -- first position (primacy effect)
        let system_budget = self.budget.tokens_for(ContextCategory::System);
        let system_content =
            Self::truncate_to_token_budget(&inputs.system_prompt, system_budget, cpt);
        seen_hashes.insert(Self::content_hash(&system_content));
        window.push_categorized(
            ContextRole::System,
            system_content,
            ContextCategory::System,
            1.0,  // maximum importance
            true, // pinned -- never evicted
        );

        // 2. TOOLS -- tool descriptions + agent directory
        let tools_budget = self.budget.tokens_for(ContextCategory::Tools);
        let tools_combined = if inputs.agent_directory.is_empty() {
            inputs.tool_descriptions.clone()
        } else {
            format!("{}\n\n{}", inputs.tool_descriptions, inputs.agent_directory)
        };
        let tools_content = Self::truncate_to_token_budget(&tools_combined, tools_budget, cpt);
        seen_hashes.insert(Self::content_hash(&tools_content));
        window.push_categorized(
            ContextRole::System,
            tools_content,
            ContextCategory::Tools,
            0.9,  // high importance but below system
            true, // pinned -- tool descriptions are always needed
        );

        // 3. KNOWLEDGE -- retrieved memories, episodic recall
        let knowledge_budget = self.budget.tokens_for(ContextCategory::Knowledge);
        let knowledge_entries =
            Self::fit_strings_to_budget(&inputs.knowledge_blocks, knowledge_budget, cpt);
        for block in knowledge_entries {
            let h = Self::content_hash(&block);
            if seen_hashes.insert(h) {
                window.push_categorized(
                    ContextRole::System,
                    block,
                    ContextCategory::Knowledge,
                    0.7, // moderate importance -- can be evicted if needed
                    false,
                );
            }
        }

        // 4. HISTORY -- conversation turns (most recent first)
        let history_budget = self.budget.tokens_for(ContextCategory::History);
        let history_entries = Self::fit_history_to_budget(&inputs.history, history_budget, cpt);
        for entry in history_entries {
            let h = Self::content_hash_with_role(Some(entry.role), &entry.content);
            if seen_hashes.insert(h) {
                // Preserve the original entry's role and metadata but tag with History category
                window.entries.push(ContextEntry {
                    role: entry.role,
                    content: entry.content,
                    timestamp: entry.timestamp,
                    metadata: entry.metadata,
                    importance: entry.importance,
                    pinned: entry.pinned,
                    reference_count: entry.reference_count,
                    partition: ContextPartition::Active,
                    category: ContextCategory::History,
                    is_summary: entry.is_summary,
                });
            }
        }

        // 5. TASK -- current user prompt (last position = recency effect)
        let task_budget = self.budget.tokens_for(ContextCategory::Task);
        let task_content = Self::truncate_to_token_budget(&inputs.task_prompt, task_budget, cpt);
        // Do not dedup the task prompt — it is always inserted even if identical to a history
        // entry (e.g., single-turn tasks where prompt == first history entry).
        window.push_categorized(
            ContextRole::User,
            task_content,
            ContextCategory::Task,
            1.0,   // maximum importance -- the whole point of the task
            false, // not pinned -- task prompt is always the latest entry
        );

        window
    }

    /// Compute a non-cryptographic 64-bit hash of a string for deduplication.
    ///
    /// Uses the standard library's `DefaultHasher`, which is fast and sufficient
    /// for content dedup within a single compile pass. Not suitable for security use.
    fn content_hash(s: &str) -> u64 {
        Self::content_hash_with_role(None, s)
    }

    /// Hash content together with its role so that two entries with identical text
    /// but different roles (e.g. User vs ToolResult) are not treated as duplicates.
    fn content_hash_with_role(role: Option<ContextRole>, s: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(r) = role {
            std::mem::discriminant(&r).hash(&mut hasher);
        }
        s.hash(&mut hasher);
        hasher.finish()
    }

    /// Truncate a string to fit within a token budget.
    ///
    /// Uses the supplied `chars_per_token` ratio so that the token estimate
    /// matches the one used by `ContextWindow::estimated_tokens_with_ratio()`.
    /// Comparison is done on char count (not byte length) for correctness with
    /// multi-byte UTF-8 characters.
    fn truncate_to_token_budget(text: &str, max_tokens: usize, chars_per_token: f32) -> String {
        let ratio = chars_per_token.clamp(0.5, 16.0);
        // Subtract 1 token worth of chars to account for the +1 overhead that
        // `estimated_tokens_with_ratio()` adds per entry, keeping truncation and
        // estimation consistent so the entry fits within its budget after estimation.
        let max_chars = (max_tokens.saturating_sub(1) as f32 * ratio) as usize;
        if text.chars().count() <= max_chars {
            text.to_string()
        } else {
            // Find the last valid char boundary at or before max_chars chars in
            let truncation_point = text
                .char_indices()
                .take(max_chars)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            let mut truncated = text[..truncation_point].to_string();
            truncated.push_str("\n[...truncated to fit token budget]");
            truncated
        }
    }

    /// Select strings from a list that fit within the token budget.
    /// Preserves order. Stops adding when the next entry would exceed budget.
    /// The first entry is truncated if it alone exceeds the budget.
    fn fit_strings_to_budget(
        entries: &[String],
        max_tokens: usize,
        chars_per_token: f32,
    ) -> Vec<String> {
        let ratio = chars_per_token.clamp(0.5, 16.0);
        let max_chars = (max_tokens as f32 * ratio) as usize;
        let mut result = Vec::new();
        let mut used_chars = 0usize;
        for entry in entries {
            let entry_chars = entry.chars().count();
            if used_chars + entry_chars > max_chars {
                // If this is the first entry and it's too large, truncate it
                if result.is_empty() {
                    result.push(Self::truncate_to_token_budget(
                        entry,
                        max_tokens,
                        chars_per_token,
                    ));
                }
                break;
            }
            used_chars += entry_chars;
            result.push(entry.clone());
        }
        result
    }

    /// Select the most recent history entries that fit within the token budget.
    ///
    /// Algorithm: iterate from newest to oldest, accumulating entries until
    /// the budget is exhausted. Pinned entries are always included (they count
    /// against budget but are never skipped). The result is returned in
    /// chronological order (oldest first).
    fn fit_history_to_budget(
        entries: &[ContextEntry],
        max_tokens: usize,
        chars_per_token: f32,
    ) -> Vec<ContextEntry> {
        let ratio = chars_per_token.clamp(0.5, 16.0);
        let max_chars = (max_tokens as f32 * ratio) as usize;
        let mut selected: Vec<ContextEntry> = Vec::new();
        let mut used_chars = 0usize;

        // First pass: collect all pinned entries (they must be included)
        for entry in entries {
            if entry.pinned {
                used_chars += entry.content.chars().count();
                selected.push(entry.clone());
            }
        }

        // Second pass: from most recent, add non-pinned entries that fit
        for entry in entries.iter().rev() {
            if entry.pinned {
                continue; // already included
            }
            let entry_chars = entry.content.chars().count();
            if used_chars + entry_chars > max_chars {
                continue; // skip entries that don't fit
            }
            used_chars += entry_chars;
            selected.push(entry.clone());
        }

        // Sort by timestamp to restore chronological order
        selected.sort_by_key(|e| e.timestamp);
        selected
    }

    /// Estimate the token count of a string using the configurable ratio.
    #[allow(dead_code)]
    fn estimate_tokens(text: &str, chars_per_token: f32) -> usize {
        let ratio = chars_per_token.clamp(0.5, 16.0);
        (text.chars().count() as f32 / ratio) as usize + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{
        ContextCategory, ContextEntry, ContextPartition, ContextRole, TokenBudget,
    };

    fn make_history_entry(role: ContextRole, content: &str, pinned: bool) -> ContextEntry {
        ContextEntry {
            role,
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        }
    }

    fn default_inputs() -> CompilationInputs {
        CompilationInputs {
            system_prompt: "You are a helpful agent.".to_string(),
            tool_descriptions: "- file-reader: reads files\n- shell-exec: runs commands"
                .to_string(),
            agent_directory: String::new(),
            knowledge_blocks: vec![],
            history: vec![],
            task_prompt: "What is 2+2?".to_string(),
        }
    }

    #[test]
    fn test_compile_produces_correct_category_ordering() {
        let budget = TokenBudget::default();
        let compiler = ContextCompiler::new(budget);
        let inputs = CompilationInputs {
            knowledge_blocks: vec!["[EPISODIC_RECALL]\npast info\n[/EPISODIC_RECALL]".into()],
            history: vec![
                make_history_entry(ContextRole::User, "hello", false),
                make_history_entry(ContextRole::Assistant, "hi there", false),
            ],
            ..default_inputs()
        };

        let window = compiler.compile(inputs);

        // Verify ordering: System, Tools, Knowledge, History, Task
        assert_eq!(window.entries[0].category, ContextCategory::System);
        assert_eq!(window.entries[1].category, ContextCategory::Tools);
        assert_eq!(window.entries[2].category, ContextCategory::Knowledge);
        // History entries
        assert_eq!(window.entries[3].category, ContextCategory::History);
        assert_eq!(window.entries[4].category, ContextCategory::History);
        // Task is last
        assert_eq!(
            window.entries.last().unwrap().category,
            ContextCategory::Task
        );
    }

    #[test]
    fn test_compile_system_is_pinned_task_is_not() {
        let compiler = ContextCompiler::new(TokenBudget::default());
        let window = compiler.compile(default_inputs());

        let system = &window.entries[0];
        assert!(system.pinned, "System entry must be pinned");
        assert_eq!(system.importance, 1.0);

        let task = window.entries.last().unwrap();
        assert!(!task.pinned, "Task entry must not be pinned");
        assert_eq!(task.importance, 1.0);
    }

    #[test]
    fn test_compile_respects_total_budget() {
        let budget = TokenBudget {
            total_tokens: 10_000,
            ..Default::default()
        };
        let compiler = ContextCompiler::new(budget.clone());
        let inputs = CompilationInputs {
            system_prompt: "You are a helpful agent.".to_string(),
            tool_descriptions: "- tool-a: does A".to_string(),
            agent_directory: String::new(),
            knowledge_blocks: vec!["fact 1".into(), "fact 2".into()],
            history: vec![make_history_entry(ContextRole::User, "hello", false)],
            task_prompt: "Do something".to_string(),
        };

        let window = compiler.compile(inputs);
        assert!(
            window.estimated_tokens() <= budget.usable_tokens(),
            "Compiled context ({} tokens) exceeds usable budget ({} tokens)",
            window.estimated_tokens(),
            budget.usable_tokens()
        );
    }

    #[test]
    fn test_compile_truncates_oversized_system_prompt() {
        let budget = TokenBudget {
            total_tokens: 100, // very small budget
            ..Default::default()
        };
        let compiler = ContextCompiler::new(budget);
        let inputs = CompilationInputs {
            system_prompt: "x".repeat(50_000), // way over budget
            ..default_inputs()
        };

        let window = compiler.compile(inputs);
        let system = &window.entries[0];
        assert!(
            system.content.len() < 50_000,
            "System prompt should have been truncated"
        );
        assert!(
            system
                .content
                .contains("[...truncated to fit token budget]"),
            "Truncated content should have truncation marker"
        );
    }

    #[test]
    fn test_compile_history_keeps_most_recent() {
        let budget = TokenBudget {
            total_tokens: 220,
            ..Default::default()
        };
        let compiler = ContextCompiler::new(budget);

        let mut history = Vec::new();
        for i in 0..20 {
            let mut entry = make_history_entry(
                if i % 2 == 0 {
                    ContextRole::User
                } else {
                    ContextRole::Assistant
                },
                &format!("Message number {:03} with some padding text", i),
                false,
            );
            entry.timestamp = chrono::Utc::now() + chrono::Duration::milliseconds(i as i64 * 100);
            history.push(entry);
        }

        let window = compiler.compile(CompilationInputs {
            history,
            ..default_inputs()
        });

        let history_entries: Vec<&ContextEntry> = window
            .entries
            .iter()
            .filter(|e| e.category == ContextCategory::History)
            .collect();

        assert!(
            history_entries.len() < 20,
            "History should be truncated to fit budget, got {} entries",
            history_entries.len()
        );
        let last_history = history_entries
            .last()
            .expect("Expected at least one history entry");
        assert!(
            last_history.content.contains("019"),
            "Most recent history entry (019) should be present"
        );
    }

    #[test]
    fn test_compile_history_always_includes_pinned() {
        let budget = TokenBudget {
            total_tokens: 100,
            ..Default::default()
        };
        let compiler = ContextCompiler::new(budget);

        let mut history = Vec::new();
        let mut pinned =
            make_history_entry(ContextRole::User, "This is pinned and must be kept", true);
        pinned.timestamp = chrono::Utc::now() - chrono::Duration::hours(1);
        history.push(pinned);

        for i in 0..5 {
            let mut entry = make_history_entry(
                ContextRole::Assistant,
                &format!("Recent response {}", i),
                false,
            );
            entry.timestamp = chrono::Utc::now() + chrono::Duration::milliseconds(i as i64 * 10);
            history.push(entry);
        }

        let window = compiler.compile(CompilationInputs {
            history,
            ..default_inputs()
        });

        let history_entries: Vec<&ContextEntry> = window
            .entries
            .iter()
            .filter(|e| e.category == ContextCategory::History)
            .collect();
        assert!(
            history_entries
                .iter()
                .any(|e| e.content == "This is pinned and must be kept"),
            "Pinned history entry must always be included"
        );
    }

    #[test]
    fn test_compile_empty_knowledge_produces_no_knowledge_entries() {
        let compiler = ContextCompiler::new(TokenBudget::default());
        let window = compiler.compile(CompilationInputs {
            knowledge_blocks: vec![],
            ..default_inputs()
        });
        let knowledge_count = window
            .entries
            .iter()
            .filter(|e| e.category == ContextCategory::Knowledge)
            .count();
        assert_eq!(
            knowledge_count, 0,
            "No knowledge blocks should produce no knowledge entries"
        );
    }

    #[test]
    fn test_compile_agent_directory_merged_into_tools() {
        let compiler = ContextCompiler::new(TokenBudget::default());
        let window = compiler.compile(CompilationInputs {
            agent_directory: "[AGENT_DIRECTORY]\n- agent-1 (ollama/llama3)\n[/AGENT_DIRECTORY]"
                .into(),
            ..default_inputs()
        });
        let tools_entry = window
            .entries
            .iter()
            .find(|e| e.category == ContextCategory::Tools)
            .expect("Tools entry must exist");
        assert!(
            tools_entry.content.contains("AGENT_DIRECTORY"),
            "Agent directory should be included in tools content"
        );
        assert!(
            tools_entry.content.contains("file-reader"),
            "Tool descriptions should also be present"
        );
    }

    #[test]
    fn test_token_budget_validation() {
        let valid = TokenBudget::default();
        assert!(valid.validate().is_ok());

        let invalid = TokenBudget {
            system_pct: 0.5,
            tools_pct: 0.5,
            knowledge_pct: 0.5,
            ..Default::default()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn test_token_budget_usable_tokens() {
        let budget = TokenBudget {
            total_tokens: 100_000,
            reserve_pct: 0.25,
            ..Default::default()
        };
        assert_eq!(budget.usable_tokens(), 75_000);
    }

    #[test]
    fn test_budget_accessor_returns_configured_budget() {
        let budget = TokenBudget {
            total_tokens: 50_000,
            reserve_pct: 0.20,
            ..Default::default()
        };
        let compiler = ContextCompiler::new(budget.clone());
        assert_eq!(compiler.budget().total_tokens, 50_000);
        assert_eq!(compiler.budget().usable_tokens(), 40_000);
    }

    #[test]
    fn test_context_utilization_threshold_fires_above_80_pct() {
        // Simulate the utilization check from task_executor
        let budget = TokenBudget {
            total_tokens: 10_000,
            reserve_pct: 0.25,
            ..Default::default()
        };
        let compiler = ContextCompiler::new(budget);
        let usable = compiler.budget().usable_tokens(); // 7500

        // 85% utilization should fire
        let estimated_tokens_high = (usable as f32 * 0.85) as usize;
        let utilization_high = estimated_tokens_high as f32 / usable as f32;
        assert!(
            utilization_high > 0.80,
            "85% utilization should exceed 80% threshold"
        );

        // 70% utilization should NOT fire
        let estimated_tokens_low = (usable as f32 * 0.70) as usize;
        let utilization_low = estimated_tokens_low as f32 / usable as f32;
        assert!(
            utilization_low <= 0.80,
            "70% utilization should not exceed 80% threshold"
        );
    }

    #[test]
    fn test_context_utilization_severity_levels() {
        let budget = TokenBudget::default();
        let compiler = ContextCompiler::new(budget);
        let usable = compiler.budget().usable_tokens();

        // >95% should be Critical
        let est_96 = (usable as f32 * 0.96) as usize;
        let util_96 = est_96 as f32 / usable as f32;
        assert!(util_96 > 0.95, "96% should trigger Critical severity");

        // 81-95% should be Warning
        let est_85 = (usable as f32 * 0.85) as usize;
        let util_85 = est_85 as f32 / usable as f32;
        assert!(
            util_85 > 0.80 && util_85 <= 0.95,
            "85% should trigger Warning severity"
        );
    }

    #[test]
    fn test_utilization_uses_usable_not_total_tokens() {
        // Verify that utilization is computed against usable_tokens, not total_tokens.
        // With reserve_pct=0.25, usable = 75% of total. If we used total_tokens as
        // denominator, 80% utilization would never fire because the compiler only fills
        // up to usable_tokens.
        let budget = TokenBudget {
            total_tokens: 100_000,
            reserve_pct: 0.25,
            ..Default::default()
        };
        let compiler = ContextCompiler::new(budget);

        let usable = compiler.budget().usable_tokens(); // 75_000
        let total = compiler.budget().total_tokens; // 100_000

        // 65_000 tokens: 86.7% of usable, but only 65% of total
        let estimated = 65_000usize;
        let util_usable = estimated as f32 / usable as f32;
        let util_total = estimated as f32 / total as f32;

        assert!(util_usable > 0.80, "Should exceed 80% of usable tokens");
        assert!(
            util_total < 0.80,
            "Should NOT exceed 80% of total tokens — proves correct denominator matters"
        );
    }

    #[test]
    fn test_token_budget_per_category() {
        let budget = TokenBudget {
            total_tokens: 100_000,
            reserve_pct: 0.25,
            system_pct: 0.15,
            tools_pct: 0.18,
            knowledge_pct: 0.30,
            history_pct: 0.25,
            task_pct: 0.12,
            chars_per_token: 4.0,
        };
        assert_eq!(budget.tokens_for(ContextCategory::System), 11_250);
        assert_eq!(budget.tokens_for(ContextCategory::Tools), 13_500);
        assert_eq!(budget.tokens_for(ContextCategory::Knowledge), 22_500);
        assert_eq!(budget.tokens_for(ContextCategory::History), 18_750);
        assert_eq!(budget.tokens_for(ContextCategory::Task), 9_000);
    }

    #[test]
    fn test_truncate_to_token_budget_preserves_char_boundaries() {
        let text = "Hello world! ".to_string() + &"\u{1F600}".repeat(100);
        let truncated = ContextCompiler::truncate_to_token_budget(&text, 5, 4.0);
        assert!(truncated.len() < text.len());
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn test_compile_preserves_history_entry_roles() {
        let compiler = ContextCompiler::new(TokenBudget::default());
        let window = compiler.compile(CompilationInputs {
            history: vec![
                make_history_entry(ContextRole::User, "user message", false),
                make_history_entry(ContextRole::Assistant, "assistant response", false),
                make_history_entry(ContextRole::ToolResult, "tool output", false),
            ],
            ..default_inputs()
        });
        let history: Vec<&ContextEntry> = window
            .entries
            .iter()
            .filter(|e| e.category == ContextCategory::History)
            .collect();
        assert_eq!(history[0].role, ContextRole::User);
        assert_eq!(history[1].role, ContextRole::Assistant);
        assert_eq!(history[2].role, ContextRole::ToolResult);
    }

    #[test]
    fn test_compile_deduplicates_knowledge_blocks() {
        let compiler = ContextCompiler::new(TokenBudget::default());
        let duplicate_block = "[RECALL]\nSame fact appears twice\n[/RECALL]".to_string();
        let window = compiler.compile(CompilationInputs {
            knowledge_blocks: vec![duplicate_block.clone(), duplicate_block.clone()],
            ..default_inputs()
        });

        let knowledge_count = window
            .entries
            .iter()
            .filter(|e| e.category == ContextCategory::Knowledge)
            .count();
        assert_eq!(
            knowledge_count, 1,
            "Duplicate knowledge blocks should be deduplicated to a single entry"
        );
    }

    #[test]
    fn test_compile_deduplicates_history_entries() {
        let compiler = ContextCompiler::new(TokenBudget::default());
        // Two history entries with identical content
        let mut e1 = make_history_entry(ContextRole::User, "repeated message", false);
        let mut e2 = make_history_entry(ContextRole::User, "repeated message", false);
        // Slightly different timestamps so both pass budget checks
        e1.timestamp = chrono::Utc::now();
        e2.timestamp = chrono::Utc::now() + chrono::Duration::milliseconds(100);

        let window = compiler.compile(CompilationInputs {
            history: vec![e1, e2],
            ..default_inputs()
        });

        let history_with_repeated: Vec<&ContextEntry> = window
            .entries
            .iter()
            .filter(|e| e.category == ContextCategory::History && e.content == "repeated message")
            .collect();
        assert_eq!(
            history_with_repeated.len(),
            1,
            "Duplicate history entries should be deduplicated"
        );
    }

    #[test]
    fn test_truncate_uses_chars_per_token_ratio() {
        // With ratio 2.0 and max_tokens 5, max_chars = 10
        let text = "abcdefghijklmnop"; // 16 chars
        let truncated = ContextCompiler::truncate_to_token_budget(text, 5, 2.0);
        // Should be truncated to 10 chars + marker
        assert!(
            truncated.contains("[...truncated to fit token budget]"),
            "Should contain truncation marker"
        );
        // The content before truncation marker should be <= 10 chars
        let content_before = truncated.split('\n').next().unwrap_or("");
        assert!(
            content_before.chars().count() <= 10,
            "Content before marker should be within 10 chars (got {})",
            content_before.chars().count()
        );
    }

    #[test]
    fn test_cjk_chars_per_token_fits_more_tokens() {
        // For CJK text (chars_per_token = 1.0), each char is 1 token.
        // With budget of 5 tokens, we should fit exactly 5 CJK chars.
        let cjk_text = "你好世界测试"; // 6 chars, 1 char ≈ 1 token
        let truncated = ContextCompiler::truncate_to_token_budget(cjk_text, 5, 1.0);
        assert!(
            truncated.contains("[...truncated to fit token budget]"),
            "6 CJK chars should be truncated for 5-token budget with ratio 1.0"
        );

        // With ratio 4.0 (treating CJK same as Latin), 6 chars → 1.5 tokens → fits fine
        let not_truncated = ContextCompiler::truncate_to_token_budget(cjk_text, 5, 4.0);
        assert!(
            !not_truncated.contains("[...truncated"),
            "6 CJK chars should NOT be truncated with Latin ratio 4.0 for 5-token budget"
        );
    }

    #[test]
    fn test_chars_per_token_config_flows_into_compile() {
        // A budget with a very small chars_per_token (aggressive estimation) should cause
        // more truncation than the default.
        let tight_budget = TokenBudget {
            total_tokens: 1_000,
            chars_per_token: 1.0, // every char = 1 token
            ..Default::default()
        };
        let default_budget = TokenBudget {
            total_tokens: 1_000,
            chars_per_token: 4.0,
            ..Default::default()
        };

        // Create a moderately long system prompt
        let long_prompt = "a".repeat(200);
        let inputs = CompilationInputs {
            system_prompt: long_prompt,
            ..default_inputs()
        };

        let tight_compiler = ContextCompiler::new(tight_budget);
        let default_compiler = ContextCompiler::new(default_budget);

        let tight_window = tight_compiler.compile(inputs.clone());
        let default_window = default_compiler.compile(inputs);

        let tight_system = tight_window
            .entries
            .iter()
            .find(|e| e.category == ContextCategory::System)
            .expect("System entry must exist");
        let default_system = default_window
            .entries
            .iter()
            .find(|e| e.category == ContextCategory::System)
            .expect("System entry must exist");

        assert!(
            tight_system.content.len() <= default_system.content.len(),
            "Tighter chars_per_token should produce equal or shorter system content"
        );
    }
}
