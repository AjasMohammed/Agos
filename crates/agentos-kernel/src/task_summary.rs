use agentos_memory::{EpisodeType, EpisodicEntry};
use agentos_types::AgentTask;
use chrono::Utc;

const TRUNCATION_MARKER: &str = "\n\n*[note truncated]*";

/// Maximum deduplication counter attempts before falling back to a UUID suffix.
const MAX_DEDUP_ATTEMPTS: u32 = 1000;

/// Summary generated from a completed task for auto-writing to the scratchpad.
pub(crate) struct TaskSummary {
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
}

/// Generate a scratchpad note summarizing a completed task.
///
/// The note includes: task metadata, key observations from episodic entries,
/// any errors encountered, and auto-detected wikilinks to existing pages.
pub(crate) fn generate_task_summary(
    task: &AgentTask,
    outcome: &str,
    episodes: &[EpisodicEntry],
    existing_pages: &[String],
    max_bytes: usize,
) -> TaskSummary {
    let mut content = String::new();

    // Header
    content.push_str(&format!(
        "# Task: {}\n\n",
        truncate_title(&task.original_prompt)
    ));
    content.push_str(&format!("**Status:** {}\n", outcome));
    content.push_str(&format!("**Agent:** {}\n", task.agent_id));
    content.push_str(&format!("**Completed:** {}\n\n", Utc::now().to_rfc3339()));

    // Key observations from episodes
    content.push_str("## What Happened\n\n");
    for ep in episodes.iter().filter(|e| {
        matches!(
            e.entry_type,
            EpisodeType::ToolResult | EpisodeType::LLMResponse
        )
    }) {
        if let Some(summary) = &ep.summary {
            content.push_str(&format!("- {}\n", summary));
        }
    }

    // Errors encountered (case-insensitive matching)
    let errors: Vec<_> = episodes
        .iter()
        .filter(|e| {
            let lower = e.content.to_lowercase();
            lower.contains("error") || lower.contains("failed")
        })
        .collect();
    if !errors.is_empty() {
        content.push_str("\n## Errors Encountered\n\n");
        for err in &errors {
            content.push_str(&format!(
                "- {}\n",
                err.summary
                    .as_deref()
                    .unwrap_or_else(|| truncate_content(&err.content, 200))
            ));
        }
    }

    // Auto-link: scan for references to existing page titles and replace with wikilinks.
    // Process longer titles first to avoid partial matches (e.g., "Login System Auth"
    // should be linked before "Login System").
    // Skip very short titles (< 4 chars) to avoid corrupting markdown formatting.
    let mut sorted_titles: Vec<&String> = existing_pages.iter().filter(|t| t.len() >= 4).collect();
    sorted_titles.sort_by_key(|b| std::cmp::Reverse(b.len()));
    for page_title in sorted_titles {
        if content.contains(page_title.as_str()) {
            let wikilink = format!("[[{}]]", page_title);
            // Don't double-link (if the text already has [[Title]])
            if !content.contains(&wikilink) {
                content = content.replace(page_title.as_str(), &wikilink);
            }
        }
    }

    // Truncate to max bytes (reserve space for the marker so total stays within limit)
    let effective_max = max_bytes.saturating_sub(TRUNCATION_MARKER.len());
    if content.len() > effective_max {
        // Find the nearest valid UTF-8 char boundary before truncating
        let mut end = effective_max;
        while !content.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        content.truncate(end);
        content.push_str(TRUNCATION_MARKER);
    }

    let title = make_title(&task.original_prompt);

    TaskSummary {
        title,
        content,
        tags: vec![
            "auto".to_string(),
            "task-summary".to_string(),
            outcome.to_lowercase(),
        ],
    }
}

/// Truncate a task description into a page title (max 80 chars).
fn truncate_title(description: &str) -> String {
    let cleaned: String = description
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let trimmed = cleaned.trim();
    if description.chars().count() > 80 {
        format!("{}...", trimmed)
    } else {
        trimmed.to_string()
    }
}

/// Create the base title for a task summary page.
fn make_title(description: &str) -> String {
    format!("Task: {}", truncate_title(description))
}

/// Produce a deduplicated title by appending a counter suffix when needed.
///
/// If the base title is not taken, returns it as-is. Otherwise appends
/// `(2)`, `(3)`, etc. up to [`MAX_DEDUP_ATTEMPTS`]. Falls back to a
/// UUID suffix if all counters are taken (extremely unlikely).
pub(crate) fn deduplicate_title(base_title: &str, existing_titles: &[String]) -> String {
    if !existing_titles.contains(&base_title.to_string()) {
        return base_title.to_string();
    }
    for counter in 2..=MAX_DEDUP_ATTEMPTS {
        let candidate = format!("{} ({})", base_title, counter);
        if !existing_titles.contains(&candidate) {
            return candidate;
        }
    }
    // Fallback: UUID suffix guarantees uniqueness
    format!("{} ({})", base_title, uuid::Uuid::new_v4())
}

/// Truncate content for display in error summaries.
fn truncate_content(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_memory::{EpisodeType, EpisodicEntry};
    use agentos_types::{AgentID, TaskID, TraceID};
    use chrono::Utc;

    fn make_entry(entry_type: EpisodeType, content: &str, summary: Option<&str>) -> EpisodicEntry {
        EpisodicEntry {
            id: 1,
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            entry_type,
            content: content.to_string(),
            summary: summary.map(|s| s.to_string()),
            metadata: None,
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
        }
    }

    fn make_task(prompt: &str) -> AgentTask {
        use agentos_types::*;
        use std::time::Duration;
        AgentTask {
            id: TaskID::new(),
            state: TaskState::Complete,
            agent_id: AgentID::new(),
            capability_token: CapabilityToken {
                task_id: TaskID::new(),
                agent_id: AgentID::new(),
                allowed_tools: std::collections::BTreeSet::new(),
                allowed_intents: std::collections::BTreeSet::new(),
                permissions: PermissionSet {
                    entries: vec![],
                    deny_entries: vec![],
                },
                issued_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::hours(1),
                signature: vec![],
            },
            assigned_llm: None,
            priority: 5,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            timeout: Duration::from_secs(60),
            original_prompt: prompt.to_string(),
            history: vec![],
            parent_task: None,
            reasoning_hints: None,
            max_iterations: None,
            trigger_source: None,
            autonomous: false,
        }
    }

    #[test]
    fn test_auto_write_content() {
        let task = make_task("Fix the login bug");
        let episodes = vec![
            make_entry(
                EpisodeType::ToolResult,
                "ran file-read",
                Some("Read auth.rs"),
            ),
            make_entry(
                EpisodeType::LLMResponse,
                "analysis complete",
                Some("Identified null check issue"),
            ),
            make_entry(
                EpisodeType::ToolCall,
                "calling file-write",
                Some("Wrote fix"),
            ),
        ];
        let summary = generate_task_summary(&task, "Success", &episodes, &[], 2048);

        assert!(summary.content.contains("**Status:** Success"));
        assert!(summary.content.contains("**Agent:**"));
        assert!(summary.content.contains("**Completed:**"));
        assert!(summary.content.contains("Read auth.rs"));
        assert!(summary.content.contains("Identified null check issue"));
        // ToolCall entries are not included in "What Happened" (only ToolResult and LLMResponse)
        assert!(!summary.content.contains("Wrote fix"));
        // Tags include outcome
        assert!(summary.tags.contains(&"auto".to_string()));
        assert!(summary.tags.contains(&"task-summary".to_string()));
        assert!(summary.tags.contains(&"success".to_string()));
    }

    #[test]
    fn test_auto_write_errors() {
        let task = make_task("Deploy service");
        let episodes = vec![
            make_entry(EpisodeType::ToolResult, "build succeeded", Some("Build OK")),
            make_entry(
                EpisodeType::ToolResult,
                "deploy error: connection refused",
                Some("Deploy failed"),
            ),
            make_entry(EpisodeType::LLMResponse, "analysis", Some("Retrying")),
        ];
        let summary = generate_task_summary(&task, "Failed", &episodes, &[], 2048);

        assert!(summary.content.contains("## Errors Encountered"));
        assert!(summary.content.contains("Deploy failed"));
        assert!(summary.tags.contains(&"failed".to_string()));
    }

    #[test]
    fn test_auto_write_errors_case_insensitive() {
        let task = make_task("Run tests");
        let episodes = vec![
            make_entry(
                EpisodeType::ToolResult,
                "FAILED: test_login",
                Some("Login test FAILED"),
            ),
            make_entry(
                EpisodeType::ToolResult,
                "ERROR in module",
                Some("Module ERROR"),
            ),
        ];
        let summary = generate_task_summary(&task, "Failed", &episodes, &[], 2048);

        assert!(summary.content.contains("## Errors Encountered"));
        assert!(summary.content.contains("Login test FAILED"));
        assert!(summary.content.contains("Module ERROR"));
    }

    #[test]
    fn test_auto_link_detection() {
        let task = make_task("Review Login System");
        let episodes = vec![make_entry(
            EpisodeType::LLMResponse,
            "checked Login System",
            Some("Reviewed Login System code"),
        )];
        let existing = vec!["Login System".to_string()];
        let summary = generate_task_summary(&task, "Success", &episodes, &existing, 2048);

        assert!(summary.content.contains("[[Login System]]"));
    }

    #[test]
    fn test_auto_link_skips_short_titles() {
        let task = make_task("Check API status");
        let episodes = vec![make_entry(
            EpisodeType::LLMResponse,
            "API returned an error",
            Some("API error"),
        )];
        // Short title "API" should NOT be auto-linked (< 4 chars)
        let existing = vec!["API".to_string()];
        let summary = generate_task_summary(&task, "Failed", &episodes, &existing, 2048);

        assert!(!summary.content.contains("[[API]]"));
    }

    #[test]
    fn test_title_dedup() {
        let existing = vec!["Task: Fix login bug".to_string()];
        assert_eq!(
            deduplicate_title("Task: Fix login bug", &existing),
            "Task: Fix login bug (2)"
        );

        let existing2 = vec![
            "Task: Fix login bug".to_string(),
            "Task: Fix login bug (2)".to_string(),
        ];
        assert_eq!(
            deduplicate_title("Task: Fix login bug", &existing2),
            "Task: Fix login bug (3)"
        );
    }

    #[test]
    fn test_title_dedup_no_conflict() {
        let existing: Vec<String> = vec![];
        assert_eq!(
            deduplicate_title("Task: New feature", &existing),
            "Task: New feature"
        );
    }

    #[test]
    fn test_truncate_title_long() {
        let long = "a".repeat(200);
        let title = truncate_title(&long);
        assert!(title.len() <= 84); // 80 chars + "..."
        assert!(title.ends_with("..."));
    }

    #[test]
    fn test_max_bytes_truncation() {
        let task = make_task("Generate report");
        let mut episodes = Vec::new();
        for i in 0..100 {
            episodes.push(make_entry(
                EpisodeType::LLMResponse,
                &format!("response {}", i),
                Some(&format!(
                    "Very long summary line number {} with lots of detail to fill up space quickly",
                    i
                )),
            ));
        }
        let summary = generate_task_summary(&task, "Success", &episodes, &[], 512);

        // Total content (including marker) must stay within max_bytes
        assert!(summary.content.len() <= 512);
        assert!(summary.content.contains("[note truncated]"));
    }

    #[test]
    fn test_truncation_with_multibyte_chars() {
        let task = make_task("Process emoji data");
        // Create episodes with multi-byte content that will push past the limit
        let episodes = vec![make_entry(
            EpisodeType::LLMResponse,
            "processed data",
            Some(&"🎉 Success! 你好世界 ".repeat(50)),
        )];
        // Should not panic on multi-byte boundary
        let summary = generate_task_summary(&task, "Success", &episodes, &[], 256);
        assert!(summary.content.len() <= 256);
    }
}
