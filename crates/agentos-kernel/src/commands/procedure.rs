//! Running a stored executable procedure.
//!
//! Resolve the recipe, re-validate it, compile it, bind the caller's inputs and
//! hand it to the pipeline engine — as the calling agent, so every step is
//! gated against that agent's own `PermissionSet` by the machinery the operator
//! pipeline path already uses.
//!
//! Nothing here widens authority. `procedure-run` grants `memory.procedural:r`,
//! which is permission to *read* the recipe; each step's capability token is
//! minted from the agent's own grants, so an ambitious procedure simply fails
//! the step it is not entitled to.

use crate::kernel::Kernel;
use crate::kernel_action::KernelActionResult;
use crate::procedure_compile::{bind_inputs, compile, ProcedureLimits};
use agentos_memory::Procedure;
use agentos_types::{AgentTask, TraceID};
use std::time::Duration;

fn failure(error: impl std::fmt::Display) -> KernelActionResult {
    KernelActionResult {
        success: false,
        result: serde_json::json!({ "error": error.to_string() }),
    }
}

impl Kernel {
    pub(crate) async fn execute_run_procedure(
        &self,
        task: &AgentTask,
        name: &str,
        inputs: &serde_json::Value,
        detach: bool,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let agent_id = task.agent_id;

        // Own procedures shadow global ones, and an ambiguous name is refused
        // rather than guessed — running the wrong recipe silently is worse than
        // making the caller disambiguate.
        let procedure = match self.resolve_procedure(name, &agent_id).await {
            Ok(procedure) => procedure,
            Err(result) => return result,
        };

        // The curator archives a recipe that has gone stale, and
        // `procedure-search` already hides those — but `find_by_name` sees them
        // (the upsert path needs it), so without this an agent could still run
        // a recipe it can no longer find.
        if procedure.status != agentos_memory::MemoryStatus::Active {
            return failure(format!(
                "procedure '{name}' is {:?}, not active — it was archived or marked stale. \
                 Rewrite it with procedure-create to bring it back.",
                procedure.status
            ));
        }

        // The store is NOT a trust boundary. This row may predate the authoring
        // validator, or have been written by another path, so every rule is
        // re-checked here against what will actually execute.
        if let Err(e) =
            agentos_tools::procedure_create::validate_recipe(&procedure.steps, &procedure.inputs)
        {
            return failure(format!("procedure '{name}' is not runnable: {e}"));
        }

        // Tool existence is checked here rather than at authoring, because this
        // is the first place with a tool registry.
        {
            let registry = self.tool_registry.read().await;
            for step in &procedure.steps {
                let Some(tool) = step.tool.as_deref() else {
                    continue;
                };
                // Normalised, because dispatch resolves `file_reader` to
                // `file-reader`; matching the raw name here would reject a step
                // that would in fact have run.
                let resolved = self.tool_runner.resolve_tool_name(tool);
                let resolved = resolved.as_deref().unwrap_or(tool);
                if registry.get_by_name(resolved).is_none() {
                    return failure(format!(
                        "procedure '{name}' step[{}] calls '{tool}', which is not an installed tool",
                        step.order
                    ));
                }
            }
        }

        let limits = ProcedureLimits {
            step_timeout_minutes: self.config.procedures.step_timeout_minutes,
            max_cost_usd: self.config.procedures.max_cost_usd,
            max_wall_time_minutes: self.config.procedures.max_wall_time_minutes,
            max_input_bytes: self.config.procedures.max_input_bytes,
        };

        let definition = match compile(&procedure, &limits) {
            Ok(definition) => definition,
            Err(e) => return failure(e),
        };
        let bindings = match bind_inputs(&procedure, inputs, &limits) {
            Ok(bindings) => bindings,
            Err(e) => return failure(e),
        };

        // `pipeline_runs.pipeline_name` is a foreign key into `pipelines`, so
        // the compiled definition has to exist there before the engine can
        // record a run. Without this every successful procedure run died on
        // "FOREIGN KEY constraint failed" — which only the happy path reaches,
        // so no unit test could have found it.
        let yaml = match definition.to_yaml() {
            Ok(yaml) => yaml,
            Err(e) => return failure(format!("could not serialize '{name}': {e}")),
        };
        if let Err(e) = self.pipeline_engine.store().install_pipeline(
            &definition.name,
            &definition.version,
            &yaml,
        ) {
            return failure(format!("could not register '{name}' for execution: {e}"));
        }

        let run_id = agentos_types::RunID::new();
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::IntentReceived,
            agent_id: Some(agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "action": "pipeline_run_started",
                "source": "procedure",
                "procedure_id": procedure.id,
                "pipeline_name": definition.name,
                "run_id": run_id.to_string(),
                "steps": definition.steps.len(),
                "detach": detach,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        // The agent identity is the security boundary: it decides which
        // PermissionSet every step's token is minted from. It comes from the
        // authenticated task, never from the payload.
        // Clamped to the CALLING TASK's token, not just the agent's registry
        // grants. A sub-agent spawned with a narrowed token must not be able to
        // widen itself back out by running a recipe.
        let executor = self
            .owned_pipeline_executor(agent_id)
            .clamped_to(task.capability_token.permissions.clone());
        let engine = self.pipeline_engine.clone();

        if detach {
            // Registered with the background pool, like a detached operator
            // pipeline run, so `agentos bg list` and the panel see it and the
            // agent can be told what happened after the turn ends.
            let bg_pool = self.background_pool.clone();
            let bg_task_id = agentos_types::TaskID::new();
            let agent_name = {
                let registry = self.agent_registry.read().await;
                registry
                    .get_by_id(&agent_id)
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| "procedure".to_string())
            };
            bg_pool
                .register(agentos_types::BackgroundTask {
                    id: bg_task_id,
                    name: format!("procedure:{}", definition.name),
                    agent_name,
                    task_prompt: format!("Run procedure '{}'", definition.name),
                    state: agentos_types::TaskState::Running,
                    started_at: Some(chrono::Utc::now()),
                    completed_at: None,
                    result: None,
                    detached: true,
                    scheduled_job_id: None,
                })
                .await;

            tokio::spawn(async move {
                match engine
                    .run_with_bindings(&definition, "", run_id, &executor, Some(bindings))
                    .await
                {
                    // A step that failed returns Ok with `status: Failed`;
                    // only a VALIDATION failure returns Err. Marking that
                    // complete showed a green run in `bg list` for a procedure
                    // that did not do its job.
                    Ok(run) if run.status == agentos_pipeline::PipelineRunStatus::Complete => {
                        bg_pool
                            .complete(&bg_task_id, serde_json::to_value(&run).unwrap_or_default())
                            .await;
                    }
                    Ok(run) => {
                        bg_pool
                            .fail(
                                &bg_task_id,
                                run.error
                                    .clone()
                                    .unwrap_or_else(|| format!("procedure {}", run.status)),
                            )
                            .await;
                    }
                    Err(e) => bg_pool.fail(&bg_task_id, e.to_string()).await,
                }
            });

            return KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "started": true,
                    "run_id": run_id.to_string(),
                    "procedure": name,
                    "detached": true,
                    "background_task_id": bg_task_id.to_string(),
                }),
            };
        }

        // `max_wall_time_minutes` is documented as a ceiling on the run, and
        // nothing in the engine reads it. Without this a 32-step recipe at the
        // 5-minute per-step cap could park the agent's turn for 160 minutes.
        let budget = Duration::from_secs(
            self.config
                .procedures
                .max_wall_time_minutes
                .unwrap_or(30)
                .saturating_mul(60),
        );
        let outcome = tokio::time::timeout(
            budget,
            engine.run_with_bindings(&definition, "", run_id, &executor, Some(bindings)),
        )
        .await;
        let Ok(outcome) = outcome else {
            return failure(format!(
                "procedure '{name}' exceeded the {} minute run budget ([procedures].max_wall_time_minutes)",
                budget.as_secs() / 60
            ));
        };
        match outcome {
            Ok(run) => {
                let failed_step = run
                    .step_results
                    .values()
                    .find(|s| s.status == agentos_pipeline::StepStatus::Failed);
                KernelActionResult {
                    success: run.status == agentos_pipeline::PipelineRunStatus::Complete,
                    result: serde_json::json!({
                        "run_id": run.id.to_string(),
                        "procedure": name,
                        "status": run.status.to_string(),
                        "steps_run": run.step_results.len(),
                        "output": run.output,
                        "error": run.error,
                        "failed_step": failed_step.map(|s| s.step_id.clone()),
                    }),
                }
            }
            Err(e) => failure(e),
        }
    }

    /// Find `name` among the agent's own procedures, then the global ones.
    ///
    /// An agent's own recipe shadows a global one of the same name — it is the
    /// more specific answer, and it is the one the agent wrote.
    async fn resolve_procedure(
        &self,
        name: &str,
        agent_id: &agentos_types::AgentID,
    ) -> Result<Procedure, KernelActionResult> {
        for owner in [Some(agent_id), None] {
            match self.procedural_memory.find_by_name(name, owner).await {
                Ok(Some(procedure)) => return Ok(procedure),
                Ok(None) => {}
                Err(e) => return Err(failure(e)),
            }
        }
        Err(failure(format!(
            "no procedure named '{name}' is available to this agent — list them with \
             procedure-list, or write one with procedure-create"
        )))
    }

    /// The detached executor the operator pipeline path builds, for one agent.
    ///
    /// Factored out so the procedure path cannot drift from it: every field
    /// here is a security input, and a second hand-written copy is how one of
    /// them ends up missing.
    pub(crate) fn owned_pipeline_executor(
        &self,
        agent_id: agentos_types::AgentID,
    ) -> crate::commands::pipeline::OwnedPipelineExecutor {
        let workspace = self.workspace_paths_for_agent(&agent_id);
        crate::commands::pipeline::OwnedPipelineExecutor {
            agent_registry: self.agent_registry.clone(),
            active_llms: self.active_llms.clone(),
            tool_runner: self.tool_runner.clone(),
            vault: self.vault.clone(),
            hal: self.hal.clone(),
            data_dir: self.data_dir.clone(),
            workspace_paths: workspace.read,
            workspace_paths_writable: workspace.writable,
            workspace_paths_executable: workspace.executable,
            context_manager: self.context_manager.clone(),
            cost_tracker: self.cost_tracker.clone(),
            agent_id,
            // A procedure compiles to tool steps only (`procedure_compile.rs`),
            // so `resolve_step_agent` is never reached from this path — a
            // property of the compiler, not an assumption about recipe authors.
            agent_name: None,
            capability_engine: self.capability_engine.clone(),
            tool_registry: self.tool_registry.clone(),
            token_ttl: Duration::from_secs(
                self.config.kernel.tool_execution.default_timeout_seconds,
            ),
            injection_scanner: self.injection_scanner.clone(),
            event_sender: self.event_sender.clone(),
            audit: self.audit.clone(),
            hook_registry: self.hook_registry.clone(),
            escalation_manager: self.escalation_manager.clone(),
            zone_table: self.zone_table.clone(),
            cancellation_token: self.cancellation_token.child_token(),
            permission_ceiling: None,
        }
    }
}

impl crate::commands::pipeline::OwnedPipelineExecutor {
    /// Bound every step to `ceiling` on top of the agent's registry grants.
    fn clamped_to(mut self, ceiling: agentos_types::PermissionSet) -> Self {
        self.permission_ceiling = Some(ceiling);
        self
    }
}
