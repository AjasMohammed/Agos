use agentos_kernel::config::{
    AuditSettings, BusSettings, HealthMonitorConfig, KernelConfig, KernelSettings, LlmSettings,
    MemorySettings, OllamaSettings, PreflightConfig, SecretsSettings, ToolsSettings,
};
use agentos_kernel::{parse_tool_call, Kernel};
use agentos_llm::{
    calculate_inference_cost, default_pricing_table, AnthropicCore, GeminiCore, LLMCore,
    MockLLMCore, ModelPricing, OllamaCore, OpenAICore,
};
use agentos_tools::ToolExecutionContext;
use agentos_types::*;
use agentos_vault::ZeroizingString;
use secrecy::SecretString;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::auto_feedback;
use crate::feedback::{parse_feedback, FeedbackCollector};
use crate::scenarios::{ScenarioOutcome, ScenarioResult, TestScenario, TurnMetrics};

/// Maximum number of context entries for the test harness context window.
/// Kept in sync with `context_window_max_entries` in `create_test_config`.
const CTX_MAX_ENTRIES: usize = 100;

pub struct TestHarness {
    pub kernel: Arc<Kernel>,
    pub llm: Arc<dyn LLMCore>,
    pub agent_name: String,
    pub agent_id: AgentID,
    pub data_dir: tempfile::TempDir,
    pub pricing: ModelPricing,
    /// Handle to the kernel run-loop task. Stored so `shutdown` can await clean
    /// teardown; `Drop` signals cancellation even if `shutdown` is never called.
    run_loop_task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        // Always signal the cancellation token so the run-loop terminates.
        // We cannot `.await` here (Drop is synchronous), but signalling ensures
        // the task exits promptly rather than running until process exit.
        self.kernel.shutdown();
        if let Some(handle) = self.run_loop_task.take() {
            handle.abort();
        }
    }
}

fn shared_model_cache_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-model-cache")
}

fn project_core_tools_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/core")
}

fn create_test_config(temp_dir: &tempfile::TempDir) -> KernelConfig {
    KernelConfig {
        kernel: KernelSettings {
            max_concurrent_tasks: 4,
            default_task_timeout_secs: 60,
            context_window_max_entries: CTX_MAX_ENTRIES,
            context_window_token_budget: 0,
            state_db_path: temp_dir
                .path()
                .join("kernel_state.db")
                .to_string_lossy()
                .to_string(),
            task_limits: Default::default(),
            tool_calls: Default::default(),
            tool_execution: Default::default(),
            autonomous_mode: Default::default(),
            health_port: 0,
            per_agent_rate_limit: 0,
            events: Default::default(),
            sandbox_policy: Default::default(),
            max_concurrent_sandbox_children: 4,
        },
        routing: Default::default(),
        secrets: SecretsSettings {
            vault_path: temp_dir
                .path()
                .join("vault/secrets.db")
                .to_string_lossy()
                .to_string(),
        },
        audit: AuditSettings {
            log_path: temp_dir
                .path()
                .join("data/audit.db")
                .to_string_lossy()
                .to_string(),
            max_audit_entries: 0,
            verify_last_n_entries: 0,
        },
        tools: ToolsSettings {
            core_tools_dir: temp_dir
                .path()
                .join("tools/core")
                .to_string_lossy()
                .to_string(),
            user_tools_dir: temp_dir
                .path()
                .join("tools/user")
                .to_string_lossy()
                .to_string(),
            data_dir: temp_dir.path().join("data").to_string_lossy().to_string(),
            crl_path: None,
            workspace: Default::default(),
        },
        bus: BusSettings {
            socket_path: temp_dir
                .path()
                .join("agentos.sock")
                .to_string_lossy()
                .to_string(),
            tls: None,
        },
        ollama: OllamaSettings {
            host: "http://localhost:11434".to_string(),
            default_model: "llama3.2".to_string(),
            request_timeout_secs: 300,
        },
        llm: LlmSettings::default(),
        memory: MemorySettings {
            model_cache_dir: shared_model_cache_dir().to_string_lossy().to_string(),
            extraction: Default::default(),
            consolidation: Default::default(),
        },
        context_budget: Default::default(),
        health_monitor: HealthMonitorConfig::default(),
        preflight: PreflightConfig::default(),
        logging: Default::default(),
    }
}

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
        "ollama" => Ok(Arc::new(OllamaCore::new(ollama_host, model))),
        "gemini" => {
            let key = api_key.ok_or_else(|| anyhow::anyhow!("--api-key required for gemini"))?;
            Ok(Arc::new(GeminiCore::new(
                SecretString::new(key.to_string()),
                model.to_string(),
            )))
        }
        "mock" => {
            // Fallback generic response used when the harness LLM is invoked directly via
            // `run_scenario()`. For deterministic mock runs use `run_scenario_with_mock()`
            // instead, which initializes the LLM with per-scenario canned responses.
            let base = "I have explored the AgentOS environment and found the available tools.\n\
                [FEEDBACK]\n\
                {\"category\":\"usability\",\"severity\":\"info\",\"observation\":\"System exploration complete\",\"suggestion\":null,\"context\":\"Tool discovery\"}\n\
                [/FEEDBACK]";
            Ok(Arc::new(MockLLMCore::new(vec![base.to_string(); 50])))
        }
        other => Err(anyhow::anyhow!("Unknown provider: {}", other)),
    }
}

/// Look up per-token pricing for the given provider/model from the built-in table.
/// Exact model match takes priority over a provider-level wildcard `"*"` entry.
/// Falls back to zero-cost pricing if neither is found.
fn find_pricing(provider: &str, model: &str) -> ModelPricing {
    let table = default_pricing_table();
    let exact = table
        .iter()
        .find(|p| p.provider == provider && p.model == model)
        .cloned();
    let wildcard = table
        .iter()
        .find(|p| p.provider == provider && p.model == "*")
        .cloned();
    exact.or(wildcard).unwrap_or(ModelPricing {
        provider: provider.to_string(),
        model: model.to_string(),
        input_per_1k: 0.0,
        output_per_1k: 0.0,
    })
}

/// Push the assistant's response text into the context window and return whether
/// any of the scenario's goal keywords are present in it.
///
/// The text is moved (not cloned) so callers in both the tool-call and no-tool-call
/// branches can avoid a heap allocation.
fn push_assistant_response(
    ctx: &mut ContextWindow,
    scenario: &TestScenario,
    response_text: String,
) -> bool {
    let goal_met = scenario
        .goal_keywords
        .iter()
        .any(|kw| response_text.to_lowercase().contains(&kw.to_lowercase()));
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
        is_summary: false,
    });
    goal_met
}

impl TestHarness {
    /// Boot a kernel in a temp directory and register the test agent.
    pub async fn boot(
        provider: &str,
        model: &str,
        api_key: Option<&str>,
    ) -> Result<Self, anyhow::Error> {
        let temp_dir = tempfile::TempDir::new()?;
        let config = create_test_config(&temp_dir);
        let config_path = temp_dir.path().join("config.toml");

        // Use async I/O — never block the async runtime with std::fs.
        tokio::fs::write(&config_path, toml::to_string(&config)?).await?;
        tokio::fs::create_dir_all(temp_dir.path().join("data")).await?;
        tokio::fs::create_dir_all(temp_dir.path().join("vault")).await?;
        tokio::fs::create_dir_all(temp_dir.path().join("tools/core")).await?;
        tokio::fs::create_dir_all(temp_dir.path().join("tools/user")).await?;

        // Copy real tool manifests from the project's tools/core directory into the
        // temp dir so the kernel has actual tools to register during tests.
        let src_tools = project_core_tools_dir();
        if src_tools.is_dir() {
            let dst_tools = temp_dir.path().join("tools/core");
            let mut entries = tokio::fs::read_dir(&src_tools).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                    let file_name = entry.file_name();
                    tokio::fs::copy(&path, dst_tools.join(&file_name)).await?;
                }
            }
        }

        // Ensure the shared model cache directory exists before kernel boot.
        tokio::fs::create_dir_all(shared_model_cache_dir()).await?;

        // Derive a unique passphrase from the temp dir's random component so
        // the vault is not trivially decryptable if the directory ends up on
        // shared storage (e.g. a CI NFS mount).
        let passphrase = ZeroizingString::new(format!(
            "test-{}",
            temp_dir
                .path()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));

        let kernel = Arc::new(Kernel::boot(&config_path, &passphrase).await?);

        // All remaining operations are fallible. The run-loop is spawned only after
        // they all succeed so that a failed boot never leaks a background task.
        let llm = create_llm_adapter(provider, model, api_key, &config.ollama.host)?;
        let agent_name = "test-agent".to_string();

        let provider_enum = match provider {
            "anthropic" => LLMProvider::Anthropic,
            "openai" => LLMProvider::OpenAI,
            "gemini" => LLMProvider::Gemini,
            // "mock" registers as Ollama to avoid the Custom provider's URL
            // validation in api_connect_agent. The actual LLM adapter is
            // replaced immediately after registration so the kernel never
            // performs inference through the registered provider type.
            "ollama" | "mock" => LLMProvider::Ollama,
            other => LLMProvider::Custom(other.to_string()),
        };

        kernel
            .api_connect_agent(
                agent_name.clone(),
                provider_enum,
                model.to_string(),
                None,
                vec!["base".to_string()],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect agent: {}", e))?;

        let agent_id = {
            let registry = kernel.agent_registry.read().await;
            registry
                .get_by_name(&agent_name)
                .ok_or_else(|| anyhow::anyhow!("Agent not found after registration"))?
                .id
        };

        // Wire the provided LLM adapter (overrides the one created by cmd_connect_agent).
        kernel
            .active_llms
            .write()
            .await
            .insert(agent_id, llm.clone());

        // Spawn run-loop only after all setup succeeds to avoid leaked background tasks.
        // Store the handle so shutdown() can await clean termination.
        let kernel_clone = kernel.clone();
        let run_loop_task = tokio::spawn(async move {
            if let Err(e) = kernel_clone.run().await {
                tracing::error!(error = %e, "Kernel run loop failed");
            }
        });

        Ok(Self {
            kernel,
            llm,
            agent_name,
            agent_id,
            data_dir: temp_dir,
            pricing: find_pricing(provider, model),
            run_loop_task: Some(run_loop_task),
        })
    }

    /// Signal the kernel to stop and await the run-loop task for clean teardown.
    pub async fn shutdown(&mut self) {
        self.kernel.shutdown();
        if let Some(handle) = self.run_loop_task.take() {
            let _ = handle.await;
        }
    }

    /// Grant scenario-specific permissions to the test agent.
    /// Permission format: `resource:rwx` (e.g. `fs.user_data:rw`).
    ///
    /// Returns the list of permissions that could not be granted. An empty vec
    /// means all grants succeeded. Callers must treat any failures as a setup
    /// error so they are not confused with correct permission-denial behaviour.
    pub async fn grant_permissions(&self, permissions: &[String]) -> Vec<String> {
        let mut failed = Vec::new();
        for perm in permissions {
            if let Err(e) = self
                .kernel
                .api_grant_permission(self.agent_name.clone(), perm.clone())
                .await
            {
                tracing::warn!(permission = %perm, error = %e, "Failed to grant permission");
                failed.push(perm.clone());
            }
        }
        failed
    }

    /// Run a test scenario using the harness's configured LLM adapter.
    pub async fn run_scenario(
        &self,
        scenario: &TestScenario,
        collector: &mut FeedbackCollector,
    ) -> ScenarioResult {
        self.run_scenario_with_llm(scenario, self.llm.clone(), collector)
            .await
    }

    /// Run a test scenario using per-scenario canned mock responses.
    /// Used when `--provider mock` is specified to run scenarios deterministically.
    pub async fn run_scenario_with_mock(
        &self,
        scenario: &TestScenario,
        mock_responses: Vec<String>,
        collector: &mut FeedbackCollector,
    ) -> ScenarioResult {
        let mock_llm: Arc<dyn LLMCore> = Arc::new(MockLLMCore::new(mock_responses));
        self.run_scenario_with_llm(scenario, mock_llm, collector)
            .await
    }

    /// Core scenario runner. Executes up to `scenario.max_turns` LLM inference rounds,
    /// collecting feedback and tool calls, and returns a `ScenarioResult`.
    async fn run_scenario_with_llm(
        &self,
        scenario: &TestScenario,
        llm: Arc<dyn LLMCore>,
        collector: &mut FeedbackCollector,
    ) -> ScenarioResult {
        let start = std::time::Instant::now();
        let mut total_tokens: u64 = 0;
        let mut total_cost_usd: f64 = 0.0;
        let mut tool_calls_made: usize = 0;
        let mut feedback_count: usize = 0;
        let mut turn_metrics: Vec<TurnMetrics> = Vec::new();

        let mut ctx =
            ContextWindow::with_strategy(CTX_MAX_ENTRIES, OverflowStrategy::SemanticEviction);

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
            is_summary: false,
        });

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
            is_summary: false,
        });

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
            is_summary: false,
        });

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
            is_summary: false,
        });

        let failed_perms = self.grant_permissions(&scenario.required_permissions).await;
        if !failed_perms.is_empty() {
            for perm in &failed_perms {
                collector.add(auto_feedback::feedback_from_permission_grant_failure(
                    &scenario.name,
                    perm,
                ));
            }
            return ScenarioResult {
                scenario_name: scenario.name.clone(),
                outcome: ScenarioOutcome::Errored,
                turns_used: 0,
                max_turns: scenario.max_turns,
                tool_calls_made: 0,
                feedback_count: failed_perms.len(),
                total_tokens: 0,
                total_cost_usd: 0.0,
                duration_ms: start.elapsed().as_millis() as u64,
                error_message: Some(format!(
                    "Scenario setup failed: could not grant permissions: {}",
                    failed_perms.join(", ")
                )),
                turn_metrics: Vec::new(),
            };
        }

        let mut outcome = ScenarioOutcome::Incomplete;
        let mut turns_used = 0;
        let mut error_message = None;

        for turn in 1..=scenario.max_turns {
            turns_used = turn;

            let infer_start = std::time::Instant::now();
            let infer_result = match llm.infer(&ctx).await {
                Ok(r) => r,
                Err(e) => {
                    let err_str = e.to_string();
                    error_message = Some(format!("LLM inference failed: {}", err_str));
                    collector.add(auto_feedback::feedback_from_inference_error(
                        &scenario.name,
                        turn,
                        &err_str,
                    ));
                    outcome = ScenarioOutcome::Errored;
                    break;
                }
            };
            let inference_ms = infer_start.elapsed().as_millis() as u64;

            let tokens_used = infer_result.tokens_used;
            let cost = calculate_inference_cost(&tokens_used, &self.pricing);
            total_cost_usd += cost.total_cost_usd;
            total_tokens += tokens_used.total_tokens;
            // Move text out of infer_result — no clone needed since tokens_used
            // is the only other field we use and it has already been extracted.
            let response_text = infer_result.text;

            let feedback_entries = parse_feedback(&response_text, &scenario.name, turn);
            feedback_count += feedback_entries.len();
            for entry in feedback_entries {
                collector.add(entry);
            }

            if let Some(tool_call) = parse_tool_call(&response_text) {
                tool_calls_made += 1;

                let tool_start = std::time::Instant::now();
                let (tool_result, succeeded) = match self.execute_tool_call(&tool_call).await {
                    Ok(output) => (output, true),
                    Err(error) => {
                        collector.add(auto_feedback::feedback_from_tool_error(
                            &scenario.name,
                            turn,
                            &tool_call.tool_name,
                            &error,
                        ));
                        (error, false)
                    }
                };
                let tool_ms = tool_start.elapsed().as_millis() as u64;

                turn_metrics.push(TurnMetrics {
                    turn,
                    inference_ms,
                    tool_execution_ms: Some(tool_ms),
                    input_tokens: tokens_used.prompt_tokens,
                    output_tokens: tokens_used.completion_tokens,
                    cost_usd: cost.total_cost_usd,
                    tool_called: Some(tool_call.tool_name.clone()),
                    tool_succeeded: Some(succeeded),
                });

                // Check goal keywords and push assistant entry (moves response_text).
                // Goal check runs before ctx.push so we can move the string in.
                let goal_met = push_assistant_response(&mut ctx, scenario, response_text);

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
                    is_summary: false,
                });

                if goal_met {
                    outcome = ScenarioOutcome::Complete;
                    break;
                }

                continue;
            }

            turn_metrics.push(TurnMetrics {
                turn,
                inference_ms,
                tool_execution_ms: None,
                input_tokens: tokens_used.prompt_tokens,
                output_tokens: tokens_used.completion_tokens,
                cost_usd: cost.total_cost_usd,
                tool_called: None,
                tool_succeeded: None, // no tool was called this turn
            });

            // Check goal keywords and push assistant entry (moves response_text).
            if push_assistant_response(&mut ctx, scenario, response_text) {
                outcome = ScenarioOutcome::Complete;
                break;
            }

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
                    is_summary: false,
                });
            }
        }

        if outcome == ScenarioOutcome::Incomplete {
            collector.add(auto_feedback::feedback_from_timeout(
                &scenario.name,
                scenario.max_turns,
            ));
        }

        ScenarioResult {
            scenario_name: scenario.name.clone(),
            outcome,
            turns_used,
            max_turns: scenario.max_turns,
            tool_calls_made,
            feedback_count,
            total_tokens,
            total_cost_usd,
            duration_ms: start.elapsed().as_millis() as u64,
            error_message,
            turn_metrics,
        }
    }

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

To use a tool, emit a tool call block in JSON format:

```json
{{"tool": "tool-name", "intent_type": "read", "payload": {{"key": "value"}}}}
```

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

    /// Execute a parsed tool call and return the serialized output.
    /// Returns `Ok(json)` on success and `Err(message)` on failure so callers
    /// can distinguish outcomes structurally rather than via string matching.
    async fn execute_tool_call(
        &self,
        tool_call: &agentos_kernel::ParsedToolCall,
    ) -> Result<String, String> {
        let permissions = self.get_agent_permissions().await;
        let ctx = ToolExecutionContext {
            task_id: TaskID::new(),
            agent_id: self.agent_id,
            data_dir: self.data_dir.path().join("data"),
            trace_id: TraceID::new(),
            permissions,
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            workspace_paths: vec![],
            cancellation_token: CancellationToken::new(),
        };

        self.kernel
            .tool_runner
            .execute(&tool_call.tool_name, tool_call.payload.clone(), ctx)
            .await
            .map(|result| {
                serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
            })
            .map_err(|e| e.to_string())
    }

    async fn get_agent_permissions(&self) -> PermissionSet {
        // Start with role-based permissions from the registry.
        let mut effective = {
            let registry = self.kernel.agent_registry.read().await;
            registry.compute_effective_permissions(&self.agent_id)
        };

        // Merge in runtime permissions granted via api_grant_permission (stored in capability engine).
        if let Ok(cap_perms) = self
            .kernel
            .capability_engine
            .get_permissions(&self.agent_id)
        {
            for entry in cap_perms.entries() {
                effective.grant(
                    entry.resource.clone(),
                    entry.read,
                    entry.write,
                    entry.execute,
                    entry.expires_at,
                );
            }
        }

        effective
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_pricing_known_model() {
        let pricing = find_pricing("anthropic", "claude-sonnet-4-6");
        assert_eq!(pricing.provider, "anthropic");
        assert_eq!(pricing.model, "claude-sonnet-4-6");
        assert!(pricing.input_per_1k > 0.0);
        assert!(pricing.output_per_1k > 0.0);
    }

    #[test]
    fn test_find_pricing_unknown_model_returns_zero() {
        let pricing = find_pricing("unknown-vendor", "unknown-model");
        assert_eq!(pricing.input_per_1k, 0.0);
        assert_eq!(pricing.output_per_1k, 0.0);
    }

    #[test]
    fn test_find_pricing_ollama_wildcard() {
        let pricing = find_pricing("ollama", "llama3.2");
        assert_eq!(pricing.input_per_1k, 0.0);
        assert_eq!(pricing.output_per_1k, 0.0);
    }
}
