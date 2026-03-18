---
title: "Phase 03: Test Scenario Library"
tags:
  - testing
  - llm
  - agent
  - plan
date: 2026-03-18
status: planned
effort: 3d
priority: high
---

# Phase 03: Test Scenario Library

> Build the library of concrete test scenarios covering every major AgentOS subsystem: agent lifecycle, tool discovery, file I/O, memory, pipelines, secrets, permissions, audit, and error handling.

---

## Why This Phase

Scenarios are the "test cases" of the LLM agent testing system. Without well-defined scenarios with clear goals, system prompts, and success criteria, the LLM would wander aimlessly and produce unusable feedback. Each scenario targets a specific subsystem and asks the LLM to perform a concrete task.

---

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `src/scenarios.rs` | Type definitions + empty `builtin_scenarios()` | 10 fully defined scenarios with system prompts, goals, and permission sets |
| Scenario modules | N/A | `src/scenarios/` directory with one file per scenario category |
| Goal checking | `goal_keywords: Vec<String>` pattern matching | Same -- keyword matching is sufficient for structured scenarios |
| Mock scripted responses | N/A | Each scenario has a `mock_responses()` method returning canned responses for `--provider mock` |

---

## What to Do

### 1. Restructure `src/scenarios.rs` into `src/scenarios/mod.rs`

Move `TestScenario`, `ScenarioResult`, `ScenarioOutcome` to `src/scenarios/mod.rs` and add:

```rust
pub mod agent_lifecycle;
pub mod tool_discovery;
pub mod file_io;
pub mod memory_rw;
pub mod pipeline_exec;
pub mod secret_management;
pub mod permission_denial;
pub mod audit_inspection;
pub mod error_handling;
pub mod web_ui;

pub fn builtin_scenarios(max_turns: usize) -> Vec<TestScenario> {
    vec![
        agent_lifecycle::scenario(max_turns),
        tool_discovery::scenario(max_turns),
        file_io::scenario(max_turns),
        memory_rw::scenario(max_turns),
        pipeline_exec::scenario(max_turns),
        secret_management::scenario(max_turns),
        permission_denial::scenario(max_turns),
        audit_inspection::scenario(max_turns),
        error_handling::scenario(max_turns),
        web_ui::scenario(max_turns),
    ]
}

/// Return scenarios filtered by name.
pub fn filter_scenarios(names: &[String], max_turns: usize) -> Vec<TestScenario> {
    builtin_scenarios(max_turns)
        .into_iter()
        .filter(|s| names.iter().any(|n| n == &s.name))
        .collect()
}
```

### 2. Implement Scenario 1: Agent Lifecycle (`src/scenarios/agent_lifecycle.rs`)

```rust
use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "agent-lifecycle".to_string(),
        description: "Test agent registration, status checking, and identity verification".to_string(),
        system_prompt: r#"You are testing the agent lifecycle in AgentOS.

Your task:
1. Confirm you are registered as an agent by checking your status
2. Verify you have an agent identity (Ed25519 public key)
3. List all registered agents to see who else is connected
4. Report any issues with the registration process

Use the available tools to accomplish this. If you encounter errors, report them as feedback.

When you have completed all steps, include the word "LIFECYCLE_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Begin the agent lifecycle test. Start by checking your registration status.".to_string(),
        max_turns,
        required_permissions: vec![
            "fs.user_data".to_string(),
        ],
        goal_keywords: vec!["LIFECYCLE_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll check my agent status. Let me try to use the system.

[FEEDBACK]
{"category": "usability", "severity": "info", "observation": "I'm starting as a new agent. The system prompt tells me I'm registered but I need to verify.", "suggestion": "Provide an explicit agent status tool", "context": "Initial registration check"}
[/FEEDBACK]

I can see I'm registered. LIFECYCLE_COMPLETE"#.to_string(),
    ]
}
```

### 3. Implement Scenario 2: Tool Discovery (`src/scenarios/tool_discovery.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "tool-discovery".to_string(),
        description: "Test tool listing, manifest inspection, and understanding tool capabilities".to_string(),
        system_prompt: r#"You are testing tool discovery in AgentOS.

Your task:
1. Review the list of available tools provided in the system context
2. Try to understand what each tool does from its description
3. Identify any tools whose purpose or usage is unclear
4. Try calling one tool (file-reader) to verify it works
5. Report on the quality of tool descriptions and discoverability

When done, include "TOOLS_DISCOVERED" in your response."#.to_string(),
        initial_user_message: "Review the available tools and test the file-reader tool. Create a test file first using file-writer, then read it back.".to_string(),
        max_turns,
        required_permissions: vec![
            "fs.user_data".to_string(),
        ],
        goal_keywords: vec!["TOOLS_DISCOVERED".to_string()],
    }
}
```

### 4. Implement Scenario 3: File I/O (`src/scenarios/file_io.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "file-io".to_string(),
        description: "Test file reading, writing, directory listing, and path handling".to_string(),
        system_prompt: r#"You are testing file I/O operations in AgentOS.

Your task:
1. Write a file named "test-output.txt" with the content "Hello from AgentOS test"
2. Read the file back and verify the content matches
3. List the directory contents to see the file
4. Try to write to a nested subdirectory path ("subdir/nested.txt")
5. Try to read a non-existent file and observe the error
6. Report on error message quality, path handling, and overall file I/O ergonomics

Use the file-writer and file-reader tools.

When done, include "FILE_IO_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Start the file I/O test. Write a test file, then read it back.".to_string(),
        max_turns,
        required_permissions: vec![
            "fs.user_data".to_string(),
        ],
        goal_keywords: vec!["FILE_IO_COMPLETE".to_string()],
    }
}
```

### 5. Implement Scenario 4: Memory Read/Write (`src/scenarios/memory_rw.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "memory-rw".to_string(),
        description: "Test semantic and episodic memory write and search".to_string(),
        system_prompt: r#"You are testing the memory subsystem in AgentOS.

Your task:
1. Write a semantic memory entry about "Q1 revenue was 2.5 million dollars" with key "q1-revenue"
2. Search for it using a query like "revenue earnings"
3. Write an episodic memory entry about "Agent completed file I/O test successfully" with scope "episodic"
4. Search for the episodic entry
5. Report on memory search quality (relevance of results, latency, ease of use)

Use the memory-write and memory-search tools.

When done, include "MEMORY_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Begin the memory test. Write a semantic memory entry, then search for it.".to_string(),
        max_turns,
        required_permissions: vec![
            "memory.semantic".to_string(),
            "memory.episodic".to_string(),
        ],
        goal_keywords: vec!["MEMORY_COMPLETE".to_string()],
    }
}
```

### 6. Implement Scenario 5: Pipeline Execution (`src/scenarios/pipeline_exec.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "pipeline-exec".to_string(),
        description: "Test multi-step pipeline definition and execution".to_string(),
        system_prompt: r#"You are testing the pipeline execution system in AgentOS.

Your task:
1. Define a simple 2-step pipeline in YAML:
   - Step 1: Write a file "pipeline-output.txt" with content "Pipeline step 1"
   - Step 2: Read the file back
2. Install the pipeline
3. Run the pipeline
4. Check the pipeline status
5. Report on the pipeline definition format, error messages, and execution feedback

Note: Pipeline YAML format:
```yaml
name: test-pipeline
description: A test pipeline
steps:
  - name: write-step
    tool: file-writer
    input:
      path: pipeline-output.txt
      content: "Pipeline step 1"
  - name: read-step
    tool: file-reader
    input:
      path: pipeline-output.txt
    depends_on:
      - write-step
```

When done, include "PIPELINE_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Begin the pipeline test. Note that pipeline operations use the kernel API, not tools. Describe what you would do and provide feedback on the pipeline system design.".to_string(),
        max_turns,
        required_permissions: vec![
            "fs.user_data".to_string(),
            "pipeline.execute".to_string(),
        ],
        goal_keywords: vec!["PIPELINE_COMPLETE".to_string()],
    }
}
```

### 7. Implement Scenario 6: Secret Management (`src/scenarios/secret_management.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "secret-management".to_string(),
        description: "Test secrets vault interaction: set, list, and revoke secrets".to_string(),
        system_prompt: r#"You are testing the secrets vault in AgentOS.

Your task:
1. Describe how you would store an API key as a secret
2. Consider the security implications of secret storage
3. Note whether secrets are accessible to agents or only via proxy tokens
4. Report on the secret management ergonomics from an agent perspective

Note: Secrets are managed through the kernel API (set/list/revoke), not through tools.
The vault uses AES-256-GCM encryption with Argon2id key derivation.
Agents should use proxy tokens rather than accessing raw secret values.

When done, include "SECRETS_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Evaluate the secrets management system from an agent's perspective. What would you need to store API keys securely?".to_string(),
        max_turns,
        required_permissions: vec![],
        goal_keywords: vec!["SECRETS_COMPLETE".to_string()],
    }
}
```

### 8. Implement Scenario 7: Permission Denial (`src/scenarios/permission_denial.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "permission-denial".to_string(),
        description: "Test error handling when agent lacks required permissions".to_string(),
        system_prompt: r#"You are testing permission enforcement in AgentOS.

Your task:
1. Try to use the shell-exec tool (you should NOT have execute permissions)
2. Try to read a file outside your allowed path
3. Observe the error messages you get
4. Report on the quality of permission denial messages:
   - Are they clear about what permission is missing?
   - Do they suggest how to request the permission?
   - Are they consistent across different tools?

When done, include "PERMISSION_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Test permission boundaries. Try operations you should not be allowed to perform and report on error quality.".to_string(),
        max_turns,
        // Intentionally minimal permissions -- the test is about denial
        required_permissions: vec![],
        goal_keywords: vec!["PERMISSION_COMPLETE".to_string()],
    }
}
```

### 9. Implement Scenario 8: Audit Inspection (`src/scenarios/audit_inspection.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "audit-inspection".to_string(),
        description: "Test audit log inspection and understanding of audit trail".to_string(),
        system_prompt: r#"You are testing the audit log system in AgentOS.

Your task:
1. Reason about what audit events should exist after kernel boot and agent registration
2. Consider what information an agent would want from the audit log
3. Report on the audit system from an agent's perspective:
   - What events are useful for an agent to see?
   - What events should be hidden from agents for security?
   - How would an agent verify the integrity of the audit chain?

Note: The audit log uses an append-only SQLite database with Merkle chain verification.
83+ event types are tracked. The kernel writes audit entries for all security-relevant operations.

When done, include "AUDIT_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Evaluate the audit system from an agent's perspective. What audit information would be most useful?".to_string(),
        max_turns,
        required_permissions: vec![],
        goal_keywords: vec!["AUDIT_COMPLETE".to_string()],
    }
}
```

### 10. Implement Scenario 9: Error Handling (`src/scenarios/error_handling.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "error-handling".to_string(),
        description: "Test error conditions: invalid tool names, malformed inputs, timeouts".to_string(),
        system_prompt: r#"You are testing error handling in AgentOS.

Your task:
1. Try to call a tool that does not exist ("nonexistent-tool")
2. Try to call file-reader with invalid JSON input
3. Try to call file-reader with a missing required field (no "path")
4. Try to read a file that does not exist ("does-not-exist.txt")
5. For each error, report:
   - Was the error message helpful?
   - Did it tell you what went wrong?
   - Did it suggest how to fix the issue?
   - Was the error type/category appropriate?

When done, include "ERRORS_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Test various error conditions. Intentionally make mistakes and evaluate the error messages.".to_string(),
        max_turns,
        required_permissions: vec![
            "fs.user_data".to_string(),
        ],
        goal_keywords: vec!["ERRORS_COMPLETE".to_string()],
    }
}
```

### 11. Implement Scenario 10: Web UI (`src/scenarios/web_ui.rs`)

```rust
pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "web-ui".to_string(),
        description: "Evaluate the web UI design from an agent perspective (conceptual)".to_string(),
        system_prompt: r#"You are evaluating the AgentOS web UI from an agent's perspective.

AgentOS includes a web UI (Axum + HTMX) that provides:
- Dashboard showing connected agents, active tasks, system health
- Agent management (connect, disconnect, view status)
- Task management (run, cancel, view logs)
- Tool management (list, install, remove)
- Audit log viewer
- Secrets management
- Pipeline management

Your task:
1. Consider what API endpoints an agent would need to interact with the web UI programmatically
2. Evaluate whether the web UI's REST endpoints are agent-friendly (JSON responses vs HTML)
3. Suggest improvements for making the web UI usable by both human operators and AI agents
4. Consider authentication and authorization for web UI access

When done, include "WEBUI_COMPLETE" in your response."#.to_string(),
        initial_user_message: "Evaluate the web UI design from an agent's perspective. What would you need from an API to manage agents and tasks programmatically?".to_string(),
        max_turns,
        required_permissions: vec![],
        goal_keywords: vec!["WEBUI_COMPLETE".to_string()],
    }
}
```

### 12. Add `mock_responses()` to each scenario module

Each scenario module already shown above should include a `pub fn mock_responses() -> Vec<String>` that returns canned LLM responses containing the goal keyword and at least one `[FEEDBACK]` block. This allows `--provider mock` to run all scenarios deterministically.

### 13. Update `TestHarness::run_scenario()` to use mock responses per scenario

In the driver loop (Phase 02), when the provider is "mock", replace the generic `MockLLMCore` with one initialized from the scenario's `mock_responses()`. This requires the harness to swap out the LLM adapter per scenario. Add a method:

```rust
pub async fn run_scenario_with_mock(
    &self,
    scenario: &TestScenario,
    mock_responses: Vec<String>,
    collector: &mut FeedbackCollector,
) -> ScenarioResult {
    let mock_llm: Arc<dyn LLMCore> = Arc::new(MockLLMCore::new(mock_responses));
    // Replace self.llm temporarily for this scenario
    self.run_scenario_with_llm(scenario, &mock_llm, collector).await
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-agent-tester/src/scenarios/mod.rs` | Move types from `scenarios.rs`, add `builtin_scenarios()`, `filter_scenarios()` |
| `crates/agentos-agent-tester/src/scenarios/agent_lifecycle.rs` | New -- scenario 1 |
| `crates/agentos-agent-tester/src/scenarios/tool_discovery.rs` | New -- scenario 2 |
| `crates/agentos-agent-tester/src/scenarios/file_io.rs` | New -- scenario 3 |
| `crates/agentos-agent-tester/src/scenarios/memory_rw.rs` | New -- scenario 4 |
| `crates/agentos-agent-tester/src/scenarios/pipeline_exec.rs` | New -- scenario 5 |
| `crates/agentos-agent-tester/src/scenarios/secret_management.rs` | New -- scenario 6 |
| `crates/agentos-agent-tester/src/scenarios/permission_denial.rs` | New -- scenario 7 |
| `crates/agentos-agent-tester/src/scenarios/audit_inspection.rs` | New -- scenario 8 |
| `crates/agentos-agent-tester/src/scenarios/error_handling.rs` | New -- scenario 9 |
| `crates/agentos-agent-tester/src/scenarios/web_ui.rs` | New -- scenario 10 |
| `crates/agentos-agent-tester/src/lib.rs` | Update `scenarios` module path |
| `crates/agentos-agent-tester/src/main.rs` | Wire scenario filtering via `--scenarios` flag |

---

## Dependencies

[[02-llm-driver-loop]] must be complete first -- the driver loop executes scenarios, and the harness `run_scenario()` method must work before scenarios can be meaningfully tested.

---

## Test Plan

- `cargo build -p agentos-agent-tester` must compile.
- `cargo test -p agentos-agent-tester` must pass.
- Add unit tests:
  - Test: `builtin_scenarios(10)` returns exactly 10 scenarios.
  - Test: Each scenario has a non-empty `name`, `description`, `system_prompt`, `initial_user_message`, and at least one `goal_keywords` entry.
  - Test: `filter_scenarios(&["file-io".to_string()], 10)` returns exactly 1 scenario with name "file-io".
  - Test: Each `mock_responses()` contains the corresponding `goal_keywords` string so mock runs complete.
- Integration test: `./target/debug/agent-tester --provider mock` runs all 10 scenarios and completes without errors.
- Integration test: `./target/debug/agent-tester --provider mock --scenarios file-io,memory-rw` runs exactly 2 scenarios.

---

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
./target/debug/agent-tester --provider mock
./target/debug/agent-tester --provider mock --scenarios file-io,memory-rw
```
