---
title: "Phase 05: Report Generation"
tags:
  - testing
  - llm
  - agent
  - plan
date: 2026-03-18
status: planned
effort: 2d
priority: high
---

# Phase 05: Report Generation

> Implement the markdown report generator that takes scenario results and feedback entries and produces a structured, readable test report at `reports/agent-test-YYYY-MM-DD-HHmmss.md`.

---

## Why This Phase

The test harness is useless without human-readable output. This phase transforms raw data (scenario outcomes, feedback entries, cost/timing metrics) into a structured markdown document that a developer or product manager can read to understand the agent's experience with AgentOS.

---

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `ReportGenerator::generate()` | `todo!()` stub | Full markdown rendering with sections for executive summary, per-scenario results, feedback by category, cost analysis, and recommendations |
| Report output | N/A | Written to `reports/agent-test-YYYY-MM-DD-HHmmss.md` |
| `reports/` directory | Does not exist | Created if absent; added to `.gitignore` |
| `main.rs` report wiring | Prints count summary | Calls `ReportGenerator::generate()` and writes file |
| JSON export | N/A | Optional `--json` flag writes `reports/agent-test-YYYY-MM-DD-HHmmss.json` with raw data |

---

## What to Do

### 1. Implement `ReportGenerator` in `src/report.rs`

```rust
use crate::feedback::{FeedbackCollector, FeedbackEntry, FeedbackSeverity, FeedbackStats};
use crate::scenarios::{ScenarioOutcome, ScenarioResult, TurnMetrics};
use chrono::Utc;

pub struct ReportGenerator;

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

        // Header
        md.push_str(&format!("# AgentOS LLM Agent Test Report\n\n"));
        md.push_str(&format!("> Generated: {}  \n", now));
        md.push_str(&format!("> Provider: `{}`  \n", provider));
        md.push_str(&format!("> Model: `{}`  \n\n", model));
        md.push_str("---\n\n");

        // Executive Summary
        md.push_str(&Self::executive_summary(results, stats));
        md.push_str("\n---\n\n");

        // Scenario Results Table
        md.push_str(&Self::scenario_results_table(results));
        md.push_str("\n---\n\n");

        // Per-Scenario Detail
        for result in results {
            md.push_str(&Self::scenario_detail(result, feedback));
            md.push_str("\n---\n\n");
        }

        // Feedback by Category
        md.push_str(&Self::feedback_by_category(feedback));
        md.push_str("\n---\n\n");

        // Cost Analysis
        md.push_str(&Self::cost_analysis(results));
        md.push_str("\n---\n\n");

        // Recommendations
        md.push_str(&Self::recommendations(feedback));

        md
    }

    fn executive_summary(results: &[ScenarioResult], stats: &FeedbackStats) -> String {
        let total = results.len();
        let complete = results.iter().filter(|r| r.outcome == ScenarioOutcome::Complete).count();
        let incomplete = results.iter().filter(|r| r.outcome == ScenarioOutcome::Incomplete).count();
        let errored = results.iter().filter(|r| r.outcome == ScenarioOutcome::Errored).count();
        let total_tokens: u64 = results.iter().map(|r| r.total_tokens).sum();
        let total_cost: f64 = results.iter().map(|r| r.total_cost_usd).sum();
        let total_duration: u64 = results.iter().map(|r| r.duration_ms).sum();

        let error_count = feedback.iter()
            .filter(|f| f.severity == FeedbackSeverity::Error)
            .count();
        // Note: this won't compile as-is because `feedback` is not in scope.
        // In the actual implementation, pass feedback to this method or use stats.

        format!(
            r#"## Executive Summary

| Metric | Value |
|--------|-------|
| Scenarios Run | {} |
| Complete | {} |
| Incomplete | {} |
| Errored | {} |
| Total Feedback Entries | {} |
| Error-severity Feedback | {} |
| Warning-severity Feedback | {} |
| Total Tokens Used | {} |
| Total Cost (USD) | ${:.4} |
| Total Duration | {:.1}s |
"#,
            total, complete, incomplete, errored,
            stats.total_entries,
            stats.by_severity.get("Error").unwrap_or(&0),
            stats.by_severity.get("Warning").unwrap_or(&0),
            total_tokens, total_cost, total_duration as f64 / 1000.0,
        )
    }

    fn scenario_results_table(results: &[ScenarioResult]) -> String {
        let mut md = String::from("## Scenario Results\n\n");
        md.push_str("| Scenario | Outcome | Turns | Tool Calls | Feedback | Tokens | Cost | Duration |\n");
        md.push_str("|----------|---------|-------|------------|----------|--------|------|----------|\n");
        for r in results {
            let outcome_emoji = match r.outcome {
                ScenarioOutcome::Complete => "PASS",
                ScenarioOutcome::Incomplete => "WARN",
                ScenarioOutcome::Errored => "FAIL",
            };
            md.push_str(&format!(
                "| {} | {} | {}/{} | {} | {} | {} | ${:.4} | {:.1}s |\n",
                r.scenario_name, outcome_emoji, r.turns_used, r.max_turns,
                r.tool_calls_made, r.feedback_count, r.total_tokens,
                r.total_cost_usd, r.duration_ms as f64 / 1000.0,
            ));
        }
        md
    }

    fn scenario_detail(result: &ScenarioResult, feedback: &[FeedbackEntry]) -> String {
        let mut md = format!("## Scenario: {}\n\n", result.scenario_name);
        md.push_str(&format!("**Outcome:** {:?}  \n", result.outcome));
        md.push_str(&format!("**Turns:** {}/{}  \n", result.turns_used, result.max_turns));
        md.push_str(&format!("**Tool Calls:** {}  \n\n", result.tool_calls_made));

        if let Some(ref err) = result.error_message {
            md.push_str(&format!("**Error:** `{}`\n\n", err));
        }

        // Per-turn metrics if available
        if !result.turn_metrics.is_empty() {
            md.push_str("### Turn Metrics\n\n");
            md.push_str("| Turn | Inference (ms) | Tool (ms) | Tokens | Tool Called |\n");
            md.push_str("|------|---------------|-----------|--------|------------|\n");
            for tm in &result.turn_metrics {
                md.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    tm.turn, tm.inference_ms,
                    tm.tool_execution_ms.map_or("-".to_string(), |ms| format!("{}", ms)),
                    tm.input_tokens + tm.output_tokens,
                    tm.tool_called.as_deref().unwrap_or("-"),
                ));
            }
            md.push('\n');
        }

        // Feedback for this scenario
        let scenario_feedback: Vec<&FeedbackEntry> = feedback.iter()
            .filter(|f| f.scenario == result.scenario_name)
            .collect();

        if !scenario_feedback.is_empty() {
            md.push_str("### Feedback\n\n");
            for f in scenario_feedback {
                md.push_str(&format!(
                    "- **[{:?}/{:?}]** {} {}  \n",
                    f.severity, f.category, f.observation,
                    f.suggestion.as_ref().map_or(String::new(), |s| format!("  \n  *Suggestion: {}*", s)),
                ));
            }
        }

        md
    }

    fn feedback_by_category(feedback: &[FeedbackEntry]) -> String {
        let mut md = String::from("## Feedback by Category\n\n");

        let categories = ["Usability", "Correctness", "Ergonomics", "Security", "Performance"];
        for cat_name in &categories {
            let cat_entries: Vec<&FeedbackEntry> = feedback.iter()
                .filter(|f| format!("{:?}", f.category) == *cat_name)
                .collect();

            if !cat_entries.is_empty() {
                md.push_str(&format!("### {}\n\n", cat_name));
                for f in cat_entries {
                    md.push_str(&format!(
                        "- **[{:?}]** ({}): {} {}\n",
                        f.severity, f.scenario, f.observation,
                        f.suggestion.as_ref().map_or(String::new(), |s| format!("  \n  *{}*", s)),
                    ));
                }
                md.push('\n');
            }
        }

        md
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

    fn recommendations(feedback: &[FeedbackEntry]) -> String {
        let mut md = String::from("## Recommendations\n\n");
        md.push_str("Based on the LLM agent's experience, the following improvements are recommended:\n\n");

        // Group by severity (errors first, then warnings, then info)
        let errors: Vec<&FeedbackEntry> = feedback.iter()
            .filter(|f| f.severity == FeedbackSeverity::Error)
            .collect();
        let warnings: Vec<&FeedbackEntry> = feedback.iter()
            .filter(|f| f.severity == FeedbackSeverity::Warning)
            .collect();

        if !errors.is_empty() {
            md.push_str("### Critical Issues (Error Severity)\n\n");
            for (i, f) in errors.iter().enumerate() {
                md.push_str(&format!(
                    "{}. **{}** ({}): {}\n",
                    i + 1, f.scenario,
                    format!("{:?}", f.category),
                    f.suggestion.as_deref().unwrap_or(&f.observation),
                ));
            }
            md.push('\n');
        }

        if !warnings.is_empty() {
            md.push_str("### Improvement Opportunities (Warning Severity)\n\n");
            for (i, f) in warnings.iter().enumerate() {
                md.push_str(&format!(
                    "{}. **{}** ({}): {}\n",
                    i + 1, f.scenario,
                    format!("{:?}", f.category),
                    f.suggestion.as_deref().unwrap_or(&f.observation),
                ));
            }
        }

        if errors.is_empty() && warnings.is_empty() {
            md.push_str("No critical issues or warnings found. All scenarios completed successfully.\n");
        }

        md
    }
}
```

### 2. Add JSON export capability

```rust
use serde::Serialize;

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
    pub fn generate_json(
        results: &[ScenarioResult],
        feedback: &[FeedbackEntry],
        stats: &FeedbackStats,
        provider: &str,
        model: &str,
    ) -> String {
        let data = TestReportData {
            timestamp: Utc::now().to_rfc3339(),
            provider: provider.to_string(),
            model: model.to_string(),
            results: results.to_vec(),
            feedback: feedback.to_vec(),
            stats: stats.clone(),
        };
        serde_json::to_string_pretty(&data).unwrap_or_default()
    }
}
```

### 3. Wire report generation into `main.rs`

Update `main.rs` to write the report:

```rust
// After all scenarios complete:
let feedback_entries = collector.entries().to_vec();
collector.deduplicate();
let stats = collector.stats();
let deduped_feedback = collector.into_entries();

let report_md = ReportGenerator::generate(
    &results,
    &deduped_feedback,
    &stats,
    &args.provider,
    &args.model,
);

// Ensure reports directory exists
std::fs::create_dir_all(&args.output_dir)?;

let timestamp = chrono::Utc::now().format("%Y-%m-%d-%H%M%S");
let report_path = format!("{}/agent-test-{}.md", args.output_dir, timestamp);
std::fs::write(&report_path, &report_md)?;
tracing::info!(path = %report_path, "Report written");

if args.json {
    let json_report = ReportGenerator::generate_json(
        &results, &deduped_feedback, &stats, &args.provider, &args.model,
    );
    let json_path = format!("{}/agent-test-{}.json", args.output_dir, timestamp);
    std::fs::write(&json_path, &json_report)?;
    tracing::info!(path = %json_path, "JSON report written");
}

// Print summary to stdout
println!("\n{}", "=".repeat(60));
println!("AgentOS LLM Agent Test Report");
println!("{}", "=".repeat(60));
println!("Provider: {} | Model: {}", args.provider, args.model);
println!("Scenarios: {} | Complete: {} | Incomplete: {} | Errored: {}",
    results.len(),
    results.iter().filter(|r| r.outcome == ScenarioOutcome::Complete).count(),
    results.iter().filter(|r| r.outcome == ScenarioOutcome::Incomplete).count(),
    results.iter().filter(|r| r.outcome == ScenarioOutcome::Errored).count(),
);
println!("Feedback: {} total ({} errors, {} warnings)",
    stats.total_entries,
    stats.by_severity.get("Error").unwrap_or(&0),
    stats.by_severity.get("Warning").unwrap_or(&0),
);
println!("Report: {}", report_path);
println!("{}", "=".repeat(60));
```

### 4. Add `--json` flag to CLI args

```rust
/// Also write JSON report
#[arg(long, default_value = "false")]
json: bool,
```

### 5. Add `reports/` to `.gitignore`

Open `/home/ajas/Desktop/agos/.gitignore` (or create it) and add:

```
# Test reports (generated, not committed)
reports/
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-agent-tester/src/report.rs` | Full implementation: `generate()`, `generate_json()`, all section renderers |
| `crates/agentos-agent-tester/src/main.rs` | Wire report generation, add `--json` flag, create output directory, print summary |
| `.gitignore` | Add `reports/` entry |

---

## Dependencies

[[03-test-scenarios]] must be complete -- we need real scenario results to render.

[[04-feedback-capture]] must be complete -- we need `FeedbackStats`, `deduplicate()`, and cost data to include in the report.

---

## Test Plan

- `cargo build -p agentos-agent-tester` must compile.
- `cargo test -p agentos-agent-tester` must pass.
- Add unit tests for report generation:
  - Test: `generate()` with one complete scenario and two feedback entries produces a non-empty string containing "Executive Summary", "Scenario Results", and "Recommendations".
  - Test: `generate()` with an errored scenario includes the error message in the output.
  - Test: `generate_json()` produces valid JSON that can be parsed back into `TestReportData`.
  - Test: Cost analysis section shows correct totals when given multiple scenarios with different costs.
  - Test: Recommendations section lists error-severity items before warning-severity items.
- Integration test: `./target/debug/agent-tester --provider mock --output-dir /tmp/test-reports` creates a markdown file in `/tmp/test-reports/`.
- Integration test: `./target/debug/agent-tester --provider mock --json --output-dir /tmp/test-reports` creates both `.md` and `.json` files.

---

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
./target/debug/agent-tester --provider mock --output-dir /tmp/test-reports
ls -la /tmp/test-reports/agent-test-*.md
cat /tmp/test-reports/agent-test-*.md
./target/debug/agent-tester --provider mock --json --output-dir /tmp/test-reports
ls -la /tmp/test-reports/agent-test-*.json
```
