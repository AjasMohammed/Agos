use crate::definition::{OnFailure, PipelineDefinition, PipelineStep, StepAction};
use crate::store::PipelineStore;
use crate::types::{PipelineRun, PipelineRunStatus, StepResult, StepStatus};
use agentos_types::{AgentOSError, RunID};
use chrono::Utc;
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;

/// Callback trait for the pipeline engine to dispatch agent tasks and tool calls.
/// The kernel implements this to bridge the pipeline engine to the actual kernel subsystems.
#[async_trait::async_trait]
pub trait PipelineExecutor: Send + Sync {
    /// Run an agent task and return the result string.
    async fn run_agent_task(&self, agent_name: &str, prompt: &str) -> Result<String, AgentOSError>;

    /// Execute a tool directly and return the result string.
    async fn run_tool(
        &self,
        tool_name: &str,
        input: serde_json::Value,
    ) -> Result<String, AgentOSError>;
}

pub struct PipelineEngine {
    store: Arc<PipelineStore>,
}

impl PipelineEngine {
    pub fn new(store: Arc<PipelineStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &PipelineStore {
        &self.store
    }

    /// Run a pipeline end-to-end.
    pub async fn run(
        &self,
        definition: &PipelineDefinition,
        input: &str,
        run_id: RunID,
        executor: &dyn PipelineExecutor,
    ) -> Result<PipelineRun, AgentOSError> {
        // Validate the pipeline
        self.validate(definition)?;

        // Initialize run
        let mut run = PipelineRun {
            id: run_id,
            pipeline_name: definition.name.clone(),
            input: input.to_string(),
            status: PipelineRunStatus::Running,
            step_results: HashMap::new(),
            output: None,
            started_at: Utc::now(),
            completed_at: None,
            error: None,
        };

        // Persist initial run state
        self.store.create_run(&run)?;

        // Build variable context with built-in variables
        let mut context: HashMap<String, String> = HashMap::new();
        context.insert("input".to_string(), input.to_string());
        context.insert("run_id".to_string(), run_id.to_string());
        context.insert(
            "date".to_string(),
            Utc::now().format("%Y-%m-%d").to_string(),
        );
        context.insert("timestamp".to_string(), Utc::now().timestamp().to_string());

        // Topologically sort steps
        let sorted_steps = Self::topological_sort(&definition.steps)?;

        // Execute each step in order
        for step in sorted_steps {
            let result = self.execute_step(step, &context, &run, executor).await;

            match result {
                Ok(step_result) => {
                    // Store output in variable context
                    if let Some(ref var_name) = step.output_var {
                        if let Some(ref output) = step_result.output {
                            context.insert(var_name.clone(), output.clone());
                        }
                    }

                    self.store.record_step_execution(&run.id, &step_result)?;
                    run.step_results.insert(step.id.clone(), step_result);
                }
                Err(e) => {
                    let error_msg = e.to_string();

                    match &step.on_failure {
                        OnFailure::Fail => {
                            let failed_result = StepResult {
                                step_id: step.id.clone(),
                                status: StepStatus::Failed,
                                output: None,
                                error: Some(error_msg.clone()),
                                started_at: Some(Utc::now()),
                                completed_at: Some(Utc::now()),
                                attempt: 1,
                                duration_ms: Some(0),
                            };
                            self.store.record_step_execution(&run.id, &failed_result)?;
                            run.step_results.insert(step.id.clone(), failed_result);
                            run.status = PipelineRunStatus::Failed;
                            run.error = Some(error_msg);
                            run.completed_at = Some(Utc::now());
                            self.store.update_run(&run)?;
                            return Ok(run);
                        }
                        OnFailure::Skip => {
                            tracing::warn!(step = %step.id, "Step failed, skipping (on_failure=skip)");
                            let skipped_result = StepResult {
                                step_id: step.id.clone(),
                                status: StepStatus::Skipped,
                                output: None,
                                error: Some(error_msg),
                                started_at: Some(Utc::now()),
                                completed_at: Some(Utc::now()),
                                attempt: 1,
                                duration_ms: Some(0),
                            };
                            self.store.record_step_execution(&run.id, &skipped_result)?;
                            run.step_results.insert(step.id.clone(), skipped_result);
                            // Continue to next step
                        }
                        OnFailure::UseDefault => {
                            let default_val = step.default_value.clone().unwrap_or_default();
                            tracing::warn!(
                                step = %step.id,
                                default = %default_val,
                                "Step failed, using default value (on_failure=use_default)"
                            );
                            // Insert the default value into the variable context
                            if let Some(ref var_name) = step.output_var {
                                context.insert(var_name.clone(), default_val.clone());
                            }
                            let default_result = StepResult {
                                step_id: step.id.clone(),
                                status: StepStatus::Complete,
                                output: Some(default_val),
                                error: Some(error_msg),
                                started_at: Some(Utc::now()),
                                completed_at: Some(Utc::now()),
                                attempt: 1,
                                duration_ms: Some(0),
                            };
                            self.store.record_step_execution(&run.id, &default_result)?;
                            run.step_results.insert(step.id.clone(), default_result);
                            // Continue to next step
                        }
                    }
                }
            }
        }

        // Extract final output
        if let Some(ref output_var) = definition.output {
            run.output = context.get(output_var).cloned();
        }

        run.status = PipelineRunStatus::Complete;
        run.completed_at = Some(Utc::now());
        self.store.update_run(&run)?;

        Ok(run)
    }

    /// Execute a single step — either dispatch to an agent or call a tool.
    async fn execute_step(
        &self,
        step: &PipelineStep,
        context: &HashMap<String, String>,
        _run: &PipelineRun,
        executor: &dyn PipelineExecutor,
    ) -> Result<StepResult, AgentOSError> {
        let started_at = Utc::now();
        let max_attempts = step.retry_on_failure.unwrap_or(0) + 1;
        let timeout_duration = step
            .timeout_minutes
            .map(|m| std::time::Duration::from_secs(m * 60));

        let mut last_error = None;

        for attempt in 1..=max_attempts {
            let result = match &step.action {
                StepAction::Agent { agent, task } => {
                    let rendered_task = Self::render_template(task, context);
                    let fut = executor.run_agent_task(agent, &rendered_task);
                    match timeout_duration {
                        Some(dur) => match tokio::time::timeout(dur, fut).await {
                            Ok(r) => r,
                            Err(_) => Err(AgentOSError::KernelError {
                                reason: format!(
                                    "Step '{}' timed out after {} minutes",
                                    step.id,
                                    step.timeout_minutes.unwrap_or(0)
                                ),
                            }),
                        },
                        None => fut.await,
                    }
                }
                StepAction::Tool { tool, input } => {
                    // Render template variables in tool input
                    let input_str = serde_json::to_string(input).unwrap_or_default();
                    let rendered_input_str = Self::render_template(&input_str, context);
                    let rendered_input: serde_json::Value =
                        serde_json::from_str(&rendered_input_str).unwrap_or(input.clone());

                    let fut = executor.run_tool(tool, rendered_input);
                    match timeout_duration {
                        Some(dur) => match tokio::time::timeout(dur, fut).await {
                            Ok(r) => r,
                            Err(_) => Err(AgentOSError::KernelError {
                                reason: format!(
                                    "Step '{}' timed out after {} minutes",
                                    step.id,
                                    step.timeout_minutes.unwrap_or(0)
                                ),
                            }),
                        },
                        None => fut.await,
                    }
                }
            };

            match result {
                Ok(output) => {
                    let completed_at = Utc::now();
                    let duration_ms = (completed_at - started_at).num_milliseconds() as u64;
                    return Ok(StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Complete,
                        output: Some(output),
                        error: None,
                        started_at: Some(started_at),
                        completed_at: Some(completed_at),
                        attempt,
                        duration_ms: Some(duration_ms),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        step = %step.id,
                        attempt = attempt,
                        error = %e,
                        "Step failed"
                    );
                    last_error = Some(e);
                    if attempt < max_attempts {
                        tracing::info!(step = %step.id, "Retrying step (attempt {}/{})", attempt + 1, max_attempts);
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| AgentOSError::KernelError {
            reason: format!("Step '{}' failed with no error details", step.id),
        }))
    }

    /// Resolve all `{{var}}` references in a template string (double-brace syntax).
    /// Unresolved variables are logged and replaced with a visible marker.
    /// Double braces avoid conflicts with JSON `{...}` and natural language.
    pub fn render_template(template: &str, context: &HashMap<String, String>) -> String {
        let re = Regex::new(r"\{\{([a-zA-Z_][a-zA-Z0-9_]*)\}\}").unwrap();
        re.replace_all(template, |caps: &regex::Captures| {
            let var_name = &caps[1];
            context.get(var_name).cloned().unwrap_or_else(|| {
                tracing::warn!(var = var_name, "Unresolved pipeline variable");
                format!("{{{{UNRESOLVED:{var_name}}}}}")
            })
        })
        .into_owned()
    }

    /// Topologically sort steps to respect `depends_on` constraints.
    pub fn topological_sort(steps: &[PipelineStep]) -> Result<Vec<&PipelineStep>, AgentOSError> {
        let step_map: HashMap<&str, &PipelineStep> =
            steps.iter().map(|s| (s.id.as_str(), s)).collect();

        // Validate all depends_on references exist
        for step in steps {
            for dep in &step.depends_on {
                if !step_map.contains_key(dep.as_str()) {
                    return Err(AgentOSError::KernelError {
                        reason: format!("Step '{}' depends on unknown step '{}'", step.id, dep),
                    });
                }
            }
        }

        // Kahn's algorithm
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

        for step in steps {
            in_degree.entry(step.id.as_str()).or_insert(0);
            adj.entry(step.id.as_str()).or_default();
            for dep in &step.depends_on {
                adj.entry(dep.as_str()).or_default().push(step.id.as_str());
                *in_degree.entry(step.id.as_str()).or_insert(0) += 1;
            }
        }

        let mut queue: Vec<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();
        queue.sort(); // deterministic order for steps with same priority

        let mut sorted = Vec::new();
        while let Some(current) = queue.pop() {
            sorted.push(current);
            if let Some(neighbors) = adj.get(current) {
                for &neighbor in neighbors {
                    let deg = in_degree.get_mut(neighbor).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(neighbor);
                        queue.sort();
                    }
                }
            }
        }

        if sorted.len() != steps.len() {
            return Err(AgentOSError::KernelError {
                reason: "Circular dependency detected in pipeline steps".to_string(),
            });
        }

        // Map back to step references
        Ok(sorted
            .into_iter()
            .map(|id| *step_map.get(id).unwrap())
            .collect())
    }

    /// Validate a pipeline definition.
    fn validate(&self, definition: &PipelineDefinition) -> Result<(), AgentOSError> {
        if definition.steps.is_empty() {
            return Err(AgentOSError::KernelError {
                reason: "Pipeline has no steps".to_string(),
            });
        }

        // Check for duplicate step IDs
        let mut seen = std::collections::HashSet::new();
        for step in &definition.steps {
            if !seen.insert(&step.id) {
                return Err(AgentOSError::KernelError {
                    reason: format!("Duplicate step ID: '{}'", step.id),
                });
            }
        }

        // Validate topological sort (checks deps and cycles)
        Self::topological_sort(&definition.steps)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::PipelineStep;
    use std::sync::Mutex;

    fn make_step(id: &str, deps: Vec<&str>) -> PipelineStep {
        PipelineStep {
            id: id.to_string(),
            action: StepAction::Agent {
                agent: "test".to_string(),
                task: "test task".to_string(),
            },
            output_var: None,
            depends_on: deps.into_iter().map(String::from).collect(),
            timeout_minutes: None,
            retry_on_failure: None,
            on_failure: OnFailure::default(),
            default_value: None,
        }
    }

    /// Mock executor that records calls and returns configurable results.
    struct MockExecutor {
        agent_calls: Mutex<Vec<(String, String)>>,
        tool_calls: Mutex<Vec<(String, serde_json::Value)>>,
        agent_response: Box<dyn Fn(&str, &str) -> Result<String, AgentOSError> + Send + Sync>,
        tool_response:
            Box<dyn Fn(&str, &serde_json::Value) -> Result<String, AgentOSError> + Send + Sync>,
    }

    impl MockExecutor {
        fn new() -> Self {
            Self {
                agent_calls: Mutex::new(Vec::new()),
                tool_calls: Mutex::new(Vec::new()),
                agent_response: Box::new(|agent, prompt| {
                    Ok(format!("[{agent} processed: {prompt}]"))
                }),
                tool_response: Box::new(|tool, input| {
                    Ok(format!("[{tool} executed with: {{input}}]"))
                }),
            }
        }

        fn with_agent_response<F>(mut self, f: F) -> Self
        where
            F: Fn(&str, &str) -> Result<String, AgentOSError> + Send + Sync + 'static,
        {
            self.agent_response = Box::new(f);
            self
        }

        fn with_tool_response<F>(mut self, f: F) -> Self
        where
            F: Fn(&str, &serde_json::Value) -> Result<String, AgentOSError> + Send + Sync + 'static,
        {
            self.tool_response = Box::new(f);
            self
        }
    }

    #[async_trait::async_trait]
    impl PipelineExecutor for MockExecutor {
        async fn run_agent_task(
            &self,
            agent_name: &str,
            prompt: &str,
        ) -> Result<String, AgentOSError> {
            self.agent_calls
                .lock()
                .unwrap()
                .push((agent_name.to_string(), prompt.to_string()));
            (self.agent_response)(agent_name, prompt)
        }

        async fn run_tool(
            &self,
            tool_name: &str,
            input: serde_json::Value,
        ) -> Result<String, AgentOSError> {
            self.tool_calls
                .lock()
                .unwrap()
                .push((tool_name.to_string(), input.clone()));
            (self.tool_response)(tool_name, &input)
        }
    }

    fn test_engine() -> (PipelineEngine, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(PipelineStore::open(&dir.path().join("test.db")).unwrap());
        (PipelineEngine::new(store), dir)
    }

    /// Install a pipeline definition in the store (required for FK constraint on runs).
    fn install_def(engine: &PipelineEngine, yaml: &str) {
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        engine
            .store()
            .install_pipeline(&def.name, &def.version, yaml)
            .unwrap();
    }

    #[test]
    fn test_topological_sort_respects_deps() {
        let steps = vec![
            make_step("c", vec!["b"]),
            make_step("a", vec![]),
            make_step("b", vec!["a"]),
        ];
        let sorted = PipelineEngine::topological_sort(&steps).unwrap();
        assert_eq!(sorted[0].id, "a");
        assert_eq!(sorted[1].id, "b");
        assert_eq!(sorted[2].id, "c");
    }

    #[test]
    fn test_circular_dependency_rejected() {
        let steps = vec![make_step("a", vec!["b"]), make_step("b", vec!["a"])];
        let result = PipelineEngine::topological_sort(&steps);
        assert!(result.is_err());
    }

    #[test]
    fn test_unknown_dep_rejected() {
        let steps = vec![make_step("a", vec!["nonexistent"])];
        let result = PipelineEngine::topological_sort(&steps);
        assert!(result.is_err());
    }

    #[test]
    fn test_template_rendering() {
        let ctx = HashMap::from([
            ("input".to_string(), "quantum computing".to_string()),
            ("raw_research".to_string(), "Some research text".to_string()),
        ]);
        let result =
            PipelineEngine::render_template("Research about {{input}}: {{raw_research}}", &ctx);
        assert_eq!(
            result,
            "Research about quantum computing: Some research text"
        );
    }

    #[test]
    fn test_unresolved_variables_left_as_is() {
        let ctx = HashMap::from([("input".to_string(), "test".to_string())]);
        // Single braces pass through unchanged (not treated as variables)
        let result = PipelineEngine::render_template("{{input}} and {single_brace}", &ctx);
        assert_eq!(result, "test and {single_brace}");

        // Double-brace unresolved variables get a marker
        let result2 = PipelineEngine::render_template("{{input}} and {{unknown}}", &ctx);
        assert_eq!(result2, "test and {{UNRESOLVED:unknown}}");
    }

    #[test]
    fn test_pipeline_yaml_parses() {
        let yaml = r#"
name: "test-pipeline"
version: "1.0.0"
description: "A test pipeline"
permissions:
  - "network.outbound:x"
steps:
  - id: research
    agent: researcher
    task: "Search for: {{input}}"
    output_var: raw_research
    timeout_minutes: 10
  - id: analyse
    agent: analyst
    task: "Analyse: {{raw_research}}"
    output_var: analysis
    depends_on: [research]
  - id: save
    tool: file-writer
    input:
      path: "/output/report-{{run_id}}.md"
      content: "{{analysis}}"
    depends_on: [analyse]
output: analysis
"#;
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        assert_eq!(def.steps.len(), 3);
        assert_eq!(def.steps[0].id, "research");
        assert_eq!(def.name, "test-pipeline");
        assert_eq!(def.output, Some("analysis".to_string()));
    }

    // --- End-to-end pipeline execution tests ---

    #[tokio::test]
    async fn test_run_simple_agent_pipeline() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new();

        let yaml = r#"
name: "simple-pipeline"
version: "1.0.0"
steps:
  - id: step1
    agent: researcher
    task: "Research: {{input}}"
    output_var: research_result
output: research_result
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run_id = RunID::new();

        let run = engine.run(&def, "quantum computing", run_id, &executor).await.unwrap();

        assert_eq!(run.status, PipelineRunStatus::Complete);
        assert!(run.output.is_some());
        assert!(run.output.as_ref().unwrap().contains("researcher processed"));
        assert!(run.output.as_ref().unwrap().contains("Research: quantum computing"));
        assert!(run.completed_at.is_some());
        assert!(run.error.is_none());

        // Verify the executor was called correctly
        let calls = executor.agent_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "researcher");
        assert_eq!(calls[0].1, "Research: quantum computing");
    }

    #[tokio::test]
    async fn test_run_multi_step_pipeline_with_variable_passing() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new().with_agent_response(|agent, _prompt| {
            match agent {
                "researcher" => Ok("Raw research data about quantum computing".to_string()),
                "analyst" => Ok("Key finding: quantum supremacy achieved".to_string()),
                "summarizer" => Ok("Executive summary: quantum computing has reached a milestone".to_string()),
                _ => Ok("unknown agent".to_string()),
            }
        });

        let yaml = r#"
name: "multi-step"
version: "1.0.0"
steps:
  - id: research
    agent: researcher
    task: "Research: {{input}}"
    output_var: raw_research
  - id: analyse
    agent: analyst
    task: "Analyse: {{raw_research}}"
    output_var: analysis
    depends_on: [research]
  - id: summarise
    agent: summarizer
    task: "Summarise: {{analysis}}"
    output_var: summary
    depends_on: [analyse]
output: summary
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run = engine.run(&def, "quantum computing", RunID::new(), &executor).await.unwrap();

        assert_eq!(run.status, PipelineRunStatus::Complete);
        assert_eq!(
            run.output.as_deref(),
            Some("Executive summary: quantum computing has reached a milestone")
        );
        assert_eq!(run.step_results.len(), 3);

        // Verify variable passing: analyst should have received researcher's output
        let calls = executor.agent_calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls[1].1.contains("Raw research data about quantum computing"));
        assert!(calls[2].1.contains("Key finding: quantum supremacy achieved"));
    }

    #[tokio::test]
    async fn test_run_pipeline_with_tool_step() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new()
            .with_agent_response(|_, _| Ok("Generated report content".to_string()))
            .with_tool_response(|_tool, input| {
                Ok(format!("Saved to {}", input.get("path").and_then(|v| v.as_str()).unwrap_or("?")))
            });

        let yaml = r#"
name: "with-tool"
version: "1.0.0"
steps:
  - id: generate
    agent: writer
    task: "Write report about: {{input}}"
    output_var: report
  - id: save
    tool: file-writer
    input:
      path: "/output/report-{{run_id}}.md"
      content: "{{report}}"
    depends_on: [generate]
    output_var: save_result
output: save_result
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run = engine.run(&def, "AI trends", RunID::new(), &executor).await.unwrap();

        assert_eq!(run.status, PipelineRunStatus::Complete);

        // Verify tool was called with rendered variables
        let tool_calls = executor.tool_calls.lock().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].0, "file-writer");
        let tool_input = &tool_calls[0].1;
        assert_eq!(
            tool_input.get("content").and_then(|v| v.as_str()),
            Some("Generated report content")
        );
    }

    #[tokio::test]
    async fn test_pipeline_step_failure_stops_execution() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new().with_agent_response(|agent, _| {
            if agent == "failing-agent" {
                Err(AgentOSError::KernelError {
                    reason: "Agent crashed".to_string(),
                })
            } else {
                Ok("success".to_string())
            }
        });

        let yaml = r#"
name: "failing-pipeline"
version: "1.0.0"
steps:
  - id: step1
    agent: good-agent
    task: "Do something"
    output_var: result1
  - id: step2
    agent: failing-agent
    task: "This will fail"
    output_var: result2
    depends_on: [step1]
  - id: step3
    agent: good-agent
    task: "This should not run"
    output_var: result3
    depends_on: [step2]
output: result3
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run = engine.run(&def, "test", RunID::new(), &executor).await.unwrap();

        assert_eq!(run.status, PipelineRunStatus::Failed);
        assert!(run.error.is_some());
        assert!(run.error.as_ref().unwrap().contains("Agent crashed"));
        // step3 should NOT have been executed
        assert!(!run.step_results.contains_key("step3"));
        // step1 and step2 should be in results
        assert_eq!(run.step_results["step1"].status, StepStatus::Complete);
        assert_eq!(run.step_results["step2"].status, StepStatus::Failed);
    }

    #[tokio::test]
    async fn test_pipeline_retry_on_failure() {
        let (engine, _dir) = test_engine();
        let call_count = Arc::new(Mutex::new(0u32));
        let count_clone = call_count.clone();

        let executor = MockExecutor::new().with_agent_response(move |_, _| {
            let mut count = count_clone.lock().unwrap();
            *count += 1;
            if *count < 3 {
                Err(AgentOSError::KernelError {
                    reason: format!("Transient error (attempt {})", count),
                })
            } else {
                Ok("Success on retry".to_string())
            }
        });

        let yaml = r#"
name: "retry-pipeline"
version: "1.0.0"
steps:
  - id: flaky
    agent: flaky-agent
    task: "Do flaky thing"
    output_var: result
    retry_on_failure: 3
output: result
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run = engine.run(&def, "test", RunID::new(), &executor).await.unwrap();

        assert_eq!(run.status, PipelineRunStatus::Complete);
        assert_eq!(run.output.as_deref(), Some("Success on retry"));
        assert_eq!(run.step_results["flaky"].attempt, 3);
    }

    #[tokio::test]
    async fn test_pipeline_retry_exhausted() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new().with_agent_response(|_, _| {
            Err(AgentOSError::KernelError {
                reason: "Always fails".to_string(),
            })
        });

        let yaml = r#"
name: "always-fails"
version: "1.0.0"
steps:
  - id: doomed
    agent: bad-agent
    task: "Will always fail"
    output_var: result
    retry_on_failure: 2
output: result
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run = engine.run(&def, "test", RunID::new(), &executor).await.unwrap();

        assert_eq!(run.status, PipelineRunStatus::Failed);
        assert!(run.error.as_ref().unwrap().contains("Always fails"));
    }

    #[tokio::test]
    async fn test_pipeline_run_persisted_to_store() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new();

        let yaml = r#"
name: "persist-test"
version: "1.0.0"
steps:
  - id: step1
    agent: test-agent
    task: "Do: {{input}}"
    output_var: result
output: result
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run_id = RunID::new();

        let run = engine.run(&def, "hello", run_id, &executor).await.unwrap();
        assert_eq!(run.status, PipelineRunStatus::Complete);

        // Verify run was persisted
        let stored_run = engine.store().get_run(&run_id).unwrap();
        assert_eq!(stored_run.status, PipelineRunStatus::Complete);
        assert_eq!(stored_run.pipeline_name, "persist-test");
        assert_eq!(stored_run.input, "hello");
        assert!(stored_run.output.is_some());

        // Verify step logs were persisted
        let step_logs = engine.store().get_step_logs(&run_id, "step1").unwrap();
        assert_eq!(step_logs.len(), 1);
        assert_eq!(step_logs[0].status, StepStatus::Complete);
    }

    #[tokio::test]
    async fn test_empty_pipeline_rejected() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new();

        let yaml = r#"
name: "empty"
version: "1.0.0"
steps: []
"#;
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let result = engine.run(&def, "test", RunID::new(), &executor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_duplicate_step_ids_rejected() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new();

        let yaml = r#"
name: "dupes"
version: "1.0.0"
steps:
  - id: step1
    agent: a
    task: "t"
  - id: step1
    agent: b
    task: "t"
"#;
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let result = engine.run(&def, "test", RunID::new(), &executor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_builtin_variables_available() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new().with_agent_response(|_, prompt| {
            Ok(prompt.to_string())
        });

        let yaml = r#"
name: "builtins"
version: "1.0.0"
steps:
  - id: check
    agent: test
    task: "input={{input}} date={{date}} ts={{timestamp}} rid={{run_id}}"
    output_var: result
output: result
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run = engine.run(&def, "hello", RunID::new(), &executor).await.unwrap();

        let output = run.output.unwrap();
        assert!(output.contains("input=hello"));
        assert!(output.contains("date="));
        assert!(output.contains("ts="));
        assert!(output.contains("rid="));
        // Ensure variables were actually resolved (not left as {var})
        assert!(!output.contains("{{input}}"));
        assert!(!output.contains("{{date}}"));
        assert!(!output.contains("{{timestamp}}"));
        assert!(!output.contains("{{run_id}}"));
    }

    #[tokio::test]
    async fn test_on_failure_skip_continues_pipeline() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new().with_agent_response(|agent, _| {
            if agent == "failing-agent" {
                Err(AgentOSError::KernelError {
                    reason: "Step failed".to_string(),
                })
            } else {
                Ok("success".to_string())
            }
        });

        let yaml = r#"
name: "skip-on-fail"
version: "1.0.0"
steps:
  - id: step1
    agent: good-agent
    task: "Do step 1"
    output_var: result1
  - id: step2
    agent: failing-agent
    task: "This will fail"
    output_var: result2
    depends_on: [step1]
    on_failure: skip
  - id: step3
    agent: good-agent
    task: "This should still run"
    output_var: result3
    depends_on: [step2]
output: result3
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run = engine.run(&def, "test", RunID::new(), &executor).await.unwrap();

        assert_eq!(run.status, PipelineRunStatus::Complete);
        assert_eq!(run.step_results["step1"].status, StepStatus::Complete);
        assert_eq!(run.step_results["step2"].status, StepStatus::Skipped);
        assert_eq!(run.step_results["step3"].status, StepStatus::Complete);
    }

    #[tokio::test]
    async fn test_on_failure_use_default_provides_value() {
        let (engine, _dir) = test_engine();
        let executor = MockExecutor::new().with_agent_response(|agent, prompt| {
            if agent == "failing-agent" {
                Err(AgentOSError::KernelError {
                    reason: "Step failed".to_string(),
                })
            } else {
                Ok(prompt.to_string())
            }
        });

        let yaml = r#"
name: "default-on-fail"
version: "1.0.0"
steps:
  - id: step1
    agent: failing-agent
    task: "This will fail"
    output_var: result1
    on_failure: use_default
    default_value: "fallback value"
  - id: step2
    agent: good-agent
    task: "Using: {{result1}}"
    output_var: result2
    depends_on: [step1]
output: result2
"#;
        install_def(&engine, yaml);
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        let run = engine.run(&def, "test", RunID::new(), &executor).await.unwrap();

        assert_eq!(run.status, PipelineRunStatus::Complete);
        // step2 should have received the default value from step1
        let output = run.output.as_ref().unwrap();
        assert!(output.contains("fallback value"), "Output was: {}", output);
    }
}
