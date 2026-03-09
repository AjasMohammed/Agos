use crate::definition::{PipelineDefinition, PipelineStep, StepAction};
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
                    let failed_result = StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: None,
                        error: Some(e.to_string()),
                        started_at: Some(Utc::now()),
                        completed_at: Some(Utc::now()),
                        attempt: 1,
                        duration_ms: Some(0),
                    };
                    self.store.record_step_execution(&run.id, &failed_result)?;
                    run.step_results.insert(step.id.clone(), failed_result);
                    run.status = PipelineRunStatus::Failed;
                    run.error = Some(e.to_string());
                    run.completed_at = Some(Utc::now());
                    self.store.update_run(&run)?;
                    return Ok(run);
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

    /// Resolve all `{var}` references in a template string.
    /// Unresolved variables are left as-is so that errors are visible in logs.
    pub fn render_template(template: &str, context: &HashMap<String, String>) -> String {
        let re = Regex::new(r"\{([a-zA-Z_][a-zA-Z0-9_]*)\}").unwrap();
        re.replace_all(template, |caps: &regex::Captures| {
            let var_name = &caps[1];
            context
                .get(var_name)
                .cloned()
                .unwrap_or_else(|| format!("{{{var_name}}}"))
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
        }
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
            PipelineEngine::render_template("Research about {input}: {raw_research}", &ctx);
        assert_eq!(
            result,
            "Research about quantum computing: Some research text"
        );
    }

    #[test]
    fn test_unresolved_variables_left_as_is() {
        let ctx = HashMap::from([("input".to_string(), "test".to_string())]);
        let result = PipelineEngine::render_template("{input} and {unknown}", &ctx);
        assert_eq!(result, "test and {unknown}");
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
    task: "Search for: {input}"
    output_var: raw_research
    timeout_minutes: 10
  - id: analyse
    agent: analyst
    task: "Analyse: {raw_research}"
    output_var: analysis
    depends_on: [research]
  - id: save
    tool: file-writer
    input:
      path: "/output/report-{run_id}.md"
      content: "{analysis}"
    depends_on: [analyse]
output: analysis
"#;
        let def = PipelineDefinition::from_yaml(yaml).unwrap();
        assert_eq!(def.steps.len(), 3);
        assert_eq!(def.steps[0].id, "research");
        assert_eq!(def.name, "test-pipeline");
        assert_eq!(def.output, Some("analysis".to_string()));
    }
}
