---
title: Implement LLM Driver Loop
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

# Implement LLM Driver Loop

> Implement kernel boot, LLM adapter selection, agent registration, and the multi-turn infer/parse/execute driver loop in `TestHarness`.

---

## Why This Subtask

The driver loop is the core engine. It boots a kernel, sends prompts to a real (or mock) LLM, parses tool calls from responses, executes them against the kernel, and feeds results back. Without this, scenarios cannot run.

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `TestHarness::boot()` | `todo!()` | Boots kernel in temp dir (reusing e2e pattern from `crates/agentos-kernel/tests/e2e/common.rs`), creates LLM adapter, registers agent |
| `create_llm_adapter()` | N/A | Factory: maps `"anthropic"` to `AnthropicCore`, `"openai"` to `OpenAICore`, `"ollama"` to `OllamaCore`, `"gemini"` to `GeminiCore`, `"mock"` to `MockLLMCore` |
| `run_scenario()` | N/A | Multi-turn loop: `infer()` -> `parse_tool_call()` -> `execute_tool_call()` -> append to context -> check goal -> repeat |
| `execute_tool_call()` | N/A | Calls `ToolRunner::execute()` with proper `ToolExecutionContext` |
| `main.rs` | Prints skeleton message | Boots harness, iterates scenarios, runs driver loop |

## What to Do

1. Open `crates/agentos-agent-tester/src/harness.rs`

2. Add `create_test_config(temp_dir: &TempDir) -> KernelConfig` -- copy the pattern from `crates/agentos-kernel/tests/e2e/common.rs` (lines 20-82). Use `shared_model_cache_dir()` pointing to `target/test-model-cache`. Key fields:
   - `kernel.max_concurrent_tasks = 4`
   - `kernel.context_window_max_entries = 100`
   - `kernel.per_agent_rate_limit = 0` (disable rate limiting for tests)
   - All paths relative to `temp_dir.path()`

3. Add `create_llm_adapter(provider, model, api_key, ollama_host) -> Result<Arc<dyn LLMCore>>`:
   - `"anthropic"` -> `AnthropicCore::new(SecretString::new(key), model)` (see `crates/agentos-llm/src/anthropic.rs` line 21)
   - `"openai"` -> `OpenAICore::new(SecretString::new(key), model)` (verify constructor in `crates/agentos-llm/src/openai.rs`)
   - `"ollama"` -> `OllamaCore::new(host, model)` (see `crates/agentos-llm/src/ollama.rs` line 17)
   - `"gemini"` -> `GeminiCore::new(SecretString::new(key), model)` (verify constructor in `crates/agentos-llm/src/gemini.rs`)
   - `"mock"` -> `MockLLMCore::new(vec!["...".to_string()])` (see `crates/agentos-llm/src/mock.rs` line 18)

4. Implement `TestHarness::boot()`:
   - Create temp dir, write config TOML, create subdirectories (data, vault, tools/core, tools/user)
   - Call `Kernel::boot(&config_path, &ZeroizingString::new("test-passphrase"))` (see `crates/agentos-kernel/src/kernel.rs` line 123)
   - Spawn `kernel.run()` in a background tokio task
   - Call `kernel.api_connect_agent(name, provider_enum, model, None, vec!["base".to_string()])` (see `kernel.rs` line 546)
   - Look up agent_id from `kernel.agent_registry.read().await.get_by_name(name)`
   - Insert LLM into `kernel.active_llms.write().await.insert(agent_id, llm)`

5. Implement `TestHarness::run_scenario()`:
   - Build `ContextWindow::new(100)` with `SemanticEviction` strategy
   - Push system prompt (testing persona) as pinned `ContextRole::System` entry with `ContextCategory::System`
   - Push scenario system prompt as pinned `ContextRole::System`
   - Push tool descriptions from `kernel.tool_registry.read().await.loaded` (field `manifest.manifest.name` and `.description`)
   - Push initial user message as `ContextRole::User` with `ContextCategory::Task`
   - Loop up to `max_turns`:
     - Call `self.llm.infer(&ctx).await` (see `LLMCore` trait in `crates/agentos-llm/src/traits.rs` line 9)
     - Parse `[FEEDBACK]` blocks via `parse_feedback()` from `src/feedback.rs`
     - Parse tool calls via `parse_tool_call()` from `crates/agentos-kernel/src/tool_call.rs` (re-exported as `agentos_kernel::parse_tool_call`)
     - If tool call found: execute via `ToolRunner`, append assistant + tool result entries
     - If no tool call: check goal keywords, append assistant entry
     - Return `ScenarioResult`

6. Implement `execute_tool_call()`:
   - Build `ToolExecutionContext` with `task_id: TaskID::new()`, `agent_id: self.agent_id`, `data_dir: self.data_dir.path().join("data")`, `trace_id: TraceID::new()`, permissions from `kernel.agent_registry.read().await.compute_effective_permissions(&self.agent_id)`, `vault: None`, `hal: None`, `file_lock_registry: None`
   - Call `self.kernel.tool_runner.execute(name, input, ctx).await`
   - Verify the exact signature of `ToolRunner::execute()` from `crates/agentos-tools/src/runner.rs`

7. Implement `grant_permissions()` using the kernel's permission command dispatch.

8. Update `main.rs` to boot the harness and run scenarios in a loop.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-agent-tester/src/harness.rs` | Implement `boot()`, `run_scenario()`, `execute_tool_call()`, helpers |
| `crates/agentos-agent-tester/src/main.rs` | Wire boot + scenario loop |

## Prerequisites

[[26-01-Create Agent Tester Crate]] must be complete first.

## Test Plan

- `cargo build -p agentos-agent-tester` compiles
- Add integration test `tests/harness_boot_test.rs`:
  - `TestHarness::boot("mock", "mock-model", None)` succeeds
  - Agent is registered in `kernel.agent_registry`
  - LLM is inserted in `kernel.active_llms`
  - `harness.shutdown()` completes cleanly
- `./target/debug/agent-tester --provider mock` boots, runs mock scenario, exits 0

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
./target/debug/agent-tester --provider mock
```
