---
title: "Phase 02: LLM Driver Loop"
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

# Phase 02: LLM Driver Loop

> Implement the core interaction loop that boots a kernel, connects a real LLM, sends scenario prompts, parses tool calls from responses, executes them against the kernel, and feeds results back to the LLM for multi-turn conversations.

---

## Why This Phase

The driver loop is the central engine of the entire test harness. Without it, scenarios cannot be executed. This phase turns the skeleton from Phase 01 into a functional multi-turn LLM<->Kernel interaction system.

---

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `TestHarness::boot()` | `todo!()` stub | Boots kernel in temp dir, creates LLM adapter based on `--provider`, registers agent with permissions |
| LLM adapter creation | N/A | Factory function: `create_llm_adapter(provider, model, api_key, base_url) -> Arc<dyn LLMCore>` |
| Driver loop | N/A | `TestHarness::run_scenario(&self, scenario, collector) -> ScenarioResult` -- multi-turn infer/parse/execute cycle |
| Tool call execution | N/A | Routes parsed tool calls through `ToolRunner` with proper `ToolExecutionContext` |
| `main.rs` | Prints skeleton message | Boots harness, runs scenarios, collects feedback, prints summary |

---

## What to Do

### 1. Implement LLM adapter factory in `src/harness.rs`

Add a function that maps CLI strings to concrete adapters:

```rust
use agentos_llm::{
    AnthropicCore, GeminiCore, LLMCore, MockLLMCore, OllamaCore, OpenAICore,
};
use secrecy::SecretString;

fn create_llm_adapter(
    provider: &str,
    model: &str,
    api_key: Option<&str>,
    ollama_host: &str,
) -> Result<Arc<dyn LLMCore>, anyhow::Error> {
    match provider {
        "anthropic" => {
            let key = api_key.ok_or_else(|| anyhow::anyhow!("--api-key required for anthropic"))?;
            Ok(Arc::new(AnthropicCore::new(
                SecretString::new(key.to_string()),
                model.to_string(),
            )))
        }
        "openai" => {
            let key = api_key.ok_or_else(|| anyhow::anyhow!("--api-key required for openai"))?;
            Ok(Arc::new(OpenAICore::new(
                SecretString::new(key.to_string()),
                model.to_string(),
            )))
        }
        "ollama" => {
            Ok(Arc::new(OllamaCore::new(ollama_host, model)))
        }
        "gemini" => {
            let key = api_key.ok_or_else(|| anyhow::anyhow!("--api-key required for gemini"))?;
            Ok(Arc::new(GeminiCore::new(
                SecretString::new(key.to_string()),
                model.to_string(),
            )))
        }
        "mock" => {
            // Mock returns a fixed set of responses; scenarios provide scripted responses
            Ok(Arc::new(MockLLMCore::new(vec![
                "I understand my role as a test agent. Let me explore the system.".to_string(),
            ])))
        }
        other => Err(anyhow::anyhow!("Unknown provider: {}", other)),
    }
}
```

Note: Check `OpenAICore::new` and `GeminiCore::new` constructor signatures from `crates/agentos-llm/src/openai.rs` and `crates/agentos-llm/src/gemini.rs` respectively before implementing. They use `SecretString` for API keys similar to `AnthropicCore`.

### 2. Implement `TestHarness::boot()`

Reuse the kernel boot pattern from `crates/agentos-kernel/tests/e2e/common.rs`:

```rust
use agentos_kernel::config::*;
use agentos_kernel::Kernel;
use agentos_vault::ZeroizingString;

impl TestHarness {
    pub async fn boot(
        provider: &str,
        model: &str,
        api_key: Option<&str>,
    ) -> Result<Self, anyhow::Error> {
        let temp_dir = tempfile::TempDir::new()?;
        let config = create_test_config(&temp_dir);
        let config_path = temp_dir.path().join("config.toml");
        std::fs::write(&config_path, toml::to_string(&config)?)?;

        // Create required directories
        std::fs::create_dir_all(temp_dir.path().join("data"))?;
        std::fs::create_dir_all(temp_dir.path().join("vault"))?;
        std::fs::create_dir_all(temp_dir.path().join("tools/core"))?;
        std::fs::create_dir_all(temp_dir.path().join("tools/user"))?;

        let kernel = Arc::new(
            Kernel::boot(&config_path, &ZeroizingString::new("test-passphrase".to_string()))
                .await?,
        );

        // Spawn the kernel run loop in background
        let kernel_clone = kernel.clone();
        tokio::spawn(async move {
            if let Err(e) = kernel_clone.run().await {
                tracing::error!(error = %e, "Kernel run loop failed");
            }
        });

        let llm = create_llm_adapter(provider, model, api_key, &config.ollama.host)?;
        let agent_name = "test-agent".to_string();

        // Register the test agent
        let provider_enum = match provider {
            "anthropic" => agentos_types::LLMProvider::Anthropic,
            "openai" => agentos_types::LLMProvider::OpenAI,
            "ollama" => agentos_types::LLMProvider::Ollama,
            "gemini" => agentos_types::LLMProvider::Custom("gemini".to_string()),
            _ => agentos_types::LLMProvider::Ollama,
        };

        kernel.api_connect_agent(
            agent_name.clone(),
            provider_enum,
            model.to_string(),
            None,
            vec!["base".to_string()],
        ).await.map_err(|e| anyhow::anyhow!("Failed to connect agent: {}", e))?;

        // Look up the agent ID from the registry
        let agent_id = {
            let registry = kernel.agent_registry.read().await;
            registry.get_by_name(&agent_name)
                .ok_or_else(|| anyhow::anyhow!("Agent not found after registration"))?
                .id
        };

        // Wire the LLM adapter into the kernel's active_llms map
        kernel.active_llms.write().await.insert(agent_id, llm.clone());

        Ok(Self {
            kernel,
            llm,
            agent_name,
            agent_id,
            data_dir: temp_dir,
        })
    }
}
```

Also add `create_test_config()` as a private function in `harness.rs`, modeled on `tests/e2e/common.rs::create_test_config()` with the same `KernelConfig` fields. Use `MemorySettings` with a shared model cache dir at `target/test-model-cache` (same pattern as e2e tests).

### 3. Implement `TestHarness::grant_permissions()`

```rust
impl TestHarness {
    /// Grant scenario-specific permissions to the test agent.
    pub async fn grant_permissions(&self, permissions: &[String]) {
        let mut registry = self.kernel.agent_registry.write().await;
        if let Some(agent) = registry.agents.get_mut(&self.agent_id) {
            // Note: agents field is private; use the kernel's public API instead
        }
        drop(registry);

        // Use kernel's grant permission command
        for perm in permissions {
            let _ = self.kernel.cmd_grant_permission(
                self.agent_name.clone(),
                perm.clone(),
            ).await;
        }
    }
}
```

Note: Verify the exact method signature of `cmd_grant_permission` in `crates/agentos-kernel/src/commands/` before implementing. It may accept `(agent_name: String, permission: String)` and return `KernelResponse`.

### 4. Implement `TestHarness::run_scenario()`

This is the core driver loop:

```rust
use agentos_kernel::parse_tool_call;
use agentos_types::*;
use crate::feedback::{parse_feedback, FeedbackCollector};
use crate::scenarios::{ScenarioOutcome, ScenarioResult, TestScenario};

impl TestHarness {
    pub async fn run_scenario(
        &self,
        scenario: &TestScenario,
        collector: &mut FeedbackCollector,
    ) -> ScenarioResult {
        let start = std::time::Instant::now();
        let mut total_tokens: u64 = 0;
        let mut tool_calls_made: usize = 0;
        let mut feedback_count: usize = 0;

        // Build context window
        let mut ctx = ContextWindow::new(100);
        ctx.overflow_strategy = OverflowStrategy::SemanticEviction;

        // System prompt: testing persona
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: self.build_testing_persona_prompt(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
        });

        // Scenario-specific system prompt
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: scenario.system_prompt.clone(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.9,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
        });

        // Tool descriptions
        let tool_descriptions = self.get_tool_descriptions().await;
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: format!("Available tools:\n{}", tool_descriptions),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.8,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Tools,
        });

        // Initial user message
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: scenario.initial_user_message.clone(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.7,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
        });

        // Grant scenario permissions
        self.grant_permissions(&scenario.required_permissions).await;

        // Driver loop
        let mut outcome = ScenarioOutcome::Incomplete;
        let mut turns_used = 0;
        let mut error_message = None;

        for turn in 1..=scenario.max_turns {
            turns_used = turn;

            // Call LLM
            let infer_result = match self.llm.infer(&ctx).await {
                Ok(r) => r,
                Err(e) => {
                    error_message = Some(format!("LLM inference failed: {}", e));
                    outcome = ScenarioOutcome::Errored;
                    break;
                }
            };

            total_tokens += infer_result.tokens_used.total_tokens;
            let response_text = infer_result.text.clone();

            // Parse feedback blocks
            let feedback_entries = parse_feedback(&response_text, &scenario.name, turn);
            feedback_count += feedback_entries.len();
            for entry in feedback_entries {
                collector.add(entry);
            }

            // Parse tool calls
            if let Some(tool_call) = parse_tool_call(&response_text) {
                tool_calls_made += 1;

                // Execute tool
                let tool_result = self.execute_tool_call(&tool_call).await;

                // Append assistant message
                ctx.push(ContextEntry {
                    role: ContextRole::Assistant,
                    content: response_text,
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                    importance: 0.5,
                    pinned: false,
                    reference_count: 0,
                    partition: ContextPartition::Active,
                    category: ContextCategory::History,
                });

                // Append tool result
                ctx.push(ContextEntry {
                    role: ContextRole::ToolResult,
                    content: tool_result,
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                    importance: 0.6,
                    pinned: false,
                    reference_count: 0,
                    partition: ContextPartition::Active,
                    category: ContextCategory::History,
                });

                continue; // Tool result feeds back to LLM on next turn
            }

            // No tool call -- append as assistant response
            ctx.push(ContextEntry {
                role: ContextRole::Assistant,
                content: response_text.clone(),
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.5,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::Active,
                category: ContextCategory::History,
            });

            // Check if goal is met
            let goal_met = scenario.goal_keywords.iter().any(|kw| {
                response_text.to_lowercase().contains(&kw.to_lowercase())
            });

            if goal_met {
                outcome = ScenarioOutcome::Complete;
                break;
            }

            // If not the last turn, prompt for continuation
            if turn < scenario.max_turns {
                ctx.push(ContextEntry {
                    role: ContextRole::User,
                    content: "Continue with the task. Remember to emit [FEEDBACK] blocks for any observations.".to_string(),
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                    importance: 0.4,
                    pinned: false,
                    reference_count: 0,
                    partition: ContextPartition::Active,
                    category: ContextCategory::Task,
                });
            }
        }

        ScenarioResult {
            scenario_name: scenario.name.clone(),
            outcome,
            turns_used,
            max_turns: scenario.max_turns,
            tool_calls_made,
            feedback_count,
            total_tokens,
            total_cost_usd: 0.0, // Calculated in Phase 04
            duration_ms: start.elapsed().as_millis() as u64,
            error_message,
        }
    }
}
```

### 5. Implement helper methods

```rust
impl TestHarness {
    fn build_testing_persona_prompt(&self) -> String {
        format!(
            r#"You are an AI agent testing AgentOS, a Rust-based operating system designed for AI agents.

Your role is to explore the system's capabilities as a new user would, attempting the task given to you and providing structured feedback about your experience.

IMPORTANT: After each interaction with the system, emit a feedback block in this exact format:

[FEEDBACK]
{{"category": "usability", "severity": "info", "observation": "Description of what you observed", "suggestion": "How it could be improved", "context": "What you were trying to do"}}
[/FEEDBACK]

Categories: usability, correctness, ergonomics, security, performance
Severities: info, warning, error

To use a tool, emit a tool call block:

[TOOL_CALL]
name: tool-name
input: {{"key": "value"}}
[/TOOL_CALL]

Always provide at least one feedback observation per response. Be honest about confusion, errors, or friction.
Your agent name is: {}"#,
            self.agent_name
        )
    }

    async fn get_tool_descriptions(&self) -> String {
        let registry = self.kernel.tool_registry.read().await;
        let mut descriptions = Vec::new();
        for loaded in &registry.loaded {
            let m = &loaded.manifest.manifest;
            descriptions.push(format!("- {}: {}", m.name, m.description));
        }
        if descriptions.is_empty() {
            "No tools are currently registered.".to_string()
        } else {
            descriptions.join("\n")
        }
    }

    async fn execute_tool_call(
        &self,
        tool_call: &agentos_kernel::ParsedToolCall,
    ) -> String {
        let ctx = agentos_tools::ToolExecutionContext {
            task_id: agentos_types::TaskID::new(),
            agent_id: self.agent_id,
            data_dir: self.data_dir.path().join("data"),
            trace_id: agentos_types::TraceID::new(),
            permissions: self.get_agent_permissions().await,
            vault: None,
            hal: None,
            file_lock_registry: None,
        };

        match self.kernel.tool_runner.execute(&tool_call.name, tool_call.input.clone(), ctx).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string()),
            Err(e) => format!("Tool execution failed: {}", e),
        }
    }

    async fn get_agent_permissions(&self) -> agentos_types::PermissionSet {
        let registry = self.kernel.agent_registry.read().await;
        registry.compute_effective_permissions(&self.agent_id)
    }
}
```

Note: Verify that `ToolRunner` has an `execute(&self, name: &str, input: Value, ctx: ToolExecutionContext) -> Result<Value, AgentOSError>` method. Check `crates/agentos-tools/src/runner.rs` for the exact signature. The `loaded` field on `ToolRegistry` is public based on usage in `kernel.rs`.

### 6. Wire `main.rs` to boot and run scenarios

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let harness = TestHarness::boot(
        &args.provider,
        &args.model,
        args.api_key.as_deref(),
    ).await?;

    let scenarios = crate::scenarios::builtin_scenarios(args.max_turns);
    let mut collector = FeedbackCollector::new();
    let mut results = Vec::new();

    for scenario in &scenarios {
        tracing::info!(scenario = %scenario.name, "Running scenario");
        for run in 0..args.runs {
            tracing::info!(scenario = %scenario.name, run = run + 1, "Run");
            let result = harness.run_scenario(scenario, &mut collector).await;
            tracing::info!(
                scenario = %result.scenario_name,
                outcome = ?result.outcome,
                turns = result.turns_used,
                "Scenario complete"
            );
            results.push(result);
        }
    }

    // Report generation (Phase 05)
    let feedback = collector.into_entries();
    println!("Completed {} scenario runs, collected {} feedback entries", results.len(), feedback.len());

    harness.shutdown().await;
    Ok(())
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-agent-tester/src/harness.rs` | Implement `TestHarness::boot()`, `run_scenario()`, `execute_tool_call()`, helper methods |
| `crates/agentos-agent-tester/src/main.rs` | Wire CLI args to harness boot, scenario execution loop |
| `crates/agentos-agent-tester/src/feedback.rs` | No changes (from Phase 01) |
| `crates/agentos-agent-tester/src/scenarios.rs` | Add `builtin_scenarios()` placeholder returning empty vec |

---

## Dependencies

[[01-test-harness-crate]] must be complete first -- crate skeleton, types, and CLI parsing are prerequisites.

---

## Test Plan

- `cargo build -p agentos-agent-tester` must compile.
- `cargo test -p agentos-agent-tester` must pass all existing and new tests.
- Add integration test `tests/harness_boot_test.rs`:
  - Test: `TestHarness::boot("mock", "mock-model", None)` boots successfully and registers an agent.
  - Test: After boot, `kernel.agent_registry.read().await.get_by_name("test-agent")` returns `Some`.
  - Test: After boot, `kernel.active_llms.read().await.contains_key(&agent_id)` is true.
  - Test: `harness.shutdown()` completes without panic.
- Add unit test for tool description generation:
  - Test: `get_tool_descriptions()` returns a non-empty string listing at least the core built-in tools (file-reader, file-writer, etc.).
- Running `./target/debug/agent-tester --provider mock` boots the kernel, runs the mock scenario loop, and exits cleanly.

---

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
./target/debug/agent-tester --provider mock
```
