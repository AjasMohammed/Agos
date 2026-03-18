use chrono::Utc;
use serde::Serialize;
use std::collections::HashMap;

use crate::feedback::{FeedbackCategory, FeedbackEntry, FeedbackSeverity, FeedbackStats};
use crate::scenarios::{ScenarioOutcome, ScenarioResult};

pub struct ReportGenerator;

/// Serializable container for JSON export.
#[derive(Serialize)]
pub struct TestReportData {
    pub timestamp: String,
    pub provider: String,
    pub model: String,
    pub results: Vec<ScenarioResult>,
    pub feedback: Vec<FeedbackEntry>,
    pub stats: FeedbackStats,
}

impl ReportGenerator {
    /// Generate a complete markdown test report.
    pub fn generate(
        results: &[ScenarioResult],
        feedback: &[FeedbackEntry],
        stats: &FeedbackStats,
        provider: &str,
        model: &str,
    ) -> String {
        let mut md = String::new();
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");

        md.push_str("# AgentOS LLM Agent Test Report\n\n");
        md.push_str(&format!("> Generated: {}  \n", now));
        md.push_str(&format!("> Provider: `{}`  \n", provider));
        md.push_str(&format!("> Model: `{}`  \n\n", model));
        md.push_str("---\n\n");

        md.push_str(&Self::executive_summary(results, stats));
        md.push_str("\n---\n\n");

        if let Some(consensus) = Self::consensus_section(results) {
            md.push_str(&consensus);
            md.push_str("\n---\n\n");
        }

        md.push_str(&Self::scenario_results_table(results));
        md.push_str("\n---\n\n");

        for result in results {
            md.push_str(&Self::scenario_detail(result, feedback));
            md.push_str("\n---\n\n");
        }

        if let Some(section) = Self::feedback_by_category(feedback) {
            md.push_str(&section);
            md.push_str("\n---\n\n");
        }

        md.push_str(&Self::cost_analysis(results));
        md.push_str("\n---\n\n");

        md.push_str(&Self::recommendations(feedback));

        md
    }

    /// Generate a JSON export of the full test run.
    pub fn generate_json(
        results: &[ScenarioResult],
        feedback: &[FeedbackEntry],
        stats: &FeedbackStats,
        provider: &str,
        model: &str,
    ) -> Result<String, serde_json::Error> {
        let data = TestReportData {
            timestamp: Utc::now().to_rfc3339(),
            provider: provider.to_string(),
            model: model.to_string(),
            results: results.to_vec(),
            feedback: feedback.to_vec(),
            stats: stats.clone(),
        };
        serde_json::to_string_pretty(&data)
    }

    fn executive_summary(results: &[ScenarioResult], stats: &FeedbackStats) -> String {
        let total = results.len();
        let complete = results
            .iter()
            .filter(|r| r.outcome == ScenarioOutcome::Complete)
            .count();
        let incomplete = results
            .iter()
            .filter(|r| r.outcome == ScenarioOutcome::Incomplete)
            .count();
        let errored = results
            .iter()
            .filter(|r| r.outcome == ScenarioOutcome::Errored)
            .count();
        let total_tokens: u64 = results.iter().map(|r| r.total_tokens).sum();
        let total_cost: f64 = results.iter().map(|r| r.total_cost_usd).sum();
        let total_duration: u64 = results.iter().map(|r| r.duration_ms).sum();

        let error_count = stats
            .by_severity
            .get(&FeedbackSeverity::Error)
            .copied()
            .unwrap_or(0);
        let warning_count = stats
            .by_severity
            .get(&FeedbackSeverity::Warning)
            .copied()
            .unwrap_or(0);

        format!(
            "## Executive Summary\n\n\
            | Metric | Value |\n\
            |--------|-------|\n\
            | Scenarios Run | {} |\n\
            | Complete | {} |\n\
            | Incomplete | {} |\n\
            | Errored | {} |\n\
            | Total Feedback Entries | {} |\n\
            | Error-severity Feedback | {} |\n\
            | Warning-severity Feedback | {} |\n\
            | Total Tokens Used | {} |\n\
            | Total Cost (USD) | ${:.4} |\n\
            | Total Duration | {:.1}s |\n",
            total,
            complete,
            incomplete,
            errored,
            stats.total_entries,
            error_count,
            warning_count,
            total_tokens,
            total_cost,
            total_duration as f64 / 1000.0,
        )
    }

    fn scenario_results_table(results: &[ScenarioResult]) -> String {
        let mut md = String::from("## Scenario Results\n\n");
        md.push_str(
            "| Scenario | Outcome | Turns | Tool Calls | Feedback | Tokens | Cost | Duration |\n",
        );
        md.push_str(
            "|----------|---------|-------|------------|----------|--------|------|----------|\n",
        );
        for r in results {
            let outcome_label = match r.outcome {
                ScenarioOutcome::Complete => "PASS",
                ScenarioOutcome::Incomplete => "WARN",
                ScenarioOutcome::Errored => "FAIL",
            };
            md.push_str(&format!(
                "| {} | {} | {}/{} | {} | {} | {} | ${:.4} | {:.1}s |\n",
                r.scenario_name,
                outcome_label,
                r.turns_used,
                r.max_turns,
                r.tool_calls_made,
                r.feedback_count,
                r.total_tokens,
                r.total_cost_usd,
                r.duration_ms as f64 / 1000.0,
            ));
        }
        md
    }

    fn scenario_detail(result: &ScenarioResult, feedback: &[FeedbackEntry]) -> String {
        let mut md = format!("## Scenario: {}\n\n", result.scenario_name);
        md.push_str(&format!("**Outcome:** {:?}  \n", result.outcome));
        md.push_str(&format!(
            "**Turns:** {}/{}  \n",
            result.turns_used, result.max_turns
        ));
        md.push_str(&format!("**Tool Calls:** {}  \n\n", result.tool_calls_made));

        if let Some(ref err) = result.error_message {
            md.push_str(&format!("**Error:** `{}`\n\n", err));
        }

        if !result.turn_metrics.is_empty() {
            md.push_str("### Turn Metrics\n\n");
            md.push_str("| Turn | Inference (ms) | Tool (ms) | Tokens | Tool Called |\n");
            md.push_str("|------|----------------|-----------|--------|-------------|\n");
            for tm in &result.turn_metrics {
                md.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    tm.turn,
                    tm.inference_ms,
                    tm.tool_execution_ms
                        .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
                    tm.input_tokens + tm.output_tokens,
                    tm.tool_called.as_deref().unwrap_or("-"),
                ));
            }
            md.push('\n');
        }

        let scenario_feedback: Vec<&FeedbackEntry> = feedback
            .iter()
            .filter(|f| f.scenario == result.scenario_name)
            .collect();

        if !scenario_feedback.is_empty() {
            md.push_str("### Feedback\n\n");
            for f in scenario_feedback {
                let suggestion_str = f
                    .suggestion
                    .as_ref()
                    .map_or(String::new(), |s| format!("  \n  *Suggestion: {}*", s));
                md.push_str(&format!(
                    "- **[{:?}/{:?}]** {}{}  \n",
                    f.severity, f.category, f.observation, suggestion_str,
                ));
            }
        }

        md
    }

    /// Returns `None` when there is no feedback to display, avoiding a bare section header.
    fn feedback_by_category(feedback: &[FeedbackEntry]) -> Option<String> {
        if feedback.is_empty() {
            return None;
        }

        let mut md = String::from("## Feedback by Category\n\n");

        let categories = [
            FeedbackCategory::Usability,
            FeedbackCategory::Correctness,
            FeedbackCategory::Ergonomics,
            FeedbackCategory::Security,
            FeedbackCategory::Performance,
        ];

        for category in &categories {
            let cat_entries: Vec<&FeedbackEntry> = feedback
                .iter()
                .filter(|f| f.category == *category)
                .collect();

            if !cat_entries.is_empty() {
                md.push_str(&format!("### {:?}\n\n", category));
                for f in cat_entries {
                    let suggestion_str = f
                        .suggestion
                        .as_ref()
                        .map_or(String::new(), |s| format!("  \n  *{}*", s));
                    md.push_str(&format!(
                        "- **[{:?}]** ({}): {}{}  \n",
                        f.severity, f.scenario, f.observation, suggestion_str,
                    ));
                }
                md.push('\n');
            }
        }

        Some(md)
    }

    fn cost_analysis(results: &[ScenarioResult]) -> String {
        let mut md = String::from("## Cost Analysis\n\n");
        let total_cost: f64 = results.iter().map(|r| r.total_cost_usd).sum();
        let total_tokens: u64 = results.iter().map(|r| r.total_tokens).sum();

        md.push_str(&format!("**Total cost:** ${:.4}  \n", total_cost));
        md.push_str(&format!("**Total tokens:** {}  \n\n", total_tokens));

        if results.len() > 1 {
            md.push_str("| Scenario | Tokens | Cost |\n");
            md.push_str("|----------|--------|------|\n");
            for r in results {
                md.push_str(&format!(
                    "| {} | {} | ${:.4} |\n",
                    r.scenario_name, r.total_tokens, r.total_cost_usd,
                ));
            }
        }

        md
    }

    /// Returns a multi-run consensus table when the same scenario was run more than once.
    /// Returns `None` when all scenario names are unique (single-run mode).
    fn consensus_section(results: &[ScenarioResult]) -> Option<String> {
        let mut groups: HashMap<&str, Vec<&ScenarioResult>> = HashMap::new();
        for r in results {
            groups.entry(r.scenario_name.as_str()).or_default().push(r);
        }

        if groups.values().all(|v| v.len() == 1) {
            return None;
        }

        let mut md = String::from("## Multi-Run Consensus\n\n");
        md.push_str("| Scenario | Runs | Complete | Consensus |\n");
        md.push_str("|----------|------|----------|-----------|\n");

        let mut names: Vec<&str> = groups.keys().copied().collect();
        names.sort_unstable();

        for name in names {
            let runs = &groups[name];
            let total = runs.len();
            let complete = runs
                .iter()
                .filter(|r| r.outcome == ScenarioOutcome::Complete)
                .count();
            let consensus = if complete == total {
                "PASS"
            } else if complete == 0 {
                "FAIL"
            } else {
                "PARTIAL"
            };
            md.push_str(&format!(
                "| {} | {} | {}/{} | {} |\n",
                name, total, complete, total, consensus
            ));
        }

        Some(md)
    }

    fn recommendations(feedback: &[FeedbackEntry]) -> String {
        let mut md = String::from("## Recommendations\n\n");
        md.push_str(
            "Based on the LLM agent's experience, the following improvements are recommended:\n\n",
        );

        let errors: Vec<&FeedbackEntry> = feedback
            .iter()
            .filter(|f| f.severity == FeedbackSeverity::Error)
            .collect();
        let warnings: Vec<&FeedbackEntry> = feedback
            .iter()
            .filter(|f| f.severity == FeedbackSeverity::Warning)
            .collect();

        if !errors.is_empty() {
            md.push_str("### Critical Issues (Error Severity)\n\n");
            for (i, f) in errors.iter().enumerate() {
                md.push_str(&format!(
                    "{}. **{}** ({:?}): {}\n",
                    i + 1,
                    f.scenario,
                    f.category,
                    f.suggestion.as_deref().unwrap_or(&f.observation),
                ));
            }
            md.push('\n');
        }

        if !warnings.is_empty() {
            md.push_str("### Improvement Opportunities (Warning Severity)\n\n");
            for (i, f) in warnings.iter().enumerate() {
                md.push_str(&format!(
                    "{}. **{}** ({:?}): {}\n",
                    i + 1,
                    f.scenario,
                    f.category,
                    f.suggestion.as_deref().unwrap_or(&f.observation),
                ));
            }
        }

        if errors.is_empty() && warnings.is_empty() {
            md.push_str(
                "No critical issues or warnings found. All scenarios completed successfully.\n",
            );
        }

        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{FeedbackCategory, FeedbackCollector, FeedbackEntry, FeedbackSeverity};
    use crate::scenarios::{ScenarioOutcome, ScenarioResult, TurnMetrics};

    fn make_result(name: &str, outcome: ScenarioOutcome, cost: f64, tokens: u64) -> ScenarioResult {
        ScenarioResult {
            scenario_name: name.to_string(),
            outcome,
            turns_used: 3,
            max_turns: 10,
            tool_calls_made: 2,
            feedback_count: 0,
            total_tokens: tokens,
            total_cost_usd: cost,
            duration_ms: 1500,
            error_message: None,
            turn_metrics: Vec::new(),
        }
    }

    fn make_feedback(
        scenario: &str,
        severity: FeedbackSeverity,
        category: FeedbackCategory,
        observation: &str,
        suggestion: Option<&str>,
    ) -> FeedbackEntry {
        FeedbackEntry {
            scenario: scenario.to_string(),
            turn: 1,
            category,
            severity,
            observation: observation.to_string(),
            suggestion: suggestion.map(|s| s.to_string()),
            context: None,
        }
    }

    #[test]
    fn test_generate_contains_required_sections() {
        let results = vec![make_result(
            "my-scenario",
            ScenarioOutcome::Complete,
            0.001,
            500,
        )];
        let feedback = vec![
            make_feedback(
                "my-scenario",
                FeedbackSeverity::Info,
                FeedbackCategory::Usability,
                "Works well",
                None,
            ),
            make_feedback(
                "my-scenario",
                FeedbackSeverity::Warning,
                FeedbackCategory::Correctness,
                "Minor issue",
                Some("Fix the thing"),
            ),
        ];
        let mut collector = FeedbackCollector::new();
        for f in &feedback {
            collector.add(f.clone());
        }
        let stats = collector.stats();

        let report = ReportGenerator::generate(
            &results,
            &feedback,
            &stats,
            "anthropic",
            "claude-sonnet-4-6",
        );

        assert!(report.contains("Executive Summary"));
        assert!(report.contains("Scenario Results"));
        assert!(report.contains("Recommendations"));
        assert!(report.contains("anthropic"));
        assert!(report.contains("claude-sonnet-4-6"));
    }

    #[test]
    fn test_generate_errored_scenario_includes_error_message() {
        let mut result = make_result("failing-scenario", ScenarioOutcome::Errored, 0.0, 0);
        result.error_message = Some("kernel panic".to_string());
        let feedback = vec![];
        let stats = FeedbackStats {
            total_entries: 0,
            by_category: Default::default(),
            by_severity: Default::default(),
            by_scenario: Default::default(),
        };

        let report = ReportGenerator::generate(&[result], &feedback, &stats, "mock", "mock-model");

        assert!(report.contains("kernel panic"));
    }

    #[test]
    fn test_generate_json_produces_valid_json() {
        let results = vec![make_result("s1", ScenarioOutcome::Complete, 0.005, 1000)];
        let feedback = vec![make_feedback(
            "s1",
            FeedbackSeverity::Info,
            FeedbackCategory::Performance,
            "Fast response",
            None,
        )];
        let mut collector = FeedbackCollector::new();
        for f in &feedback {
            collector.add(f.clone());
        }
        let stats = collector.stats();

        let json = ReportGenerator::generate_json(&results, &feedback, &stats, "openai", "gpt-4o")
            .expect("serialization should succeed");

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("should be valid JSON");
        assert_eq!(parsed["provider"], "openai");
        assert_eq!(parsed["model"], "gpt-4o");
        assert!(parsed["results"].is_array());
        assert!(parsed["feedback"].is_array());
    }

    #[test]
    fn test_cost_analysis_shows_correct_totals() {
        let results = vec![
            make_result("s1", ScenarioOutcome::Complete, 0.001, 100),
            make_result("s2", ScenarioOutcome::Complete, 0.003, 300),
        ];
        let stats = FeedbackStats {
            total_entries: 0,
            by_category: Default::default(),
            by_severity: Default::default(),
            by_scenario: Default::default(),
        };

        let report = ReportGenerator::generate(&results, &[], &stats, "mock", "mock-model");

        // Total cost = 0.001 + 0.003 = 0.0040
        assert!(report.contains("$0.0040"));
        // Total tokens = 400
        assert!(report.contains("400"));
    }

    #[test]
    fn test_recommendations_errors_before_warnings() {
        let feedback = vec![
            make_feedback(
                "s1",
                FeedbackSeverity::Warning,
                FeedbackCategory::Usability,
                "Minor warning",
                Some("Improve docs"),
            ),
            make_feedback(
                "s1",
                FeedbackSeverity::Error,
                FeedbackCategory::Correctness,
                "Critical bug",
                Some("Fix immediately"),
            ),
        ];
        let stats = FeedbackStats {
            total_entries: 2,
            by_category: Default::default(),
            by_severity: Default::default(),
            by_scenario: Default::default(),
        };

        let report = ReportGenerator::generate(&[], &feedback, &stats, "mock", "mock-model");

        let error_pos = report
            .find("Critical Issues")
            .expect("should contain errors section");
        let warning_pos = report
            .find("Improvement Opportunities")
            .expect("should contain warnings section");
        assert!(
            error_pos < warning_pos,
            "Error section should appear before warning section"
        );
    }

    #[test]
    fn test_generate_turn_metrics_included() {
        let mut result = make_result("s1", ScenarioOutcome::Complete, 0.001, 100);
        result.turn_metrics = vec![TurnMetrics {
            turn: 1,
            inference_ms: 250,
            tool_execution_ms: Some(50),
            input_tokens: 80,
            output_tokens: 20,
            cost_usd: 0.001,
            tool_called: Some("file.read".to_string()),
            tool_succeeded: Some(true),
        }];
        let stats = FeedbackStats {
            total_entries: 0,
            by_category: Default::default(),
            by_severity: Default::default(),
            by_scenario: Default::default(),
        };

        let report = ReportGenerator::generate(&[result], &[], &stats, "mock", "mock-model");

        assert!(report.contains("Turn Metrics"));
        assert!(report.contains("file.read"));
        assert!(report.contains("250"));
    }
}
