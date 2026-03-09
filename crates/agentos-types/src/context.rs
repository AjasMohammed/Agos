use crate::ids::*;
use serde::{Deserialize, Serialize};

/// A rolling context window for an agent task.
/// Implemented as a ring buffer with a max entry count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextWindow {
    pub id: ContextID,
    pub entries: Vec<ContextEntry>,
    pub max_entries: usize,
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
        }
    }

    /// Push a new entry. If at capacity, evict the oldest non-system entry.
    pub fn push(&mut self, entry: ContextEntry) {
        if self.entries.len() >= self.max_entries {
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
}
