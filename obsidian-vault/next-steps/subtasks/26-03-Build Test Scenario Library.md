---
title: Build Test Scenario Library
tags:
  - testing
  - llm
  - agent
  - next-steps
date: 2026-03-18
status: complete
effort: 3d
priority: high
---

# Build Test Scenario Library

> Create 10 test scenarios covering agent lifecycle, tool discovery, file I/O, memory, pipelines, secrets, permissions, audit, error handling, and web UI evaluation.

---

## Why This Subtask

Without scenarios, the driver loop has nothing to execute. Each scenario targets a specific subsystem with a clear goal, system prompt, and success criteria. Mock responses enable deterministic CI runs.

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `src/scenarios.rs` | Flat file with types + empty `builtin_scenarios()` | Directory `src/scenarios/` with `mod.rs` + 10 scenario files |
| Scenario count | 0 | 10 (agent-lifecycle, tool-discovery, file-io, memory-rw, pipeline-exec, secret-management, permission-denial, audit-inspection, error-handling, web-ui) |
| Mock responses | N/A | Each scenario module has `mock_responses() -> Vec<String>` for `--provider mock` |
| Scenario filtering | N/A | `filter_scenarios(names, max_turns)` returns subset |

## What to Do

1. Convert `crates/agentos-agent-tester/src/scenarios.rs` to `crates/agentos-agent-tester/src/scenarios/mod.rs`. Move existing types there.

2. Add 10 sub-modules, one per scenario. Each module exports:
   - `pub fn scenario(max_turns: usize) -> TestScenario` with name, description, system_prompt, initial_user_message, max_turns, required_permissions, goal_keywords
   - `pub fn mock_responses() -> Vec<String>` with at least one response containing the goal keyword and a `[FEEDBACK]` block

3. The 10 scenarios and their key properties:

   | Scenario | Key Permissions | Goal Keyword | Tools Used |
   |----------|----------------|--------------|------------|
   | agent-lifecycle | fs.user_data | LIFECYCLE_COMPLETE | None (introspection) |
   | tool-discovery | fs.user_data | TOOLS_DISCOVERED | file-reader, file-writer |
   | file-io | fs.user_data | FILE_IO_COMPLETE | file-reader, file-writer |
   | memory-rw | memory.semantic, memory.episodic | MEMORY_COMPLETE | memory-write, memory-search |
   | pipeline-exec | fs.user_data, pipeline.execute | PIPELINE_COMPLETE | None (conceptual) |
   | secret-management | (none) | SECRETS_COMPLETE | None (conceptual) |
   | permission-denial | (none -- intentionally minimal) | PERMISSION_COMPLETE | shell-exec (denied) |
   | audit-inspection | (none) | AUDIT_COMPLETE | None (conceptual) |
   | error-handling | fs.user_data | ERRORS_COMPLETE | file-reader (with bad inputs) |
   | web-ui | (none) | WEBUI_COMPLETE | None (conceptual) |

4. Add `builtin_scenarios(max_turns)` that collects all 10 scenarios.

5. Add `filter_scenarios(names: &[String], max_turns: usize) -> Vec<TestScenario>` that filters by name.

6. Update `src/lib.rs` to point the `scenarios` module at the new directory.

7. In the harness, when provider is "mock", use the scenario's `mock_responses()` to initialize `MockLLMCore` for that scenario rather than a generic response.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-agent-tester/src/scenarios/mod.rs` | Types + `builtin_scenarios()` + `filter_scenarios()` |
| `crates/agentos-agent-tester/src/scenarios/agent_lifecycle.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/tool_discovery.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/file_io.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/memory_rw.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/pipeline_exec.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/secret_management.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/permission_denial.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/audit_inspection.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/error_handling.rs` | New |
| `crates/agentos-agent-tester/src/scenarios/web_ui.rs` | New |
| `crates/agentos-agent-tester/src/lib.rs` | Update scenarios module path |
| `crates/agentos-agent-tester/src/main.rs` | Wire `--scenarios` filtering, use per-scenario mock responses |

## Prerequisites

[[26-02-Implement LLM Driver Loop]] must be complete first.

## Test Plan

- `cargo build -p agentos-agent-tester` compiles
- `cargo test -p agentos-agent-tester` passes
- Test: `builtin_scenarios(10)` returns exactly 10 scenarios
- Test: Each scenario has non-empty name, description, system_prompt, initial_user_message, and goal_keywords
- Test: `filter_scenarios(&["file-io".to_string()], 10)` returns 1 scenario
- Test: Each `mock_responses()` contains the scenario's goal keyword
- `./target/debug/agent-tester --provider mock` runs all 10 and completes
- `./target/debug/agent-tester --provider mock --scenarios file-io,memory-rw` runs exactly 2

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
./target/debug/agent-tester --provider mock
./target/debug/agent-tester --provider mock --scenarios file-io,memory-rw
```
