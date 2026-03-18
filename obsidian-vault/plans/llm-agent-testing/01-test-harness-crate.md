---
title: "Phase 01: Test Harness Crate Skeleton"
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

# Phase 01: Test Harness Crate Skeleton

> Create the `agentos-agent-tester` crate with CLI argument parsing, kernel boot, LLM adapter selection, and the foundational types for scenarios and feedback.

---

## Why This Phase

Everything else depends on having a runnable binary that can boot a kernel, instantiate an LLM adapter, and accept scenario configurations. This phase delivers the skeleton that phases 02-05 build upon.

---

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `crates/agentos-agent-tester/` | Does not exist | New crate with `Cargo.toml`, `src/main.rs`, `src/lib.rs` |
| Workspace `Cargo.toml` | 15 members | 16 members (add `crates/agentos-agent-tester`) |
| CLI parsing | N/A | `clap`-based binary with `--provider`, `--model`, `--api-key`, `--scenarios`, `--output-dir`, `--mock`, `--max-turns`, `--runs` flags |
| Kernel boot for testing | Only in `tests/e2e/common.rs` | Extracted into a reusable `TestKernelBuilder` in the new crate |
| Core types | N/A | `TestScenario`, `FeedbackEntry`, `FeedbackCategory`, `FeedbackSeverity`, `ScenarioResult`, `ScenarioOutcome` |

---

## What to Do

### 1. Create crate directory and `Cargo.toml`

Create `crates/agentos-agent-tester/Cargo.toml`:

```toml
[package]
name = "agentos-agent-tester"
version.workspace = true
edition.workspace = true

[[bin]]
name = "agent-tester"
path = "src/main.rs"

[dependencies]
agentos-types = { path = "../agentos-types" }
agentos-kernel = { path = "../agentos-kernel" }
agentos-llm = { path = "../agentos-llm" }
agentos-bus = { path = "../agentos-bus" }
agentos-tools = { path = "../agentos-tools" }
agentos-audit = { path = "../agentos-audit" }
agentos-vault = { path = "../agentos-vault" }
agentos-capability = { path = "../agentos-capability" }
tokio = { workspace = true }
clap = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
tempfile = { workspace = true }
toml = { workspace = true }
secrecy = "0.8"
```

### 2. Add to workspace

Open `/home/ajas/Desktop/agos/Cargo.toml` and add `"crates/agentos-agent-tester"` to the `members` array.

### 3. Create `src/lib.rs` with core types

```rust
pub mod feedback;
pub mod harness;
pub mod scenarios;
pub mod report;

pub use feedback::{FeedbackCategory, FeedbackCollector, FeedbackEntry, FeedbackSeverity};
pub use harness::TestHarness;
pub use scenarios::{ScenarioOutcome, ScenarioResult, TestScenario};
pub use report::ReportGenerator;
```

### 4. Create `src/feedback.rs`

Define the feedback types:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub scenario: String,
    pub turn: usize,
    pub category: FeedbackCategory,
    pub severity: FeedbackSeverity,
    pub observation: String,
    pub suggestion: Option<String>,
    pub context: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    Usability,
    Correctness,
    Ergonomics,
    Security,
    Performance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    pub fn new() -> Self { Self { entries: Vec::new() } }
    pub fn add(&mut self, entry: FeedbackEntry) { self.entries.push(entry); }
    pub fn entries(&self) -> &[FeedbackEntry] { &self.entries }
    pub fn into_entries(self) -> Vec<FeedbackEntry> { self.entries }
}
```

Also implement `parse_feedback()` -- a parser for `[FEEDBACK]...[/FEEDBACK]` blocks in LLM responses, modeled on `parse_uncertainty()` in `agentos-llm/src/types.rs`:

```rust
pub fn parse_feedback(text: &str, scenario: &str, turn: usize) -> Vec<FeedbackEntry> {
    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find("[FEEDBACK]") {
        let abs_start = search_from + start + "[FEEDBACK]".len();
        if let Some(end_offset) = text[abs_start..].find("[/FEEDBACK]") {
            let block = &text[abs_start..abs_start + end_offset].trim();
            if let Ok(entry) = serde_json::from_str::<FeedbackJson>(block) {
                results.push(FeedbackEntry {
                    scenario: scenario.to_string(),
                    turn,
                    category: entry.category,
                    severity: entry.severity,
                    observation: entry.observation,
                    suggestion: entry.suggestion,
                    context: entry.context,
                });
            }
            search_from = abs_start + end_offset + "[/FEEDBACK]".len();
        } else {
            break;
        }
    }
    results
}
```

### 5. Create `src/scenarios.rs` with scenario types

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct TestScenario {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub initial_user_message: String,
    pub max_turns: usize,
    pub required_permissions: Vec<String>,
    pub goal_keywords: Vec<String>,  // If LLM response contains any of these, goal is met
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub scenario_name: String,
    pub outcome: ScenarioOutcome,
    pub turns_used: usize,
    pub max_turns: usize,
    pub tool_calls_made: usize,
    pub feedback_count: usize,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub duration_ms: u64,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioOutcome {
    Complete,
    Incomplete,
    Errored,
}
```

### 6. Create `src/harness.rs` with `TestHarness` struct

```rust
use agentos_kernel::Kernel;
use agentos_llm::LLMCore;
use std::sync::Arc;

pub struct TestHarness {
    pub kernel: Arc<Kernel>,
    pub llm: Arc<dyn LLMCore>,
    pub agent_name: String,
    pub agent_id: agentos_types::AgentID,
    pub data_dir: tempfile::TempDir,
}

impl TestHarness {
    /// Boot a kernel in a temp directory and register the test agent.
    pub async fn boot(
        provider: &str,
        model: &str,
        api_key: Option<&str>,
    ) -> Result<Self, anyhow::Error> {
        // Implementation in Phase 02
        todo!()
    }

    pub async fn shutdown(&self) {
        self.kernel.shutdown();
    }
}
```

### 7. Create `src/report.rs` stub

```rust
pub struct ReportGenerator;

impl ReportGenerator {
    pub fn generate(
        _results: &[crate::ScenarioResult],
        _feedback: &[crate::FeedbackEntry],
    ) -> String {
        // Implementation in Phase 05
        todo!()
    }
}
```

### 8. Create `src/main.rs` with CLI parsing

```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "agent-tester", about = "LLM-driven AgentOS test harness")]
struct Args {
    /// LLM provider: anthropic, openai, ollama, gemini, mock
    #[arg(long, default_value = "mock")]
    provider: String,

    /// Model name (e.g. claude-sonnet-4-6, gpt-4o, llama3.2)
    #[arg(long, default_value = "mock-model")]
    model: String,

    /// API key (or set AGENTOS_TEST_API_KEY env var)
    #[arg(long, env = "AGENTOS_TEST_API_KEY")]
    api_key: Option<String>,

    /// Comma-separated scenario names to run (default: all)
    #[arg(long)]
    scenarios: Option<String>,

    /// Output directory for reports
    #[arg(long, default_value = "reports")]
    output_dir: String,

    /// Maximum turns per scenario
    #[arg(long, default_value = "10")]
    max_turns: usize,

    /// Number of runs per scenario (for consensus)
    #[arg(long, default_value = "1")]
    runs: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    tracing::info!(provider = %args.provider, model = %args.model, "Starting agent-tester");
    // Wire up in Phase 02
    println!("agent-tester skeleton running. Phases 02-05 provide implementation.");
    Ok(())
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `Cargo.toml` (workspace root) | Add `"crates/agentos-agent-tester"` to `members` |
| `crates/agentos-agent-tester/Cargo.toml` | New file -- crate manifest |
| `crates/agentos-agent-tester/src/main.rs` | New file -- CLI entry point |
| `crates/agentos-agent-tester/src/lib.rs` | New file -- module declarations and re-exports |
| `crates/agentos-agent-tester/src/feedback.rs` | New file -- `FeedbackEntry`, `FeedbackCollector`, `parse_feedback()` |
| `crates/agentos-agent-tester/src/scenarios.rs` | New file -- `TestScenario`, `ScenarioResult`, `ScenarioOutcome` |
| `crates/agentos-agent-tester/src/harness.rs` | New file -- `TestHarness` struct (stubs) |
| `crates/agentos-agent-tester/src/report.rs` | New file -- `ReportGenerator` stub |

---

## Dependencies

None -- this is the first phase.

---

## Test Plan

- `cargo build -p agentos-agent-tester` must compile without errors.
- `cargo test -p agentos-agent-tester` must pass (feedback parser tests).
- Add unit tests for `parse_feedback()`:
  - Test: text with one `[FEEDBACK]...[/FEEDBACK]` block produces one `FeedbackEntry` with correct fields.
  - Test: text with two feedback blocks produces two entries.
  - Test: text with no feedback blocks produces empty vec.
  - Test: malformed JSON inside a feedback block is skipped gracefully (empty vec for that block).
  - Test: nested or overlapping tags handled without panic.
- `./target/debug/agent-tester --help` prints usage.
- `./target/debug/agent-tester` runs and prints the skeleton message without crashing.

---

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
./target/debug/agent-tester --help
./target/debug/agent-tester
```
