use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub scenario: String,
    pub turn: usize,
    pub category: FeedbackCategory,
    pub severity: FeedbackSeverity,
    pub observation: String,
    pub suggestion: Option<String>,
    pub context: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    Usability,
    Correctness,
    Ergonomics,
    Security,
    Performance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSeverity {
    Info,
    Warning,
    Error,
}

pub struct FeedbackCollector {
    entries: Vec<FeedbackEntry>,
}

impl FeedbackCollector {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, entry: FeedbackEntry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[FeedbackEntry] {
        &self.entries
    }

    pub fn into_entries(self) -> Vec<FeedbackEntry> {
        self.entries
    }

    /// Deduplicate feedback entries with similar observations.
    /// Entries within the same category that share >80% word overlap are merged,
    /// incrementing a count in the observation text.
    pub fn deduplicate(&mut self) {
        let mut deduped: Vec<(FeedbackEntry, usize)> = Vec::new();

        for entry in self.entries.drain(..) {
            let mut merged = false;
            for (existing, count) in &mut deduped {
                if existing.category == entry.category
                    && word_overlap(&existing.observation, &entry.observation) > 0.8
                {
                    *count += 1;
                    merged = true;
                    break;
                }
            }
            if !merged {
                deduped.push((entry, 1));
            }
        }

        self.entries = deduped
            .into_iter()
            .map(|(mut e, count)| {
                if count > 1 {
                    e.observation = format!("{} (observed {} times)", e.observation, count);
                }
                e
            })
            .collect();
    }

    /// Compute aggregate statistics over all collected entries.
    #[must_use]
    pub fn stats(&self) -> FeedbackStats {
        let mut by_category: HashMap<FeedbackCategory, usize> = HashMap::new();
        let mut by_severity: HashMap<FeedbackSeverity, usize> = HashMap::new();
        let mut by_scenario: HashMap<String, usize> = HashMap::new();

        for entry in &self.entries {
            *by_category.entry(entry.category).or_insert(0) += 1;
            *by_severity.entry(entry.severity).or_insert(0) += 1;
            *by_scenario.entry(entry.scenario.clone()).or_insert(0) += 1;
        }

        FeedbackStats {
            total_entries: self.entries.len(),
            by_category,
            by_severity,
            by_scenario,
        }
    }
}

/// Aggregate statistics over a set of feedback entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackStats {
    pub total_entries: usize,
    pub by_category: HashMap<FeedbackCategory, usize>,
    pub by_severity: HashMap<FeedbackSeverity, usize>,
    pub by_scenario: HashMap<String, usize>,
}

/// Compute Jaccard similarity between the word sets of two strings.
/// Comparison is case-insensitive so that LLM capitalisation variance
/// does not prevent duplicate feedback entries from being merged.
/// Returns a value in [0.0, 1.0] where 1.0 means identical word sets.
fn word_overlap(a: &str, b: &str) -> f64 {
    let words_a: std::collections::HashSet<String> =
        a.split_whitespace().map(|w| w.to_lowercase()).collect();
    let words_b: std::collections::HashSet<String> =
        b.split_whitespace().map(|w| w.to_lowercase()).collect();
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }
    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();
    intersection as f64 / union as f64
}

impl Default for FeedbackCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal deserialization target for JSON inside a `[FEEDBACK]...[/FEEDBACK]` block.
#[derive(Debug, Deserialize)]
struct FeedbackJson {
    category: FeedbackCategory,
    severity: FeedbackSeverity,
    observation: String,
    suggestion: Option<String>,
    context: Option<String>,
}

/// Parse `[FEEDBACK]...[/FEEDBACK]` blocks from LLM response text.
///
/// Each block must contain a JSON object with the fields: `category`, `severity`,
/// `observation`, and optionally `suggestion` and `context`.
pub fn parse_feedback(text: &str, scenario: &str, turn: usize) -> Vec<FeedbackEntry> {
    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find("[FEEDBACK]") {
        let abs_start = search_from + start + "[FEEDBACK]".len();
        if let Some(end_offset) = text[abs_start..].find("[/FEEDBACK]") {
            let block = text[abs_start..abs_start + end_offset].trim();
            match serde_json::from_str::<FeedbackJson>(block) {
                Ok(entry) => results.push(FeedbackEntry {
                    scenario: scenario.to_string(),
                    turn,
                    category: entry.category,
                    severity: entry.severity,
                    observation: entry.observation,
                    suggestion: entry.suggestion,
                    context: entry.context,
                }),
                Err(e) => {
                    tracing::warn!(block = %block, error = %e, "Skipping malformed FEEDBACK block");
                }
            }
            search_from = abs_start + end_offset + "[/FEEDBACK]".len();
        } else {
            break;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry_json(category: &str, severity: &str, observation: &str) -> String {
        format!(
            r#"{{"category":"{category}","severity":"{severity}","observation":"{observation}"}}"#
        )
    }

    #[test]
    fn test_single_feedback_block() {
        let json = make_entry_json("correctness", "warning", "The agent used wrong tool order");
        let text = format!("[FEEDBACK]{json}[/FEEDBACK]");
        let entries = parse_feedback(&text, "test-scenario", 1);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, FeedbackCategory::Correctness);
        assert_eq!(entries[0].severity, FeedbackSeverity::Warning);
        assert_eq!(entries[0].observation, "The agent used wrong tool order");
        assert_eq!(entries[0].scenario, "test-scenario");
        assert_eq!(entries[0].turn, 1);
    }

    #[test]
    fn test_two_feedback_blocks() {
        let j1 = make_entry_json("usability", "info", "First observation");
        let j2 = make_entry_json("security", "error", "Second observation");
        let text = format!("[FEEDBACK]{j1}[/FEEDBACK] some text [FEEDBACK]{j2}[/FEEDBACK]");
        let entries = parse_feedback(&text, "s", 0);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].category, FeedbackCategory::Usability);
        assert_eq!(entries[1].category, FeedbackCategory::Security);
    }

    #[test]
    fn test_no_feedback_blocks() {
        let entries = parse_feedback("No feedback here at all.", "s", 0);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_malformed_json_skipped() {
        let text = "[FEEDBACK]{not valid json}[/FEEDBACK]";
        let entries = parse_feedback(text, "s", 0);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_unclosed_tag_no_panic() {
        let text =
            "[FEEDBACK]{\"category\":\"usability\",\"severity\":\"info\",\"observation\":\"x\"}";
        let entries = parse_feedback(text, "s", 0);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_whitespace_around_json() {
        let text = "[FEEDBACK]\n  {\"category\":\"usability\",\"severity\":\"info\",\"observation\":\"ok\"}\n[/FEEDBACK]";
        let entries = parse_feedback(text, "s", 0);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].observation, "ok");
    }

    #[test]
    fn test_optional_fields_present() {
        let json = r#"{"category":"ergonomics","severity":"info","observation":"ok","suggestion":"do better","context":"ctx"}"#;
        let text = format!("[FEEDBACK]{json}[/FEEDBACK]");
        let entries = parse_feedback(&text, "s", 2);
        assert_eq!(entries[0].suggestion.as_deref(), Some("do better"));
        assert_eq!(entries[0].context.as_deref(), Some("ctx"));
    }

    // --- word_overlap tests ---

    #[test]
    fn test_word_overlap_identical() {
        assert!((word_overlap("hello world foo", "hello world foo") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_word_overlap_completely_different() {
        assert!((word_overlap("alpha beta gamma", "delta epsilon zeta") - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_word_overlap_partial() {
        let v = word_overlap("hello world foo", "hello world bar");
        assert!(v > 0.0 && v < 1.0);
    }

    #[test]
    fn test_word_overlap_empty_strings() {
        assert!((word_overlap("", "hello world") - 0.0).abs() < 1e-9);
        assert!((word_overlap("hello world", "") - 0.0).abs() < 1e-9);
    }

    // --- deduplicate tests ---

    fn make_entry(category: FeedbackCategory, observation: &str) -> FeedbackEntry {
        FeedbackEntry {
            scenario: "s".to_string(),
            turn: 1,
            category,
            severity: FeedbackSeverity::Info,
            observation: observation.to_string(),
            suggestion: None,
            context: None,
        }
    }

    #[test]
    fn test_deduplicate_identical_same_category_merged() {
        let mut collector = FeedbackCollector::new();
        collector.add(make_entry(
            FeedbackCategory::Usability,
            "Tool docs are unclear",
        ));
        collector.add(make_entry(
            FeedbackCategory::Usability,
            "Tool docs are unclear",
        ));
        collector.deduplicate();
        let entries = collector.entries();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].observation.contains("(observed 2 times)"));
    }

    #[test]
    fn test_deduplicate_different_categories_not_merged() {
        let mut collector = FeedbackCollector::new();
        collector.add(make_entry(
            FeedbackCategory::Usability,
            "Tool docs are unclear",
        ));
        collector.add(make_entry(
            FeedbackCategory::Security,
            "Tool docs are unclear",
        ));
        collector.deduplicate();
        assert_eq!(collector.entries().len(), 2);
    }

    #[test]
    fn test_deduplicate_low_overlap_not_merged() {
        let mut collector = FeedbackCollector::new();
        collector.add(make_entry(
            FeedbackCategory::Usability,
            "alpha beta gamma delta",
        ));
        collector.add(make_entry(
            FeedbackCategory::Usability,
            "zeta eta theta iota",
        ));
        collector.deduplicate();
        assert_eq!(collector.entries().len(), 2);
    }

    // --- stats tests ---

    #[test]
    fn test_stats_counts_by_category_and_severity() {
        let mut collector = FeedbackCollector::new();
        collector.add(FeedbackEntry {
            scenario: "s1".to_string(),
            turn: 1,
            category: FeedbackCategory::Usability,
            severity: FeedbackSeverity::Info,
            observation: "obs1".to_string(),
            suggestion: None,
            context: None,
        });
        collector.add(FeedbackEntry {
            scenario: "s1".to_string(),
            turn: 2,
            category: FeedbackCategory::Security,
            severity: FeedbackSeverity::Warning,
            observation: "obs2".to_string(),
            suggestion: None,
            context: None,
        });
        collector.add(FeedbackEntry {
            scenario: "s2".to_string(),
            turn: 1,
            category: FeedbackCategory::Usability,
            severity: FeedbackSeverity::Error,
            observation: "obs3".to_string(),
            suggestion: None,
            context: None,
        });

        let stats = collector.stats();
        assert_eq!(stats.total_entries, 3);
        assert_eq!(
            stats.by_category.get(&FeedbackCategory::Usability),
            Some(&2)
        );
        assert_eq!(stats.by_category.get(&FeedbackCategory::Security), Some(&1));
        assert_eq!(stats.by_severity.get(&FeedbackSeverity::Info), Some(&1));
        assert_eq!(stats.by_severity.get(&FeedbackSeverity::Warning), Some(&1));
        assert_eq!(stats.by_severity.get(&FeedbackSeverity::Error), Some(&1));
        assert_eq!(stats.by_scenario.get("s1"), Some(&2));
        assert_eq!(stats.by_scenario.get("s2"), Some(&1));
    }

    #[test]
    fn test_word_overlap_case_insensitive() {
        // Identical modulo capitalisation — should be 1.0
        assert!(
            (word_overlap("Tool docs are unclear", "tool docs are unclear") - 1.0).abs() < 1e-9
        );
    }

    #[test]
    fn test_word_overlap_both_empty() {
        assert!((word_overlap("", "") - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_word_overlap_partial_exact_value() {
        // intersection = {"hello", "world"}, union = {"hello", "world", "foo", "bar"} → 2/4 = 0.5
        let v = word_overlap("hello world foo", "hello world bar");
        assert!((v - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_deduplicate_empty_collector() {
        let mut collector = FeedbackCollector::new();
        collector.deduplicate();
        assert!(collector.entries().is_empty());
    }

    #[test]
    fn test_deduplicate_three_identical_entries_merged() {
        let mut collector = FeedbackCollector::new();
        for _ in 0..3 {
            collector.add(make_entry(
                FeedbackCategory::Usability,
                "Tool docs are unclear",
            ));
        }
        collector.deduplicate();
        let entries = collector.entries();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].observation.contains("(observed 3 times)"));
    }
}
