---
title: Create Agent Tester Crate
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

# Create Agent Tester Crate

> Set up the `crates/agentos-agent-tester/` crate with Cargo.toml, CLI parsing, core types for feedback and scenarios, and the feedback block parser.

---

## Why This Subtask

This is the foundation for the entire LLM agent testing system. Every other subtask depends on the crate existing, compiling, and having the core types defined.

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `crates/agentos-agent-tester/` | Does not exist | New crate with binary `agent-tester` |
| Workspace members | 15 crates | 16 crates |
| Feedback types | N/A | `FeedbackEntry`, `FeedbackCategory`, `FeedbackSeverity`, `FeedbackCollector`, `parse_feedback()` |
| Scenario types | N/A | `TestScenario`, `ScenarioResult`, `ScenarioOutcome` |
| CLI | N/A | `clap`-based with `--provider`, `--model`, `--api-key`, `--scenarios`, `--output-dir`, `--max-turns`, `--runs`, `--json` flags |

## What to Do

1. Create directory `crates/agentos-agent-tester/src/`

2. Create `crates/agentos-agent-tester/Cargo.toml` with dependencies on `agentos-types`, `agentos-kernel`, `agentos-llm`, `agentos-bus`, `agentos-tools`, `agentos-audit`, `agentos-vault`, `agentos-capability`, plus `tokio`, `clap`, `serde`, `serde_json`, `chrono`, `tracing`, `tracing-subscriber`, `tempfile`, `toml`, `secrecy`, `anyhow`. Define `[[bin]] name = "agent-tester"`.

3. Open `/home/ajas/Desktop/agos/Cargo.toml` and add `"crates/agentos-agent-tester"` to the `members` array (currently 15 entries, this becomes the 16th).

4. Create `src/lib.rs` declaring modules: `feedback`, `harness`, `scenarios`, `report`. Re-export the key types.

5. Create `src/feedback.rs` with:
   - `FeedbackEntry` struct (scenario, turn, category, severity, observation, suggestion, context)
   - `FeedbackCategory` enum (Usability, Correctness, Ergonomics, Security, Performance) with Serialize/Deserialize
   - `FeedbackSeverity` enum (Info, Warning, Error) with Serialize/Deserialize and Ord
   - `FeedbackCollector` with `new()`, `add()`, `entries()`, `into_entries()`
   - `parse_feedback(text: &str, scenario: &str, turn: usize) -> Vec<FeedbackEntry>` -- parser for `[FEEDBACK]{json}[/FEEDBACK]` blocks, modeled on `parse_uncertainty()` in `crates/agentos-llm/src/types.rs` (lines 163-200)
   - Internal `FeedbackJson` serde helper struct for deserialization

6. Create `src/scenarios.rs` with:
   - `TestScenario` struct (name, description, system_prompt, initial_user_message, max_turns, required_permissions, goal_keywords)
   - `ScenarioResult` struct (scenario_name, outcome, turns_used, max_turns, tool_calls_made, feedback_count, total_tokens, total_cost_usd, duration_ms, error_message, turn_metrics)
   - `ScenarioOutcome` enum (Complete, Incomplete, Errored)
   - `builtin_scenarios(max_turns: usize) -> Vec<TestScenario>` returning an empty vec (populated in subtask 03)

7. Create `src/harness.rs` with `TestHarness` struct containing `kernel: Arc<Kernel>`, `llm: Arc<dyn LLMCore>`, `agent_name: String`, `agent_id: AgentID`, `data_dir: TempDir`. Methods are stubs (`todo!()`) -- implemented in subtask 02.

8. Create `src/report.rs` with `ReportGenerator` struct and `generate()` stub (`todo!()`).

9. Create `src/main.rs` with clap argument parsing and a basic startup message.

## Files Changed

| File | Change |
|------|--------|
| `Cargo.toml` | Add `"crates/agentos-agent-tester"` to workspace members |
| `crates/agentos-agent-tester/Cargo.toml` | New crate manifest |
| `crates/agentos-agent-tester/src/main.rs` | New -- CLI entry point |
| `crates/agentos-agent-tester/src/lib.rs` | New -- module declarations |
| `crates/agentos-agent-tester/src/feedback.rs` | New -- feedback types + parser |
| `crates/agentos-agent-tester/src/scenarios.rs` | New -- scenario types |
| `crates/agentos-agent-tester/src/harness.rs` | New -- TestHarness stub |
| `crates/agentos-agent-tester/src/report.rs` | New -- ReportGenerator stub |

## Prerequisites

None -- this is the first subtask.

## Test Plan

- `cargo build -p agentos-agent-tester` must compile without errors
- `cargo test -p agentos-agent-tester` must pass
- Add test `test_parse_feedback_single_block`: text with one `[FEEDBACK]{"category":"usability","severity":"info","observation":"test"}[/FEEDBACK]` produces one entry
- Add test `test_parse_feedback_multiple_blocks`: text with two blocks produces two entries
- Add test `test_parse_feedback_no_blocks`: plain text produces empty vec
- Add test `test_parse_feedback_malformed_json`: block with invalid JSON is skipped
- `./target/debug/agent-tester --help` prints usage text

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
./target/debug/agent-tester --help
```
