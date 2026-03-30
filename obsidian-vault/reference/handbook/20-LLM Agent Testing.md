---
title: LLM Agent Testing
tags:
  - reference
  - testing
  - llm
  - v3
date: 2026-03-18
status: complete
---

# LLM Agent Testing

> `agent-tester` is a binary that boots a real AgentOS kernel in-process and drives it with an actual LLM to evaluate how well the system behaves from an AI agent's perspective — covering usability, correctness, security, and ergonomics across every major subsystem.

---

## Overview

Traditional unit and integration tests verify that code behaves correctly for known inputs. LLM agent testing asks a different question: *does the system behave correctly when a real language model is operating it?*

The `agent-tester` binary (`crates/agentos-agent-tester`) works by:

1. Booting a full `Kernel` in a temporary directory (isolated, no config files required)
2. Registering a test agent wired to a real or mock LLM backend
3. Running structured **scenarios** — each describing a task, required permissions, and success criteria
4. Collecting **structured feedback** emitted by the LLM in `[FEEDBACK]...[/FEEDBACK]` blocks
5. Generating a **Markdown report** (and optionally JSON) with per-scenario outcomes, consensus across runs, and categorized feedback

This surfaces issues that static tests cannot: confusing tool APIs, unhelpful error messages, unclear documentation strings, missing feedback on failure, and security behaviours that only emerge when an LLM is reasoning about them.

---

## Quick Start

### Mock mode (no API key — runs in CI)

```bash
cargo run -p agentos-agent-tester -- --provider mock
```

The mock provider uses canned, deterministic responses. All 10 scenarios complete in seconds with zero cost.

### Real LLM — Anthropic (recommended for weekly evaluation)

```bash
export AGENTOS_TEST_API_KEY=sk-ant-...

cargo run -p agentos-agent-tester -- \
  --provider anthropic \
  --model claude-haiku-4-5-20251001 \
  --runs 1
```

### Real LLM — Ollama (free, local)

```bash
cargo run -p agentos-agent-tester -- \
  --provider ollama \
  --model llama3.2 \
  --runs 3
```

Reports are written to `reports/agent-test-YYYY-MM-DD-HHmmss.md`.

---

## CLI Reference

```
cargo run -p agentos-agent-tester -- [OPTIONS]
```

| Flag | Default | Description |
|---|---|---|
| `--provider <name>` | `mock` | LLM backend: `mock`, `anthropic`, `openai`, `ollama`, `gemini` |
| `--model <name>` | `mock-model` | Model identifier (e.g. `claude-sonnet-4-6`, `gpt-4o`, `llama3.2`) |
| `--api-key <key>` | — | API key (or use `AGENTOS_TEST_API_KEY` env var) |
| `--scenarios <list>` | all | Comma-separated scenario names to run |
| `--output-dir <path>` | `reports` | Directory for output reports (no `..` allowed) |
| `--max-turns <n>` | `10` | Turn budget per scenario (minimum 1) |
| `--runs <n>` | `3` | Runs per scenario for consensus (minimum 1) |
| `--json` | off | Also write a JSON report alongside the Markdown report |

### Examples

```bash
# Run only two scenarios, one run each
cargo run -p agentos-agent-tester -- \
  --provider anthropic \
  --model claude-sonnet-4-6 \
  --scenarios "tool-discovery,permission-denial" \
  --runs 1

# Full evaluation with consensus (3 runs per scenario) and JSON output
cargo run -p agentos-agent-tester -- \
  --provider anthropic \
  --model claude-sonnet-4-6 \
  --runs 3 \
  --json \
  --output-dir reports/weekly

# Quick smoke test against a local Ollama instance
cargo run -p agentos-agent-tester -- \
  --provider ollama \
  --model llama3.2 \
  --max-turns 5 \
  --runs 1
```

---

## Built-in Scenarios

Ten scenarios ship out of the box, covering every major AgentOS subsystem.

| Scenario Name | What It Tests |
|---|---|
| `agent-lifecycle` | Agent registration, connection, status, shutdown |
| `tool-discovery` | Tool listing, manifest reading, capability inspection |
| `file-io` | File read/write/delete operations and path safety |
| `memory-rw` | Writing and retrieving entries across memory tiers |
| `pipeline-exec` | Creating and executing a multi-step pipeline |
| `secret-management` | Vault storage, retrieval, rotation, and scoping |
| `permission-denial` | Capability enforcement — actions blocked without permission |
| `audit-inspection` | Querying, filtering, and exporting the audit log |
| `error-handling` | Recovery after tool errors, invalid inputs, and timeouts |
| `web-ui` | Web interface availability and response correctness |

Each scenario has:
- A **system prompt** establishing the agent's role
- An **initial user message** describing the task
- A set of **required permissions** granted before the run starts
- **Goal keywords** — if any appear in the LLM's response, the scenario is `Complete`
- A **turn budget** (controlled by `--max-turns`)

---

## How a Scenario Run Works

```
boot Kernel (TempDir)
  │
  ▼
register test-agent → grant required permissions
  │
  ▼
turn 1: inject system_prompt + initial_user_message
  │
  ├─→ LLM responds → parse [TOOL_CALL] block?
  │     ├─ yes → execute tool → inject [TOOL_RESULT] → loop
  │     └─ no  → check goal_keywords → if met → Complete
  │
  ├─→ goal keyword found in any response → Complete
  ├─→ turn budget exhausted → Incomplete
  └─→ kernel/LLM error → Errored
  │
  ▼
collect FEEDBACK blocks → auto_feedback for errors/timeouts
  │
  ▼
write report → shutdown kernel
```

### Tool Call Format

The LLM calls tools using a structured block that the harness parses:

```
[TOOL_CALL]
{"tool": "memory-write", "intent_type": "write", "payload": {"key": "foo", "value": "bar", "tier": "working"}}
[/TOOL_CALL]
```

The harness executes the tool against the real kernel, then injects the result:

```
[TOOL_RESULT]
{"success": true, "output": "Written to working memory."}
[/TOOL_RESULT]
```

### Outcome States

| Outcome | Meaning |
|---|---|
| `Complete` | A goal keyword appeared in one of the LLM's responses |
| `Incomplete` | Turn budget exhausted without a goal keyword being detected |
| `Errored` | Kernel error, permission grant failure, or unrecoverable inference error |

---

## Feedback System

Feedback is how the LLM communicates observations about the AgentOS API during a test run. This is the primary signal for improving usability, ergonomics, and error messaging.

### Emitting Feedback

The LLM includes a `[FEEDBACK]...[/FEEDBACK]` block anywhere in its response:

```
[FEEDBACK]
{
  "category": "usability",
  "severity": "warning",
  "observation": "The memory-write tool returns no confirmation of what tier was used, making it hard to verify the write succeeded.",
  "suggestion": "Include the tier name in the success response.",
  "context": "Turn 2 — tried to write to episodic tier"
}
[/FEEDBACK]
```

### Feedback Fields

| Field | Required | Values |
|---|---|---|
| `category` | yes | `usability`, `correctness`, `ergonomics`, `security`, `performance` |
| `severity` | yes | `info`, `warning`, `error` |
| `observation` | yes | Free-text description of the issue |
| `suggestion` | no | How to fix it |
| `context` | no | When/where it happened |

### Feedback Categories

| Category | Covers |
|---|---|
| `usability` | Tool APIs that are confusing, poorly named, or lacking clear documentation |
| `correctness` | Unexpected tool behaviour, wrong outputs, silent failures |
| `ergonomics` | Friction points: verbose inputs, missing defaults, awkward patterns |
| `security` | Permission denials that were too strict, too lenient, or uninformative |
| `performance` | Slow responses, excessive turns needed to complete simple tasks |

### Auto-Feedback

In addition to LLM-emitted feedback, the harness generates automatic feedback entries for:

| Trigger | Category | Severity |
|---|---|---|
| Tool execution error | `correctness` | `error` |
| Inference error (LLM call failed) | `correctness` | `error` |
| Turn budget exhausted (Incomplete outcome) | `usability` | `warning` |
| Permission grant failure during setup | `security` | `warning` |

### Deduplication

After all runs complete, the feedback collector deduplicates entries using Jaccard word similarity (case-insensitive). Entries in the same category with >80% word overlap are merged and annotated with `(observed N times)`. This prevents noise from repeated identical observations across multiple runs drowning out distinct findings.

---

## Consensus and Multi-Run Results

Running each scenario multiple times (default `--runs 3`) reveals whether failures are systematic or intermittent.

The consensus table in the report classifies each scenario as:

| Verdict | Condition |
|---|---|
| `PASS` | ≥ 2/3 runs are `Complete` |
| `PARTIAL` | Some runs `Complete`, some `Incomplete` or `Errored` |
| `FAIL` | All runs `Incomplete` or `Errored` |

Example report section:

```
## Consensus (3 runs per scenario)
| Scenario | Run 1 | Run 2 | Run 3 | Verdict |
|---|---|---|---|---|
| agent-lifecycle   | ✓ | ✓ | ✓ | PASS    |
| tool-discovery    | ✓ | ✓ | ✗ | PARTIAL |
| permission-denial | ✗ | ✗ | ✗ | FAIL    |
```

Use `--runs 1` for fast development feedback loops and `--runs 3` (the default) for meaningful pre-release evaluations.

---

## Report Format

### Markdown Report

Written to `<output-dir>/agent-test-YYYY-MM-DD-HHmmss.md`. Sections:

1. **Summary** — provider, model, date, total scenarios, outcomes breakdown
2. **Results table** — per-run outcome, turns used, tool calls, feedback count, tokens, cost, duration
3. **Consensus table** — shown when `--runs > 1`
4. **Feedback by category** — deduplicated feedback grouped by category, severity-sorted
5. **Per-run turn metrics** — inference ms, tool execution ms, tokens, cost per turn (shown for Errored/Incomplete scenarios)

### JSON Report (`--json`)

Written alongside the Markdown as `.json`. Structure:

```json
{
  "provider": "anthropic",
  "model": "claude-haiku-4-5-20251001",
  "generated_at": "2026-03-18T10:30:00Z",
  "results": [ /* ScenarioResult[] */ ],
  "feedback": [ /* FeedbackEntry[] */ ],
  "stats": {
    "total_entries": 12,
    "by_category": { "usability": 5, "correctness": 3, ... },
    "by_severity": { "error": 2, "warning": 7, "info": 3 },
    "by_scenario": { "tool-discovery": 4, ... }
  }
}
```

The JSON report is suitable for diff tracking across releases, feeding into dashboards, or automated regression gates.

---

## Provider Selection Guide

| Provider | Cost | Speed | Best For |
|---|---|---|---|
| `mock` | Free | Instant | CI, smoke tests, development |
| `ollama` + `llama3.2` | Free | ~2–5s/turn | Frequent local evaluation |
| `anthropic` + `claude-haiku-4-5-20251001` | Low (~$0.01–$0.10/run) | Fast | Pre-release evaluation |
| `anthropic` + `claude-sonnet-4-6` | Medium | Moderate | Deep quality evaluation |
| `openai` + `gpt-4o` | Medium | Fast | Cross-model comparison |

**Recommended cadence:**
- **Every commit (CI):** `--provider mock --runs 1`
- **Weekly or pre-release:** `--provider anthropic --model claude-haiku-4-5-20251001 --runs 3`
- **Major releases:** `--provider anthropic --model claude-sonnet-4-6 --runs 3 --json`

---

## Environment Variables

| Variable | Purpose |
|---|---|
| `AGENTOS_TEST_API_KEY` | API key for the selected provider (alternative to `--api-key`) |
| `RUST_LOG` | Log level (e.g. `RUST_LOG=info` for progress, `RUST_LOG=debug` for full traces) |

---

## CI Integration

Add mock-mode testing to your CI pipeline:

```yaml
# .github/workflows/ci.yml
- name: LLM Agent Tests (mock)
  run: cargo run -p agentos-agent-tester -- --provider mock --runs 1
```

Mock mode guarantees:
- Zero external dependencies
- Deterministic outcomes (canned responses per scenario)
- Sub-second execution
- All 10 scenarios exercise the real kernel code paths

---

## Isolation Model

Each test run boots a completely isolated environment:

- **Temporary directory** — created by `tempfile::TempDir`, deleted on shutdown
- **Unique vault passphrase** — derived from the temp dir path (no shared state between runs)
- **No config files required** — the harness creates a minimal in-memory kernel config
- **No network calls** — all tool execution hits the in-process kernel; the only network traffic is the LLM API call itself (absent in mock mode)

Runs do not interfere with each other or with a running production kernel.

---

## Adding Custom Scenarios

New scenarios are Rust modules in `crates/agentos-agent-tester/src/scenarios/`. Each must export:

```rust
/// The scenario definition.
pub fn scenario(max_turns: usize) -> TestScenario { ... }

/// Canned mock responses for CI / mock-mode runs.
/// Every response must contain at least one goal keyword.
pub fn mock_responses() -> Vec<String> { ... }
```

Register the module in `scenarios/mod.rs`:

```rust
pub mod my_new_scenario;

pub fn builtin_scenarios(max_turns: usize) -> Vec<TestScenario> {
    vec![
        // ... existing scenarios ...
        my_new_scenario::scenario(max_turns),
    ]
}

pub fn mock_responses_for(name: &str) -> Vec<String> {
    match name {
        // ... existing arms ...
        "my-new-scenario" => my_new_scenario::mock_responses(),
        _ => vec![],
    }
}
```

**Required fields for `TestScenario`:**

| Field | Purpose |
|---|---|
| `name` | Kebab-case identifier used with `--scenarios` flag |
| `description` | One-line summary shown in the report header |
| `system_prompt` | Role and context given to the LLM at the start |
| `initial_user_message` | The task the LLM is asked to perform |
| `max_turns` | Maximum turns before marking `Incomplete` |
| `required_permissions` | Permissions granted to the test agent before the scenario starts |
| `goal_keywords` | If any appear (case-insensitive) in the LLM's response, the scenario is `Complete` |

---

## Interpreting Results

### Complete scenarios with zero feedback
The LLM completed the task without any observations. Either the scenario is well-covered, or the model didn't engage with the feedback protocol. Check the turn count — a completion in 1–2 turns with no feedback is suspicious.

### Incomplete scenarios
The LLM could not complete the task within the turn budget. Check:
- The feedback for `usability` warnings (confusing API?)
- The turn metrics for inference errors
- Whether the `goal_keywords` are too specific (the LLM solved the task but with different wording)

### Errored scenarios
A kernel error or permission grant failure blocked execution. The `error_message` field in the result and the auto-generated `correctness/error` feedback entry will point to the cause.

### PARTIAL consensus
The scenario succeeds sometimes but not always. This typically indicates:
- Non-deterministic LLM behaviour (increase `--runs` to characterise)
- A flaky tool response (check the `correctness` feedback)
- A turn budget that is too tight for the task (increase `--max-turns`)

---

## Related

- [[15-LLM Configuration]] — configuring the LLM providers used by `agent-tester`
- [[07-Tool System]] — the tools that scenarios exercise
- [[08-Security Model]] — permission enforcement validated by the `permission-denial` scenario
- [[14-Audit Log]] — the `audit-inspection` scenario queries this directly
- [[19-Troubleshooting and FAQ]] — common errors during test runs
