use agentos_types::{
    ContentPart, ContextCategory, ContextEntry, ContextPartition, ContextRole, TaskID,
};
use std::sync::Arc;
use std::time::Duration;

const ROLLING_SUMMARY_PREFIX: &str = "[ROLLING TASK SUMMARY]";
const LLM_INPUT_MAX_CHARS: usize = 8_000;

/// Hard upper bound on how long the compactor will block on a single
/// LLM summarization round. Without this guard a rate-limited or hung
/// model can stall the iteration loop on its full inference timeout
/// (often 60s+). On expiry the extractive heuristic still runs so the
/// rolling summary is always produced (review R1 finding #3).
const LLM_SUMMARIZATION_TIMEOUT: Duration = Duration::from_secs(15);

pub struct CompactionOutcome {
    pub compressed_entries: usize,
    pub rolling_summary: String,
    /// Whether the rolling summary came from an LLM round (true) or the
    /// extractive fallback (false).
    pub llm_summarized: bool,
}

pub struct ContextCompactor {
    cadence: usize,
    keep_recent_entries: usize,
    max_summary_chars: usize,
}

impl ContextCompactor {
    pub fn new(cadence: usize, keep_recent_iterations: usize) -> Self {
        Self {
            cadence,
            keep_recent_entries: keep_recent_iterations.max(1) * 4,
            max_summary_chars: 1_800,
        }
    }

    pub fn cadence(&self) -> usize {
        self.cadence
    }

    pub async fn maybe_compact(
        &self,
        kernel: &crate::kernel::Kernel,
        task_id: &TaskID,
        completed_iterations: usize,
    ) -> Result<Option<CompactionOutcome>, agentos_types::AgentOSError> {
        self.maybe_compact_with_llm(kernel, task_id, completed_iterations, None)
            .await
    }

    /// Compact with an optional LLM adapter for semantic summarization. When
    /// `llm` is `Some`, calls the model to generate a coherent summary; on
    /// any LLM failure, falls back to the extractive heuristic without
    /// raising an error.
    pub async fn maybe_compact_with_llm(
        &self,
        kernel: &crate::kernel::Kernel,
        task_id: &TaskID,
        completed_iterations: usize,
        llm: Option<Arc<dyn agentos_llm::LLMCore>>,
    ) -> Result<Option<CompactionOutcome>, agentos_types::AgentOSError> {
        if self.cadence == 0
            || completed_iterations == 0
            || !completed_iterations.is_multiple_of(self.cadence)
        {
            return Ok(None);
        }

        let mut window = kernel.context_manager.get_context(task_id).await?;
        let compressible_count = window
            .entries
            .iter()
            .filter(|entry| Self::is_compactable_entry(entry))
            .count();
        if compressible_count <= self.keep_recent_entries || compressible_count < 6 {
            return Ok(None);
        }

        let existing_summary = Self::take_existing_rolling_summary(&mut window);
        let to_extract = compressible_count.saturating_sub(self.keep_recent_entries);
        let extracted = window.extract_compressible(to_extract);
        if extracted.is_empty() {
            return Ok(None);
        }

        let (fresh_summary, llm_summarized) = match llm {
            Some(adapter) => {
                let llm_call = crate::context::ContextManager::summarize_entries_llm(
                    &extracted,
                    adapter.as_ref(),
                    LLM_INPUT_MAX_CHARS,
                );
                match tokio::time::timeout(LLM_SUMMARIZATION_TIMEOUT, llm_call).await {
                    Ok(Ok((summary, _))) => {
                        let trimmed = if summary.chars().count() > self.max_summary_chars {
                            let mut t: String =
                                summary.chars().take(self.max_summary_chars).collect();
                            t.push_str("\n[...summary truncated]");
                            t
                        } else {
                            summary
                        };
                        (trimmed, true)
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(
                            task_id = %task_id,
                            error = %err,
                            "LLM-based context compaction failed; falling back to extractive heuristic"
                        );
                        (
                            Self::summarize_entries(&extracted, self.max_summary_chars),
                            false,
                        )
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            task_id = %task_id,
                            timeout_secs = LLM_SUMMARIZATION_TIMEOUT.as_secs(),
                            "LLM-based context compaction timed out; falling back to extractive heuristic"
                        );
                        (
                            Self::summarize_entries(&extracted, self.max_summary_chars),
                            false,
                        )
                    }
                }
            }
            None => (
                Self::summarize_entries(&extracted, self.max_summary_chars),
                false,
            ),
        };
        let rolling_summary = Self::merge_summaries(
            existing_summary.as_deref(),
            &fresh_summary,
            self.max_summary_chars,
        );

        let insert_pos = window
            .entries
            .iter()
            .position(|entry| entry.role != ContextRole::System)
            .unwrap_or(window.entries.len());
        window.entries.insert(
            insert_pos,
            ContextEntry {
                role: ContextRole::System,
                parts: vec![ContentPart::Text {
                    text: format!("{ROLLING_SUMMARY_PREFIX}\n{rolling_summary}"),
                }],
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.45,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::Active,
                category: ContextCategory::History,
                is_summary: true,
            },
        );
        window.upsert_context_notice(extracted.len());
        kernel
            .context_manager
            .replace_context(task_id, window)
            .await?;

        Ok(Some(CompactionOutcome {
            compressed_entries: extracted.len(),
            rolling_summary,
            llm_summarized,
        }))
    }

    fn is_compactable_entry(entry: &ContextEntry) -> bool {
        entry.partition == ContextPartition::Active
            && entry.role != ContextRole::System
            && !entry.pinned
            && !entry.is_summary
    }

    fn take_existing_rolling_summary(window: &mut agentos_types::ContextWindow) -> Option<String> {
        let idx = window
            .entries
            .iter()
            .position(|entry| entry.text().starts_with(ROLLING_SUMMARY_PREFIX))?;
        let entry = window.entries.remove(idx);
        entry
            .text()
            .strip_prefix(ROLLING_SUMMARY_PREFIX)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    fn merge_summaries(existing: Option<&str>, fresh: &str, max_chars: usize) -> String {
        let mut merged = String::new();
        if let Some(existing) = existing.filter(|s| !s.trim().is_empty()) {
            merged.push_str(existing.trim());
            merged.push('\n');
        }
        merged.push_str(fresh.trim());
        if merged.chars().count() <= max_chars {
            merged
        } else {
            let mut truncated: String = merged.chars().take(max_chars).collect();
            truncated.push_str("\n[...rolling summary truncated]");
            truncated
        }
    }

    fn summarize_entries(entries: &[ContextEntry], max_chars: usize) -> String {
        let mut lines = Vec::new();
        for entry in entries {
            let prefix = match entry.role {
                ContextRole::User => "User",
                ContextRole::Assistant => "Assistant",
                ContextRole::ToolResult => "Tool",
                ContextRole::System => continue,
            };
            let compact = entry
                .text()
                .split_whitespace()
                .take(32)
                .collect::<Vec<_>>()
                .join(" ");
            if !compact.is_empty() {
                lines.push(format!("- {prefix}: {compact}"));
            }
        }
        let summary = if lines.is_empty() {
            "- Earlier iterations were compacted.".to_string()
        } else {
            lines.join("\n")
        };
        if summary.chars().count() <= max_chars {
            summary
        } else {
            let mut truncated: String = summary.chars().take(max_chars).collect();
            truncated.push_str("\n[...summary truncated]");
            truncated
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: ContextRole, content: &str) -> ContextEntry {
        ContextEntry {
            role,
            parts: vec![ContentPart::Text {
                text: content.to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        }
    }

    #[test]
    fn extractive_summary_lists_each_role_prefix() {
        let entries = vec![
            entry(ContextRole::User, "first user message"),
            entry(ContextRole::Assistant, "first assistant reply"),
            entry(ContextRole::ToolResult, "tool returned ok"),
        ];
        let s = ContextCompactor::summarize_entries(&entries, 1000);
        assert!(s.contains("- User: first user message"));
        assert!(s.contains("- Assistant: first assistant reply"));
        assert!(s.contains("- Tool: tool returned ok"));
    }

    #[test]
    fn extractive_summary_skips_system_entries() {
        let entries = vec![
            entry(ContextRole::System, "system noise"),
            entry(ContextRole::User, "real user message"),
        ];
        let s = ContextCompactor::summarize_entries(&entries, 1000);
        assert!(!s.contains("system noise"));
        assert!(s.contains("real user message"));
    }

    #[test]
    fn extractive_summary_truncates_at_max_chars() {
        let entries = vec![entry(ContextRole::User, &"abc ".repeat(500))];
        let s = ContextCompactor::summarize_entries(&entries, 50);
        assert!(s.chars().count() <= 50 + "\n[...summary truncated]".len());
        assert!(s.ends_with("[...summary truncated]"));
    }

    #[test]
    fn merge_summaries_joins_existing_with_fresh() {
        let merged = ContextCompactor::merge_summaries(Some("OLD CONTENT"), "NEW CONTENT", 1000);
        assert!(merged.contains("OLD CONTENT"));
        assert!(merged.contains("NEW CONTENT"));
        // OLD must precede NEW because the rolling summary is append-only.
        let old_pos = merged.find("OLD").unwrap();
        let new_pos = merged.find("NEW").unwrap();
        assert!(old_pos < new_pos);
    }

    #[test]
    fn merge_summaries_truncates_when_combined_exceeds_max() {
        let merged = ContextCompactor::merge_summaries(Some(&"a".repeat(2000)), "fresh", 500);
        assert!(merged.chars().count() <= 500 + "\n[...rolling summary truncated]".len());
        assert!(merged.ends_with("[...rolling summary truncated]"));
    }

    #[test]
    fn cadence_zero_disables_compaction() {
        let compactor = ContextCompactor::new(0, 2);
        assert_eq!(compactor.cadence(), 0);
    }

    #[test]
    fn keep_recent_iterations_min_one() {
        // 0 iterations to keep makes no sense; constructor must clamp to 1
        // so we always preserve at least 4 raw entries.
        let compactor = ContextCompactor::new(5, 0);
        assert_eq!(compactor.keep_recent_entries, 4);
    }
}
