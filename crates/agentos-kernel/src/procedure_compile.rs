//! Compile a stored [`Procedure`] into a [`PipelineDefinition`] the pipeline
//! engine can run, and bind the caller's inputs for it.
//!
//! This is the whole of "executable procedures" that is pure logic: no kernel,
//! no store, no I/O. The gating, the agent identity and the execution live in
//! `commands::procedure`, which calls this first.
//!
//! It lives in the kernel rather than in `agentos-pipeline` because the
//! `Procedure` type belongs to `agentos-memory`, and `agentos-pipeline` is a
//! lean crate that has no business pulling in an embedding model to learn what
//! a procedure looks like. The kernel already depends on both.

use agentos_memory::{Procedure, ProcedureStep};
use agentos_pipeline::bindings::Bindings;
use agentos_pipeline::definition::{OnFailure, PipelineStep, StepAction};
use agentos_pipeline::PipelineDefinition;
use agentos_types::AgentOSError;

/// Run-time bounds a compiled procedure inherits, from `[procedures]` config.
#[derive(Debug, Clone)]
pub(crate) struct ProcedureLimits {
    pub step_timeout_minutes: u64,
    pub max_cost_usd: Option<f64>,
    pub max_wall_time_minutes: Option<u64>,
    /// Largest bound-input payload, matching the schedule path's args cap.
    pub max_input_bytes: usize,
}

fn invalid(reason: impl Into<String>) -> AgentOSError {
    AgentOSError::SchemaValidation(reason.into())
}

/// Compile `procedure` into a runnable pipeline.
///
/// Steps are chained in `order`: step N depends on step N-1. A real DAG would
/// need the author to declare dependencies, and nothing asks for that yet —
/// `ProcedureStep` is an ordered list, and inferring parallelism from which
/// variables a step happens to read would silently reorder side effects.
pub(crate) fn compile(
    procedure: &Procedure,
    limits: &ProcedureLimits,
) -> Result<PipelineDefinition, AgentOSError> {
    if !procedure.is_executable() {
        let detail = match procedure.first_prose_step() {
            Some(step) => format!(
                "step[{}] ('{}') has no tool and input payload",
                step.order, step.action
            ),
            None => "it has no steps".to_string(),
        };
        return Err(invalid(format!(
            "procedure '{}' is a prose SOP, not a runnable recipe: {detail}. \
             Rewrite it with procedure-create, giving every step a 'tool' and an 'input'.",
            procedure.name
        )));
    }

    // `order` is assigned from the array index by procedure-create, but a row
    // could have been written by another path, and the compiler sorts by it.
    // Sorting a duplicated or sparse `order` would silently change which step
    // is "earlier" — and therefore turn a binding the author validated as
    // backward into a forward one.
    let mut ordered: Vec<&ProcedureStep> = procedure.steps.iter().collect();
    ordered.sort_by_key(|step| step.order);
    for (position, step) in ordered.iter().enumerate() {
        if step.order != position {
            return Err(invalid(format!(
                "procedure '{}' has a malformed step order at position {position} \
                 (step.order = {}); expected a dense 0..n sequence",
                procedure.name, step.order
            )));
        }
    }

    let steps: Vec<PipelineStep> = ordered
        .iter()
        .map(|step| {
            let (Some(tool), Some(input)) = (step.tool.clone(), step.input.clone()) else {
                // `is_executable` already proved both are present.
                unreachable!("is_executable guarantees tool and input")
            };
            PipelineStep {
                id: step_id(step.order),
                action: StepAction::Tool { tool, input },
                output_var: step.output_var.clone(),
                depends_on: if step.order == 0 {
                    Vec::new()
                } else {
                    vec![step_id(step.order - 1)]
                },
                timeout_minutes: Some(limits.step_timeout_minutes),
                retry_on_failure: None,
                retry_backoff_ms: None,
                retry_max_delay_ms: None,
                // A recipe is a sequence its author expects to complete.
                // Skipping a step yields a result that looks fine and is not.
                on_failure: OnFailure::Fail,
                default_value: None,
            }
        })
        .collect();

    Ok(PipelineDefinition {
        name: pipeline_name(procedure),
        version: procedure.updated_at.to_rfc3339(),
        description: Some(procedure.description.clone()),
        permissions: Vec::new(),
        output: steps.last().and_then(|s| s.output_var.clone()),
        steps,
        max_cost_usd: limits.max_cost_usd,
        max_wall_time_minutes: limits.max_wall_time_minutes,
    })
}

fn step_id(order: usize) -> String {
    format!("s{order}")
}

/// The name a compiled procedure is registered and recorded under.
///
/// Namespaced for two reasons. `pipeline_runs.pipeline_name` is a foreign key
/// into `pipelines(name)`, so the compiled definition has to be installed
/// before a run can be recorded at all — and installing it under the bare
/// procedure name would let one agent's recipe collide with an operator's
/// pipeline, or with another agent's recipe of the same name, and
/// `install_pipeline` is INSERT OR REPLACE.
///
/// The agent is always shown the bare name; this spelling only appears in the
/// pipeline store.
pub(crate) fn pipeline_name(procedure: &Procedure) -> String {
    match procedure.agent_id {
        Some(agent_id) => format!("procedure/{agent_id}/{}", procedure.name),
        None => format!("procedure//{}", procedure.name),
    }
}

/// Bind the caller's arguments to the procedure's declared inputs.
///
/// Everything lands under one `inputs` root, so a step references
/// `{{inputs.text}}`. Keeping them namespaced is what stops a caller-supplied
/// argument from shadowing a step's `output_var` — which would let an input
/// decide what a later step sees instead of the step before it.
pub(crate) fn bind_inputs(
    procedure: &Procedure,
    supplied: &serde_json::Value,
    limits: &ProcedureLimits,
) -> Result<Bindings, AgentOSError> {
    let supplied = match supplied {
        serde_json::Value::Null => serde_json::Map::new(),
        serde_json::Value::Object(map) => map.clone(),
        _ => return Err(invalid("'inputs' must be an object")),
    };

    let size = serde_json::to_string(&supplied)
        .map(|s| s.len())
        .unwrap_or(0);
    if size > limits.max_input_bytes {
        return Err(invalid(format!(
            "inputs are {size} bytes; the limit is {}",
            limits.max_input_bytes
        )));
    }

    // A name the procedure does not declare is a typo, not a no-op. Ignoring it
    // means the run proceeds with a default the caller thought they overrode.
    for name in supplied.keys() {
        if !procedure.inputs.iter().any(|i| &i.name == name) {
            let declared: Vec<&str> = procedure.inputs.iter().map(|i| i.name.as_str()).collect();
            return Err(invalid(format!(
                "procedure '{}' does not declare an input named '{name}' (it declares {declared:?})",
                procedure.name
            )));
        }
    }

    let mut values = serde_json::Map::new();
    for declared in &procedure.inputs {
        match supplied
            .get(&declared.name)
            .cloned()
            .or_else(|| declared.default.clone())
        {
            Some(value) => {
                values.insert(declared.name.clone(), value);
            }
            None if declared.required => {
                return Err(invalid(format!(
                    "procedure '{}' requires input '{}'",
                    procedure.name, declared.name
                )));
            }
            // Optional and unsupplied: left unbound, so a step that references
            // it gets the visible UNRESOLVED marker rather than a silent null.
            None => {}
        }
    }

    Ok(Bindings::from([(
        "inputs".to_string(),
        serde_json::Value::Object(values),
    )]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_memory::{MemoryStatus, ProcedureInput};
    use serde_json::json;

    fn limits() -> ProcedureLimits {
        ProcedureLimits {
            step_timeout_minutes: 5,
            max_cost_usd: Some(1.0),
            max_wall_time_minutes: Some(10),
            max_input_bytes: 16 * 1024,
        }
    }

    fn procedure(steps: Vec<ProcedureStep>, inputs: Vec<ProcedureInput>) -> Procedure {
        Procedure {
            id: "p1".into(),
            name: "speak-aloud".into(),
            description: "say a line".into(),
            preconditions: vec![],
            steps,
            postconditions: vec![],
            success_count: 0,
            failure_count: 0,
            source_episodes: vec![],
            agent_id: None,
            tags: vec![],
            inputs,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_used_at: None,
            use_count: 0,
            confidence: 0.6,
            status: MemoryStatus::Active,
        }
    }

    fn tool_step(
        order: usize,
        tool: &str,
        input: serde_json::Value,
        var: Option<&str>,
    ) -> ProcedureStep {
        ProcedureStep {
            order,
            action: format!("step {order}"),
            tool: Some(tool.into()),
            expected_outcome: None,
            input: Some(input),
            output_var: var.map(String::from),
        }
    }

    fn speak_aloud() -> Procedure {
        procedure(
            vec![
                tool_step(
                    0,
                    "speak",
                    json!({ "text": "{{inputs.text}}" }),
                    Some("clip"),
                ),
                tool_step(
                    1,
                    "audio",
                    json!({ "action": "playback", "audio_path": "{{clip.path}}" }),
                    None,
                ),
            ],
            vec![ProcedureInput {
                name: "text".into(),
                description: None,
                required: true,
                default: None,
            }],
        )
    }

    #[test]
    fn compiles_a_linear_recipe() {
        let definition = compile(&speak_aloud(), &limits()).unwrap();
        // Namespaced so a recipe cannot collide with an operator pipeline or
        // with another agent's recipe of the same name.
        assert!(
            definition.name.starts_with("procedure/"),
            "{}",
            definition.name
        );
        assert!(
            definition.name.ends_with("/speak-aloud"),
            "{}",
            definition.name
        );
        assert_eq!(definition.steps.len(), 2);
        assert_eq!(definition.steps[0].id, "s0");
        assert!(definition.steps[0].depends_on.is_empty());
        assert_eq!(definition.steps[1].depends_on, vec!["s0"]);
        assert_eq!(definition.max_cost_usd, Some(1.0));
        for step in &definition.steps {
            assert_eq!(step.on_failure, OnFailure::Fail);
            assert_eq!(step.timeout_minutes, Some(5));
        }
        match &definition.steps[0].action {
            StepAction::Tool { tool, input } => {
                assert_eq!(tool, "speak");
                // The template survives compilation unrendered; binding happens
                // per step, at run time.
                assert_eq!(input["text"], "{{inputs.text}}");
            }
            other => panic!("expected a tool step, got {other:?}"),
        }
    }

    #[test]
    fn a_prose_procedure_is_refused_and_names_the_step() {
        let mut p = speak_aloud();
        p.steps[1].input = None;
        let err = compile(&p, &limits()).unwrap_err();
        assert!(err.to_string().contains("step[1]"), "{err}");
        assert!(err.to_string().contains("prose SOP"), "{err}");
    }

    #[test]
    fn an_empty_procedure_is_refused() {
        let err = compile(&procedure(vec![], vec![]), &limits()).unwrap_err();
        assert!(err.to_string().contains("no steps"), "{err}");
    }

    /// The compiler sorts by `order`, so a row whose `order` is duplicated or
    /// sparse could reorder the chain — turning a binding the author validated
    /// as backward into a forward one. The store is not a trust boundary.
    #[test]
    fn a_malformed_step_order_is_refused() {
        for orders in [vec![0, 0], vec![0, 2], vec![1, 2], vec![1, 0]] {
            let mut p = speak_aloud();
            for (step, order) in p.steps.iter_mut().zip(&orders) {
                step.order = *order;
            }
            // [1, 0] sorts into a dense 0..n and is therefore accepted — the
            // sort is what makes it well-formed, and the chain follows `order`.
            let result = compile(&p, &limits());
            if orders == vec![1, 0] {
                assert!(result.is_ok(), "{orders:?}");
            } else {
                assert!(result.is_err(), "{orders:?} must be refused");
            }
        }
    }

    /// A reordered row must chain by `order`, not by array position.
    #[test]
    fn steps_are_chained_in_order_not_array_position() {
        let mut p = speak_aloud();
        p.steps.swap(0, 1);
        p.steps[0].order = 1;
        p.steps[1].order = 0;
        let definition = compile(&p, &limits()).unwrap();
        match &definition.steps[0].action {
            StepAction::Tool { tool, .. } => assert_eq!(tool, "speak", "s0 must be the speak step"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn required_inputs_are_enforced_and_defaults_applied() {
        let mut p = speak_aloud();
        p.inputs.push(ProcedureInput {
            name: "voice".into(),
            description: None,
            required: false,
            default: Some(json!("af_heart")),
        });

        let bound = bind_inputs(&p, &json!({ "text": "hi" }), &limits()).unwrap();
        assert_eq!(bound["inputs"]["text"], json!("hi"));
        assert_eq!(
            bound["inputs"]["voice"],
            json!("af_heart"),
            "default applied"
        );

        let err = bind_inputs(&p, &json!({}), &limits()).unwrap_err();
        assert!(err.to_string().contains("requires input 'text'"), "{err}");
    }

    /// A typo'd input name that silently does nothing is a bad afternoon: the
    /// run proceeds with a default the caller believed they had overridden.
    #[test]
    fn an_undeclared_input_is_refused() {
        let err = bind_inputs(&speak_aloud(), &json!({ "txt": "hi" }), &limits()).unwrap_err();
        assert!(err.to_string().contains("'txt'"), "{err}");
    }

    /// Inputs are namespaced under `inputs`, so a caller cannot supply a value
    /// that shadows a step's `output_var` and decide what a later step reads.
    #[test]
    fn inputs_cannot_shadow_an_output_var() {
        let mut p = speak_aloud();
        p.inputs.push(ProcedureInput {
            name: "clip".into(),
            description: None,
            required: false,
            default: None,
        });
        let bound = bind_inputs(
            &p,
            &json!({ "text": "hi", "clip": "/etc/passwd" }),
            &limits(),
        )
        .unwrap();
        assert!(!bound.contains_key("clip"), "a bare 'clip' root was bound");
        assert_eq!(bound["inputs"]["clip"], json!("/etc/passwd"));
    }

    #[test]
    fn oversized_inputs_are_refused() {
        let mut small = limits();
        small.max_input_bytes = 32;
        let err =
            bind_inputs(&speak_aloud(), &json!({ "text": "x".repeat(100) }), &small).unwrap_err();
        assert!(err.to_string().contains("limit is 32"), "{err}");
    }

    #[test]
    fn a_non_object_inputs_payload_is_refused() {
        let err = bind_inputs(&speak_aloud(), &json!([1, 2]), &limits()).unwrap_err();
        assert!(err.to_string().contains("must be an object"), "{err}");
    }

    /// Null is how a model spells "no arguments"; it must not be an error, only
    /// a missing REQUIRED input should be.
    #[test]
    fn a_null_inputs_payload_means_no_arguments() {
        let p = procedure(vec![tool_step(0, "datetime", json!({}), None)], vec![]);
        let bound = bind_inputs(&p, &serde_json::Value::Null, &limits()).unwrap();
        assert_eq!(bound["inputs"], json!({}));
    }
}
