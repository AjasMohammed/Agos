---
title: LLM Agent Testing Harness
tags:
  - testing
  - llm
  - agent
  - next-steps
date: 2026-03-18
status: complete
effort: 12d
priority: high
---

# LLM Agent Testing Harness

> Build a test harness that uses a real LLM as an agent-user of AgentOS, exercises every subsystem, and produces structured usability/correctness feedback.

---

## Current State

All existing tests use `MockLLMCore` with canned responses. No test has ever exercised AgentOS from the perspective of a real reasoning LLM. The OS is designed for AI agents but has never been tested by one.

## Goal / Target State

A new `crates/agentos-agent-tester` crate with a binary `agent-tester` that:
- Boots a real kernel in a temp directory
- Connects a real LLM (Anthropic/OpenAI/Ollama/Gemini) or a mock
- Runs 10 test scenarios (agent lifecycle, tool discovery, file I/O, memory, pipelines, secrets, permissions, audit, errors, web UI)
- The LLM emits structured `[FEEDBACK]` blocks about usability, correctness, and ergonomics
- Produces a markdown (and optional JSON) report at `reports/agent-test-YYYY-MM-DD.md`

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[26-01-Create Agent Tester Crate]] | `Cargo.toml`, `crates/agentos-agent-tester/` | complete |
| 02 | [[26-02-Implement LLM Driver Loop]] | `harness.rs`, `main.rs` | complete |
| 03 | [[26-03-Build Test Scenario Library]] | `scenarios/*.rs` | complete |
| 04 | [[26-04-Add Feedback Capture Layer]] | `feedback.rs`, `auto_feedback.rs`, `harness.rs` | complete |
| 05 | [[26-05-Implement Report Generator]] | `report.rs`, `main.rs`, `.gitignore` | complete |

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester
./target/debug/agent-tester --provider mock
ls reports/agent-test-*.md
```

## Related

- [[LLM Agent Testing Plan]] -- master plan with design decisions and risks
- [[LLM Agent Testing Data Flow]] -- data flow diagram
- [[01-test-harness-crate]] through [[05-report-generation]] -- phase details
