//! `procedure-create` — write a procedure to procedural memory.
//!
//! A procedure is prose (an SOP an LLM reads) or executable (every step is a
//! `tool` plus an `input` payload template, and `procedure-run` invokes them
//! directly). The validation below is what separates the two: a recipe that
//! passes it can be compiled and run without an LLM in the loop.

use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::{ProceduralStore, Procedure, ProcedureInput, ProcedureStep};
use agentos_pipeline::bindings;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;

/// Most steps one procedure may declare.
const MAX_STEPS: usize = 32;
/// Most parameters one procedure may declare. Mirrors the manifest's `maxItems`,
/// which is advisory — `payload_schema` is not validated at execution.
const MAX_INPUTS: usize = 16;
/// Longest procedure name. It is an addressable identifier, not prose.
const MAX_NAME_LEN: usize = 128;
/// Cap on the serialized `steps` + `inputs`. A single tool call is capped at
/// 16 KiB (`MAX_TOOL_ARGS_BYTES`); a procedure is several, so this is four of
/// them, not an invitation to store a program.
const MAX_RECIPE_BYTES: usize = 64 * 1024;

fn invalid(reason: impl Into<String>) -> AgentOSError {
    AgentOSError::SchemaValidation(reason.into())
}

/// Collect every `{{key}}` appearing anywhere in a JSON value.
///
/// Walks objects, arrays and strings, because a payload template may put a
/// binding at any depth: `{"items": [{"path": "{{inputs.p}}"}]}`.
fn template_keys(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            let mut rest = text.as_str();
            while let Some(open) = rest.find("{{") {
                let after = &rest[open + 2..];
                let Some(close) = after.find("}}") else { break };
                out.push(after[..close].trim().to_string());
                rest = &after[close + 2..];
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| template_keys(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| template_keys(v, out)),
        _ => {}
    }
}

pub struct ProcedureCreate {
    procedural: Arc<ProceduralStore>,
}

impl ProcedureCreate {
    pub fn new(procedural: Arc<ProceduralStore>) -> Self {
        Self { procedural }
    }
}

#[async_trait]
impl AgentTool for ProcedureCreate {
    fn name(&self) -> &str {
        "procedure-create"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.procedural".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("memory.procedural", PermissionOp::Write)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.procedural".to_string(),
                operation: format!("{:?}", PermissionOp::Write),
            });
        }

        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("procedure-create requires a non-empty 'name' field"))?;
        // The name is how procedure-run and the schedule path address this
        // recipe, so it is an identifier, not free text.
        if name.len() > MAX_NAME_LEN {
            return Err(invalid(format!(
                "'name' is longer than {MAX_NAME_LEN} characters"
            )));
        }

        let description = payload
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "procedure-create requires 'description' field".into(),
                )
            })?;

        // Declared parameters, validated before the steps that reference them.
        let inputs: Vec<ProcedureInput> = match payload.get("inputs") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(arr)) => {
                if arr.len() > MAX_INPUTS {
                    return Err(invalid(format!(
                        "a procedure may declare at most {MAX_INPUTS} inputs (got {})",
                        arr.len()
                    )));
                }
                let mut parsed = Vec::with_capacity(arr.len());
                let mut seen = HashSet::new();
                for (i, raw) in arr.iter().enumerate() {
                    let input: ProcedureInput = serde_json::from_value(raw.clone())
                        .map_err(|e| invalid(format!("inputs[{i}] is not a valid input: {e}")))?;
                    if !agentos_memory::types::valid_template_identifier(&input.name) {
                        return Err(invalid(format!(
                            "inputs[{i}]: name '{}' must be 1-64 characters matching \
                             [A-Za-z_][A-Za-z0-9_]* — it becomes a template key",
                            input.name
                        )));
                    }
                    if !seen.insert(input.name.clone()) {
                        return Err(invalid(format!(
                            "inputs[{i}]: duplicate input name '{}'",
                            input.name
                        )));
                    }
                    parsed.push(input);
                }
                parsed
            }
            Some(_) => return Err(invalid("'inputs' must be an array")),
        };

        // Parse steps: array of {action, tool?, input?, output_var?, expected_outcome?}
        let steps: Vec<ProcedureStep> = match payload.get("steps") {
            Some(serde_json::Value::Array(arr)) if !arr.is_empty() => {
                if arr.len() > MAX_STEPS {
                    return Err(invalid(format!(
                        "a procedure may declare at most {MAX_STEPS} steps (got {})",
                        arr.len()
                    )));
                }
                let mut parsed = Vec::with_capacity(arr.len());
                for (i, step) in arr.iter().enumerate() {
                    let action = step
                        .get("action")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            invalid(format!("step[{}] requires a non-empty 'action' field", i))
                        })?;
                    let output_var = step
                        .get("output_var")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    if let Some(var) = &output_var {
                        if var == bindings::INPUTS_ROOT {
                            return Err(invalid(format!(
                                "step[{i}]: output_var '{}' is reserved — it is the root the \
                                 declared inputs bind under, so shadowing it would redirect \
                                 every later {{{{inputs.x}}}} at this step's output",
                                bindings::INPUTS_ROOT
                            )));
                        }
                        if !agentos_memory::types::valid_template_identifier(var) {
                            return Err(invalid(format!(
                                "step[{i}]: output_var '{var}' must be 1-64 characters matching \
                                 [A-Za-z_][A-Za-z0-9_]* — it becomes a template key"
                            )));
                        }
                    }
                    parsed.push(ProcedureStep {
                        order: i,
                        action: action.to_string(),
                        tool: step
                            .get("tool")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string()),
                        expected_outcome: step
                            .get("expected_outcome")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        // Must be an object: it becomes a tool payload, and a
                        // string or array there produces a call no tool accepts.
                        input: step.get("input").cloned().filter(|v| v.is_object()),
                        output_var,
                    });
                }
                parsed
            }
            _ => {
                return Err(invalid(
                    "procedure-create requires a non-empty 'steps' array",
                ))
            }
        };

        validate_recipe(&steps, &inputs)?;

        let preconditions: Vec<String> = payload
            .get("preconditions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let postconditions: Vec<String> = payload
            .get("postconditions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let tags: Vec<String> = payload
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let procedure = Procedure {
            id: String::new(), // auto-generated by store
            name: name.to_string(),
            description: description.to_string(),
            preconditions,
            steps,
            postconditions,
            success_count: 0,
            failure_count: 0,
            source_episodes: Vec::new(),
            agent_id: Some(context.agent_id),
            tags,
            inputs,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_used_at: None,
            use_count: 0,
            confidence: agentos_memory::types::default_confidence(),
            status: agentos_memory::MemoryStatus::Active,
        };

        let executable = procedure.is_executable();
        let id = self.procedural.store(&procedure).await.map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "procedure-create".into(),
                reason: format!("Store failed: {}", e),
            }
        })?;

        Ok(serde_json::json!({
            "success": true,
            "id": id,
            "name": name,
            "executable": executable,
            "message": if executable {
                "Procedure stored and is executable — run it with procedure-run."
            } else {
                "Procedure stored as a prose SOP. Give every step a 'tool' and an \
                 'input' payload to make it runnable with procedure-run."
            },
        }))
    }
}

/// True if `key` names something bound by the time the referencing step runs.
///
/// Two independent checks, and BOTH matter:
///
/// 1. **The grammar is the renderer's own** ([`bindings::is_valid_path`]). When
///    this was laxer, `{{rows.0.id}}` and `{{clip.path-x}}` passed authoring,
///    matched no binding at render time, and were handed to a live gated tool
///    as the literal text `{{rows.0.id}}` — the exact failure this validator
///    exists to prevent.
/// 2. **The root is bound** — a declared input, or an `output_var` from an
///    EARLIER step. A forward reference renders as a marker and hands the tool
///    a payload that is well-formed and wrong.
///
/// Only the root can be checked here. Whether the value actually has the field
/// is a run-time question about data that does not exist yet.
fn binding_resolves(key: &str, declared: &HashSet<&str>, bound: &HashSet<&str>) -> bool {
    if !bindings::is_valid_path(key) {
        return false;
    }
    match key.strip_prefix("inputs.") {
        Some(rest) => declared.contains(bindings::path_root(rest)),
        None => bound.contains(bindings::path_root(key)),
    }
}

/// Reject a recipe that would fail, confusingly, at run time.
///
/// Every check here exists because the failure it prevents surfaces somewhere
/// unhelpful: an unresolved `{{key}}` reaches the tool as literal text, a
/// forward reference renders empty, and a half-executable procedure silently
/// skips its prose steps.
pub fn validate_recipe(
    steps: &[ProcedureStep],
    inputs: &[ProcedureInput],
) -> Result<(), AgentOSError> {
    // Step count first: `MAX_STEPS` was checked only while parsing, so a row
    // written by any other path compiled into a pipeline of any size.
    if steps.len() > MAX_STEPS {
        return Err(invalid(format!(
            "a procedure may declare at most {MAX_STEPS} steps (got {})",
            steps.len()
        )));
    }

    // Size next, and for prose too. This text is not inert: `build_steps_text`
    // feeds it into the embedding input, the FTS index, and back into agent
    // context via procedure-search. Checking it last also meant serializing a
    // multi-megabyte payload twice just to reject it.
    let size = serde_json::to_string(steps).map(|s| s.len()).unwrap_or(0)
        + serde_json::to_string(inputs).map(|s| s.len()).unwrap_or(0);
    if size > MAX_RECIPE_BYTES {
        return Err(invalid(format!(
            "the recipe is {size} bytes; the limit is {MAX_RECIPE_BYTES}"
        )));
    }

    // `order` is assigned from the array index by this tool, but validation
    // walks the array while the compiler sorts by `order`. Asserting they agree
    // keeps this function self-contained: any future writer that produces a
    // mismatch would otherwise flip a validated backward reference into a
    // run-time forward one.
    for (position, step) in steps.iter().enumerate() {
        if step.order != position {
            return Err(invalid(format!(
                "step[{position}] declares order {}; steps must be a dense 0..n sequence",
                step.order
            )));
        }
    }

    let with_payload = steps.iter().filter(|s| s.input.is_some()).count();
    if with_payload == 0 {
        // A prose SOP. Nothing below applies — it is never executed.
        return Ok(());
    }
    // All-or-nothing: a recipe the author believes is runnable, that quietly
    // skips a step, is worse than one that is refused with the step named.
    if with_payload != steps.len() {
        let prose = steps
            .iter()
            .find(|s| s.input.is_none())
            .map(|s| s.order)
            .unwrap_or(0);
        return Err(invalid(format!(
            "step[{prose}] has no 'input' payload while other steps do. Give every step a \
             'tool' and an 'input' to make this procedure executable, or drop the payloads \
             to store it as a prose SOP."
        )));
    }

    let declared: HashSet<&str> = inputs.iter().map(|i| i.name.as_str()).collect();
    let mut bound: HashSet<&str> = HashSet::new();

    for step in steps {
        let Some(tool) = step.tool.as_deref().filter(|t| !t.is_empty()) else {
            return Err(invalid(format!(
                "step[{}] has an 'input' payload but no 'tool' to send it to",
                step.order
            )));
        };
        // The tool REGISTRY is not reachable from a tool context, so "does this
        // tool exist?" is checked at run time, where it is. This is the check
        // that can be made here, and it is the one that matters: automation
        // must not be able to schedule or spawn.
        if crate::automation_policy::is_tool_blocked_for_automation(tool) {
            return Err(invalid(format!(
                "step[{}]: '{tool}' cannot be run from a procedure — scheduling, spawning and \
                 interactive tools are excluded so a recipe cannot trigger itself",
                step.order
            )));
        }

        if let Some(payload) = &step.input {
            let mut keys = Vec::new();
            template_keys(payload, &mut keys);
            for key in keys {
                if !binding_resolves(&key, &declared, &bound) {
                    return Err(invalid(format!(
                        "step[{}] references {{{{{key}}}}}, which is not a declared input \
                         (inputs.<name>) nor an output_var bound by an EARLIER step. \
                         Append a field to select into a value: {{{{inputs.opts.retries}}}}, \
                         {{{{clip.path}}}}",
                        step.order
                    )));
                }
            }
        }

        if let Some(var) = &step.output_var {
            // Also checked while parsing, for a better error index — but it
            // belongs here too, because this is the function execution
            // re-runs against whatever the store actually holds.
            if var == bindings::INPUTS_ROOT {
                return Err(invalid(format!(
                    "step[{}]: output_var '{var}' is reserved — it is the root the declared \
                     inputs bind under, so shadowing it would redirect every later \
                     {{{{inputs.x}}}} at this step's output",
                    step.order
                )));
            }
            // Two steps binding the same name means a later `{{var}}` silently
            // takes whichever ran last — the inputs loop already refuses
            // duplicates, and this is the same mistake one level down.
            if !bound.insert(var.as_str()) {
                return Err(invalid(format!(
                    "step[{}]: output_var '{var}' is already bound by an earlier step",
                    step.order
                )));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(order: usize, tool: Option<&str>, input: Option<serde_json::Value>) -> ProcedureStep {
        ProcedureStep {
            order,
            action: format!("step {order}"),
            tool: tool.map(String::from),
            expected_outcome: None,
            input,
            output_var: None,
        }
    }

    fn input(name: &str) -> ProcedureInput {
        ProcedureInput {
            name: name.to_string(),
            description: None,
            required: true,
            default: None,
        }
    }

    /// The 25 procedures already in procedural memory are prose. None of the
    /// executable-recipe rules may touch them.
    #[test]
    fn a_prose_sop_skips_every_recipe_rule() {
        // Every one of these would be rejected in an executable recipe.
        let steps = vec![
            step(0, Some("spawn-agent"), None),
            step(1, None, None),
            step(2, Some("procedure-run"), None),
        ];
        assert!(validate_recipe(&steps, &[]).is_ok());
    }

    #[test]
    fn a_mixed_prose_and_tool_recipe_is_refused() {
        let steps = vec![
            step(0, Some("speak"), Some(json!({"text": "hi"}))),
            step(1, Some("audio"), None),
        ];
        let err = validate_recipe(&steps, &[]).unwrap_err();
        assert!(err.to_string().contains("step[1]"), "{err}");
    }

    #[test]
    fn an_executable_step_without_a_tool_is_refused() {
        let steps = vec![step(0, None, Some(json!({"text": "hi"})))];
        let err = validate_recipe(&steps, &[]).unwrap_err();
        assert!(err.to_string().contains("no 'tool'"), "{err}");
    }

    /// The recursion guard. Without it a procedure could run itself, or a cron
    /// could smuggle a spawning primitive inside one.
    #[test]
    fn automation_denylisted_tools_are_refused() {
        for tool in [
            "spawn-agent",
            "schedule-recurring",
            "procedure-run",
            "ask-user",
        ] {
            let steps = vec![step(0, Some(tool), Some(json!({})))];
            let err = validate_recipe(&steps, &[]).unwrap_err();
            assert!(err.to_string().contains(tool), "{tool}: {err}");
        }
    }

    #[test]
    fn an_undeclared_input_reference_is_refused() {
        let steps = vec![step(
            0,
            Some("speak"),
            Some(json!({"text": "{{inputs.nope}}"})),
        )];
        let err = validate_recipe(&steps, &[input("text")]).unwrap_err();
        assert!(err.to_string().contains("inputs.nope"), "{err}");
    }

    /// A forward reference renders as empty text and hands the tool a payload
    /// that is valid JSON and wrong — the worst kind of failure to debug.
    #[test]
    fn a_forward_output_var_reference_is_refused() {
        let mut first = step(0, Some("speak"), Some(json!({"text": "{{later}}"})));
        first.output_var = Some("early".into());
        let mut second = step(1, Some("audio"), Some(json!({"action": "playback"})));
        second.output_var = Some("later".into());
        let err = validate_recipe(&[first, second], &[]).unwrap_err();
        assert!(err.to_string().contains("later"), "{err}");
    }

    #[test]
    fn a_backward_output_var_reference_is_accepted() {
        let mut first = step(0, Some("speak"), Some(json!({"text": "{{inputs.text}}"})));
        first.output_var = Some("clip".into());
        let second = step(
            1,
            Some("audio"),
            Some(json!({"action": "playback", "audio_path": "{{clip.path}}"})),
        );
        validate_recipe(&[first, second], &[input("text")]).unwrap();
    }

    /// A payload template can put a binding at any depth.
    /// A tool result is an object, so chaining one step into the next means
    /// selecting a field out of it. This is the ordinary case.
    #[test]
    fn field_selection_resolves_against_the_root() {
        let declared = HashSet::from(["opts"]);
        let bound = HashSet::from(["clip"]);
        for good in [
            "inputs.opts",
            "inputs.opts.retries",
            "clip",
            "clip.path",
            "clip.a.b",
        ] {
            assert!(binding_resolves(good, &declared, &bound), "{good}");
        }
        for bad in [
            "inputs.nope",
            "inputs.nope.x",
            "other",
            "other.path",
            "inputs",
        ] {
            assert!(!binding_resolves(bad, &declared, &bound), "{bad}");
        }
    }

    #[test]
    fn nested_template_references_are_checked() {
        let steps = vec![step(
            0,
            Some("http-client"),
            Some(json!({"body": {"items": [{"path": "{{inputs.missing}}"}]}})),
        )];
        let err = validate_recipe(&steps, &[input("present")]).unwrap_err();
        assert!(err.to_string().contains("inputs.missing"), "{err}");
    }

    #[test]
    fn template_keys_finds_every_binding() {
        let mut keys = Vec::new();
        template_keys(
            &json!({"a": "{{one}} and {{ two }}", "b": ["{{three}}"], "c": 1, "d": null}),
            &mut keys,
        );
        assert_eq!(keys, vec!["one", "two", "three"]);
    }

    /// An unterminated `{{` must not hang the walker or swallow the rest.
    #[test]
    fn an_unterminated_binding_is_ignored() {
        let mut keys = Vec::new();
        template_keys(&json!({"a": "{{unclosed", "b": "{{ok}}"}), &mut keys);
        assert_eq!(keys, vec!["ok"]);
    }

    /// C1 regression. These all pass a root-only check and then match no
    /// binding at render time, so the literal `{{...}}` reaches a live tool.
    /// The validator and the renderer must accept the same grammar.
    #[test]
    fn a_path_the_renderer_cannot_match_is_refused() {
        let declared = HashSet::from(["opts"]);
        let bound = HashSet::from(["clip"]);
        for bad in [
            "clip.path-x",  // '-' is not a path character
            "clip.",        // empty trailing segment
            "inputs.opts.", // ditto, under inputs
            "clip..path",   // empty middle segment
            "clip.path x",  // space inside the path
        ] {
            assert!(!binding_resolves(bad, &declared, &bound), "{bad}");
        }
        // Array indexing IS supported by the renderer, so it must validate.
        for good in ["clip.0", "clip.0.id", "inputs.opts.0"] {
            assert!(binding_resolves(good, &declared, &bound), "{good}");
        }
    }

    /// C2 regression. `inputs` is the root the caller's parameters bind under.
    /// A step binding over it redirects every later `{{inputs.x}}` at that
    /// step's own output — parameter injection in an unattended run.
    #[test]
    fn output_var_named_inputs_is_reserved() {
        assert_eq!(agentos_pipeline::bindings::INPUTS_ROOT, "inputs");
        let mut first = step(
            0,
            Some("http-client"),
            Some(json!({"url": "{{inputs.url}}"})),
        );
        first.output_var = Some("inputs".into());
        let second = step(
            1,
            Some("file-writer"),
            Some(json!({"path": "{{inputs.path}}"})),
        );
        let err = validate_recipe(&[first, second], &[input("url"), input("path")]).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err}");
    }

    #[test]
    fn a_duplicate_output_var_is_refused() {
        let mut first = step(0, Some("speak"), Some(json!({})));
        first.output_var = Some("clip".into());
        let mut second = step(1, Some("audio"), Some(json!({})));
        second.output_var = Some("clip".into());
        let err = validate_recipe(&[first, second], &[]).unwrap_err();
        assert!(err.to_string().contains("already bound"), "{err}");
    }

    /// W1 regression: the size cap applies to prose too. That text is fed to
    /// the embedder and the FTS index, so it is not inert.
    #[test]
    fn an_oversized_prose_sop_is_refused() {
        let mut prose = step(0, None, None);
        prose.action = "x".repeat(MAX_RECIPE_BYTES + 1);
        let err = validate_recipe(&[prose], &[]).unwrap_err();
        assert!(err.to_string().contains("limit is"), "{err}");
    }

    /// A row whose `order` disagrees with its position would be re-sorted by
    /// the compiler, flipping a validated backward reference into a forward one.
    #[test]
    fn a_step_order_that_disagrees_with_position_is_refused() {
        let mut steps = vec![
            step(0, Some("speak"), Some(json!({}))),
            step(1, Some("audio"), Some(json!({}))),
        ];
        steps[1].order = 5;
        let err = validate_recipe(&steps, &[]).unwrap_err();
        assert!(err.to_string().contains("dense 0..n"), "{err}");
    }

    #[test]
    fn an_oversized_recipe_is_refused() {
        let big = "x".repeat(MAX_RECIPE_BYTES);
        let steps = vec![step(0, Some("speak"), Some(json!({"text": big})))];
        let err = validate_recipe(&steps, &[]).unwrap_err();
        assert!(err.to_string().contains("limit is"), "{err}");
    }

    #[test]
    fn identifiers_must_be_template_safe() {
        use agentos_memory::types::valid_template_identifier;
        for good in ["text", "_x", "a1", &"a".repeat(64)] {
            assert!(valid_template_identifier(good), "{good}");
        }
        for bad in ["", "1a", "a.b", "a}", "a b", &"a".repeat(65)] {
            assert!(!valid_template_identifier(bad), "{bad:?}");
        }
    }

    #[test]
    fn is_executable_requires_a_payload_on_every_step() {
        use agentos_memory::{MemoryStatus, Procedure};
        let mut procedure = Procedure {
            id: String::new(),
            name: "p".into(),
            description: "d".into(),
            preconditions: vec![],
            steps: vec![
                step(0, Some("speak"), Some(json!({}))),
                step(1, Some("audio"), Some(json!({}))),
            ],
            postconditions: vec![],
            success_count: 0,
            failure_count: 0,
            source_episodes: vec![],
            agent_id: None,
            tags: vec![],
            inputs: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_used_at: None,
            use_count: 0,
            confidence: 0.6,
            status: MemoryStatus::Active,
        };
        assert!(procedure.is_executable());

        procedure.steps[1].input = None;
        assert!(!procedure.is_executable());
        assert_eq!(procedure.first_prose_step().map(|s| s.order), Some(1));

        procedure.steps.clear();
        assert!(
            !procedure.is_executable(),
            "an empty procedure is not runnable"
        );
    }
}
