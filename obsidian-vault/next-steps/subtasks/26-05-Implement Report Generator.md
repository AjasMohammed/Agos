---
title: Implement Report Generator
tags:
  - testing
  - llm
  - agent
  - next-steps
date: 2026-03-18
status: complete
effort: 2d
priority: high
---

# Implement Report Generator

> Implement the markdown and JSON report generator that produces a structured test report from scenario results and feedback entries.

---

## Why This Subtask

The report is the deliverable of the entire test harness. Without it, all the data collected during testing is ephemeral. The report provides a permanent, readable artifact that developers can review to understand the agent's experience.

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `ReportGenerator::generate()` | `todo!()` | Full markdown rendering: executive summary, scenario results table, per-scenario detail with turn metrics, feedback by category, cost analysis, recommendations |
| JSON export | N/A | `generate_json()` produces structured JSON with all data |
| `reports/` directory | Does not exist | Created at runtime; added to `.gitignore` |
| `main.rs` | Prints count summary | Writes markdown report, optionally JSON, prints file path |

## What to Do

1. Open `crates/agentos-agent-tester/src/report.rs` and implement `ReportGenerator`:

   - `generate(results, feedback, stats, provider, model) -> String`:
     - **Header**: title, timestamp, provider/model
     - **Executive Summary**: table with scenario counts (complete/incomplete/errored), total feedback by severity, total tokens, total cost, total duration
     - **Scenario Results Table**: one row per scenario with outcome, turns, tool calls, feedback count, tokens, cost, duration
     - **Per-Scenario Detail**: for each scenario, show outcome, turns, error message if any, turn metrics table (turn, inference ms, tool ms, tokens, tool called), and feedback entries for that scenario
     - **Feedback by Category**: group all feedback by category (Usability, Correctness, Ergonomics, Security, Performance), list observations with severity
     - **Cost Analysis**: total cost, per-scenario breakdown table
     - **Recommendations**: error-severity items first, then warning-severity, with scenario name and suggestion text

   - `generate_json(results, feedback, stats, provider, model) -> String`:
     - Serialize a `TestReportData` struct (timestamp, provider, model, results, feedback, stats) to pretty-printed JSON

2. Update `crates/agentos-agent-tester/src/main.rs`:
   - After all scenarios complete, call `collector.deduplicate()` and `collector.stats()`
   - Call `ReportGenerator::generate()` and write to `{output_dir}/agent-test-{timestamp}.md`
   - If `--json` flag is set, call `generate_json()` and write `.json` file
   - Print a summary table to stdout with scenario counts and report file path
   - Create `output_dir` if it does not exist

3. Add `--json` flag to the `Args` struct in `main.rs`:
   ```rust
   #[arg(long)]
   json: bool,
   ```

4. Add `reports/` to `/home/ajas/Desktop/agos/.gitignore`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-agent-tester/src/report.rs` | Full implementation of `generate()` and `generate_json()` |
| `crates/agentos-agent-tester/src/main.rs` | Wire report writing, add `--json` flag, create output dir |
| `.gitignore` | Add `reports/` |

## Prerequisites

[[26-03-Build Test Scenario Library]] must be complete -- need scenario results to render.
[[26-04-Add Feedback Capture Layer]] must be complete -- need `FeedbackStats`, deduplication, and cost data.

## Test Plan

- `cargo build -p agentos-agent-tester` compiles
- `cargo test -p agentos-agent-tester` passes
- Test: `generate()` with one `ScenarioResult` (outcome=Complete) and two `FeedbackEntry` items produces a string containing "Executive Summary", "Scenario Results", "Recommendations"
- Test: `generate()` with an errored scenario includes the error message in output
- Test: `generate_json()` produces valid JSON parseable by `serde_json::from_str::<TestReportData>()`
- Test: Cost analysis section sums correctly for 3 scenarios with costs 0.01, 0.02, 0.03 -> total 0.06
- Test: Recommendations lists Error items before Warning items
- Integration: `./target/debug/agent-tester --provider mock --output-dir /tmp/agent-tester-test` creates `/tmp/agent-tester-test/agent-test-*.md`
- Integration: With `--json`, both `.md` and `.json` files are created

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
./target/debug/agent-tester --provider mock --output-dir /tmp/agent-tester-test
ls -la /tmp/agent-tester-test/agent-test-*.md
cat /tmp/agent-tester-test/agent-test-*.md
./target/debug/agent-tester --provider mock --json --output-dir /tmp/agent-tester-test
ls -la /tmp/agent-tester-test/agent-test-*.json
```
