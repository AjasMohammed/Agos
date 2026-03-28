use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Parsed reference to a scratchpad page — either within the same agent or cross-agent.
///
/// Use `parse_page_ref()` to parse a title string into a `PageRef`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageRef {
    /// A page in the calling agent's own scratchpad.
    SameAgent { title: String },
    /// A page in another agent's scratchpad: `@agent_id/title`.
    CrossAgent { agent_id: String, title: String },
}

/// Parse a page title that may use the `@agent_id/title` syntax for cross-agent references.
///
/// - `"My Notes"` → `PageRef::SameAgent { title: "My Notes" }`
/// - `"@agent-123/My Notes"` → `PageRef::CrossAgent { agent_id: "agent-123", title: "My Notes" }`
pub fn parse_page_ref(title: &str) -> PageRef {
    if let Some(stripped) = title.strip_prefix('@') {
        if let Some((agent_id, page_title)) = stripped.split_once('/') {
            let agent_id = agent_id.trim();
            let page_title = page_title.trim();
            // Defense-in-depth: reject agent_ids with path traversal or control chars
            let agent_valid = !agent_id.is_empty()
                && !agent_id.contains("..")
                && !agent_id.chars().any(char::is_control);
            if agent_valid && !page_title.is_empty() {
                return PageRef::CrossAgent {
                    agent_id: agent_id.to_string(),
                    title: page_title.to_string(),
                };
            }
        }
    }
    PageRef::SameAgent {
        title: title.to_string(),
    }
}

/// Detailed outlink information including cross-agent metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlinkInfo {
    /// The target page title.
    pub target_title: String,
    /// Whether this link targets another agent's scratchpad.
    pub is_cross_agent: bool,
    /// The target agent ID, if this is a cross-agent link.
    pub target_agent_id: Option<String>,
}

/// A single page in an agent's scratchpad.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchPage {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A search result with FTS5 snippet and rank score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub page: ScratchPage,
    pub snippet: String,
    pub rank: f64,
}

/// Lightweight page summary for list operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageSummary {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

/// Bidirectional link information for a single page title.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkInfo {
    /// Pages linking to this page title.
    pub backlinks: Vec<PageSummary>,
    /// Titles linked from this page.
    pub outlinks: Vec<String>,
    /// Outlinks whose target page currently does not exist in the same agent.
    pub unresolved: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_page_ref_same_agent() {
        let pr = parse_page_ref("My Notes");
        assert_eq!(
            pr,
            PageRef::SameAgent {
                title: "My Notes".to_string()
            }
        );
    }

    #[test]
    fn test_parse_page_ref_cross_agent() {
        let pr = parse_page_ref("@agent-123/Research");
        assert_eq!(
            pr,
            PageRef::CrossAgent {
                agent_id: "agent-123".to_string(),
                title: "Research".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_page_ref_cross_agent_with_spaces() {
        let pr = parse_page_ref("@  agent-1 / My Notes ");
        assert_eq!(
            pr,
            PageRef::CrossAgent {
                agent_id: "agent-1".to_string(),
                title: "My Notes".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_page_ref_malformed_no_slash() {
        // @agent without a slash falls back to SameAgent
        let pr = parse_page_ref("@agent");
        assert_eq!(
            pr,
            PageRef::SameAgent {
                title: "@agent".to_string()
            }
        );
    }

    #[test]
    fn test_parse_page_ref_empty_agent() {
        // @/title — empty agent_id falls back to SameAgent
        let pr = parse_page_ref("@/Title");
        assert_eq!(
            pr,
            PageRef::SameAgent {
                title: "@/Title".to_string()
            }
        );
    }

    #[test]
    fn test_parse_page_ref_empty_title() {
        // @agent/ — empty title falls back to SameAgent
        let pr = parse_page_ref("@agent/");
        assert_eq!(
            pr,
            PageRef::SameAgent {
                title: "@agent/".to_string()
            }
        );
    }

    #[test]
    fn test_parse_page_ref_rejects_path_traversal() {
        // Agent ID with ".." is rejected as defense-in-depth
        let pr = parse_page_ref("@../../etc/title");
        assert_eq!(
            pr,
            PageRef::SameAgent {
                title: "@../../etc/title".to_string()
            }
        );
    }

    #[test]
    fn test_parse_page_ref_rejects_control_chars_in_agent() {
        let pr = parse_page_ref("@bad\x00agent/title");
        assert_eq!(
            pr,
            PageRef::SameAgent {
                title: "@bad\x00agent/title".to_string()
            }
        );
    }

    #[test]
    fn test_parse_page_ref_title_with_slashes() {
        // Only the first slash is the separator
        let pr = parse_page_ref("@agent/path/to/page");
        assert_eq!(
            pr,
            PageRef::CrossAgent {
                agent_id: "agent".to_string(),
                title: "path/to/page".to_string(),
            }
        );
    }
}
