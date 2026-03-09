use crate::ids::*;
use serde::{Deserialize, Serialize};

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
}

/// A single entry in the context window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub role: ContextRole,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub metadata: Option<ContextMetadata>,
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
}

impl ContextWindow {
    pub fn new(max_entries: usize) -> Self {
        Self {
            id: ContextID::new(),
            entries: Vec::new(),
            max_entries,
            overflow_strategy: OverflowStrategy::default(),
        }
    }

    /// Create a context window with a specific overflow strategy.
    pub fn with_strategy(max_entries: usize, strategy: OverflowStrategy) -> Self {
        Self {
            id: ContextID::new(),
            entries: Vec::new(),
            max_entries,
            overflow_strategy: strategy,
        }
    }

    /// Push a new entry. Applies the configured overflow strategy when at capacity.
    pub fn push(&mut self, entry: ContextEntry) {
        if self.entries.len() >= self.max_entries {
            match &self.overflow_strategy {
                OverflowStrategy::FifoEviction => {
                    // Evict oldest non-System entry; if all are System, evict the oldest entry
                    if let Some(idx) = self
                        .entries
                        .iter()
                        .position(|e| e.role != ContextRole::System)
                    {
                        self.entries.remove(idx);
                    } else {
                        self.entries.remove(0);
                    }
                }
                OverflowStrategy::Summarize => {
                    // Collect the oldest non-system entries (up to half) and summarize them
                    let non_system_count = self
                        .entries
                        .iter()
                        .filter(|e| e.role != ContextRole::System)
                        .count();
                    let to_summarize = (non_system_count / 2).max(1);

                    let mut summarized_parts = Vec::new();
                    let mut removed = 0;
                    let mut i = 0;
                    while removed < to_summarize && i < self.entries.len() {
                        if self.entries[i].role != ContextRole::System {
                            let e = self.entries.remove(i);
                            summarized_parts.push(format!(
                                "[{}]: {}",
                                match e.role {
                                    ContextRole::User => "User",
                                    ContextRole::Assistant => "Assistant",
                                    ContextRole::ToolResult => "ToolResult",
                                    ContextRole::System => unreachable!(),
                                },
                                if e.content.len() > 200 {
                                    format!("{}...", &e.content[..200])
                                } else {
                                    e.content
                                }
                            ));
                            removed += 1;
                        } else {
                            i += 1;
                        }
                    }

                    // Insert a summary entry after system entries
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
                                "[CONTEXT SUMMARY - {} earlier messages condensed]\n{}",
                                summarized_parts.len(),
                                summarized_parts.join("\n")
                            ),
                            timestamp: chrono::Utc::now(),
                            metadata: None,
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
            }
        }
        self.entries.push(entry);
    }

    /// Get all entries as a slice (for assembling LLM prompts).
    pub fn as_entries(&self) -> &[ContextEntry] {
        &self.entries
    }

    /// Clear all non-system entries.
    pub fn clear_history(&mut self) {
        self.entries.retain(|e| e.role == ContextRole::System);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_window_push_and_evict() {
        let mut ctx = ContextWindow::new(3);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "You are an agent.".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        ctx.push(ContextEntry {
            role: ContextRole::Assistant,
            content: "Hi!".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        // At capacity — next push should evict oldest non-system entry ("Hello")
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Next message".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        assert_eq!(ctx.entries.len(), 3);
        assert_eq!(ctx.entries[0].content, "You are an agent."); // system preserved
        assert_eq!(ctx.entries[1].content, "Hi!"); // second non-system kept
        assert_eq!(ctx.entries[2].content, "Next message"); // newest pushed
    }

    fn make_entry(role: ContextRole, content: &str) -> ContextEntry {
        ContextEntry {
            role,
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        }
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
}
