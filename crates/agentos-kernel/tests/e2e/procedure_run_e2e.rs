//! End-to-end: an agent-authored executable procedure really becomes tool calls.
//!
//! Everything below the `procedure-run` call is the production path — the
//! approval gate, the compiler, the structural binder, the pipeline engine and
//! a per-step capability token minted from the agent's own `PermissionSet`.
//!
//! The recipe is `scratch-write` → `scratch-read`, chosen because the second
//! step binds `{{written.title}}` — the FIRST STEP'S OUTPUT, not the caller's
//! input. A run that produced the right page by re-reading `{{inputs.title}}`
//! would pass a weaker test and prove nothing about chaining.

use crate::common;
use agentos_kernel::Kernel;
use agentos_llm::{InferenceToolCall, MockResponse, StopReason};
use agentos_memory::{MemoryStatus, Procedure, ProcedureInput, ProcedureStep};
use agentos_types::{AgentID, PermissionOp, PermissionSet};
use serde_json::json;
use serial_test::serial;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn step(order: usize, tool: &str, input: serde_json::Value, var: Option<&str>) -> ProcedureStep {
    ProcedureStep {
        order,
        action: format!("step {order}"),
        tool: Some(tool.to_string()),
        expected_outcome: None,
        input: Some(input),
        output_var: var.map(String::from),
    }
}

fn chained_recipe(name: &str, owner: Option<AgentID>) -> Procedure {
    Procedure {
        id: String::new(),
        name: name.to_string(),
        description: "write a scratch page, then read it back".to_string(),
        preconditions: vec![],
        steps: vec![
            step(
                0,
                "scratch-write",
                json!({ "title": "{{inputs.title}}", "content": "{{inputs.body}}" }),
                Some("written"),
            ),
            // Binds the PREVIOUS STEP'S OUTPUT, not the caller's input.
            step(
                1,
                "scratch-read",
                json!({ "title": "{{written.title}}" }),
                Some("page"),
            ),
        ],
        postconditions: vec![],
        success_count: 0,
        failure_count: 0,
        source_episodes: vec![],
        agent_id: owner,
        tags: vec![],
        inputs: vec![
            ProcedureInput {
                name: "title".into(),
                description: None,
                required: true,
                default: None,
            },
            ProcedureInput {
                name: "body".into(),
                description: None,
                required: false,
                default: Some(json!("bound from the default")),
            },
        ],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        last_used_at: None,
        use_count: 0,
        confidence: 0.6,
        status: MemoryStatus::Active,
    }
}

fn runner_permissions() -> PermissionSet {
    let mut permissions = PermissionSet::new();
    permissions.grant_op("memory.procedural".to_string(), PermissionOp::Read, None);
    permissions.grant_op("scratchpad".to_string(), PermissionOp::Read, None);
    permissions.grant_op("scratchpad".to_string(), PermissionOp::Write, None);
    permissions
}

/// Stand in for the operator. `procedure-run` is `control_plane`, which
/// `ApprovalMode::decide` returns `Prompt` for in EVERY mode, so without this
/// the run parks until the 5-minute auto-deny.
fn spawn_approver(kernel: Arc<Kernel>) -> CancellationToken {
    let token = CancellationToken::new();
    let stop = token.clone();
    tokio::spawn(async move {
        while !stop.is_cancelled() {
            for pending in kernel.escalation_manager.list_pending().await {
                kernel
                    .escalation_manager
                    .resolve(pending.id, "approved".to_string())
                    .await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    token
}

/// The two mock turns that make an agent call `procedure-run` once: the tool
/// call, then a terminal reply so the loop ends.
fn one_procedure_call(payload: serde_json::Value) -> Vec<MockResponse> {
    vec![
        MockResponse::text("Running it.").with_tool_calls(vec![InferenceToolCall {
            id: Some("call_procedure_run".to_string()),
            tool_name: "procedure-run".to_string(),
            intent_type: "execute".to_string(),
            payload,
        }]),
        MockResponse::text("done").with_stop_reason(StopReason::EndTurn),
    ]
}

/// Fire the turn and hand back what `procedure-run` returned.
async fn run_procedure_via_agent(kernel: &Arc<Kernel>, agent: &str) -> serde_json::Value {
    let result = kernel
        .chat_infer_with_tools(agent, &[], "run the procedure", None, None)
        .await
        .expect("chat_infer_with_tools");
    result
        .tool_calls
        .first()
        .map(|c| c.result.clone())
        .unwrap_or(serde_json::Value::Null)
}

/// The scratchpad store keys pages by the agent's ID string — that is what
/// `scratch-write` passes (`context.agent_id.to_string()`), not the name.
async fn page_content(kernel: &Kernel, agent_id: &AgentID, title: &str) -> Option<String> {
    kernel
        .scratchpad_store
        .read_page(&agent_id.to_string(), title)
        .await
        .ok()
        .map(|page| page.content)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn a_stored_recipe_runs_its_steps_in_order_and_chains_their_outputs() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent_with_permissions(
        &kernel,
        "Runner",
        one_procedure_call(
            json!({ "procedure": "note-and-read", "inputs": { "title": "e2e-note" } }),
        ),
        runner_permissions(),
    )
    .await;
    kernel
        .procedural_memory
        .store(&chained_recipe("note-and-read", Some(agent_id)))
        .await
        .expect("store the recipe");

    let approver = spawn_approver(kernel.clone());
    let result = run_procedure_via_agent(&kernel, "Runner").await;
    approver.cancel();

    assert_eq!(
        result["status"], "complete",
        "run did not complete: {result}"
    );
    assert_eq!(result["steps_run"], 2, "{result}");

    // Step 0 really wrote the page, with the optional input's DEFAULT bound.
    let content = page_content(&kernel, &agent_id, "e2e-note")
        .await
        .expect("step 0 wrote no page");
    assert!(
        content.contains("bound from the default"),
        "the default was not bound: {content}"
    );

    // The last step's `output_var` becomes the run's output, so this is what
    // `scratch-read` actually answered. It answers `found: false` for a miss
    // rather than failing, so a broken chain would still report
    // `status: complete` — this is the assertion that tells them apart, and it
    // is the only one that proves `{{written.title}}` resolved to step 0's
    // OUTPUT rather than to the caller's input.
    let output = result["output"].as_str().unwrap_or_default();
    assert!(output.contains("true"), "step 1 found no page: {output}");
    assert!(
        output.contains("bound from the default"),
        "step 1 read a page, but not the one step 0 wrote: {output}"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn a_missing_required_input_fails_before_any_step_runs() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent_with_permissions(
        &kernel,
        "Runner",
        one_procedure_call(json!({ "procedure": "note-and-read", "inputs": {} })),
        runner_permissions(),
    )
    .await;
    kernel
        .procedural_memory
        .store(&chained_recipe("note-and-read", Some(agent_id)))
        .await
        .unwrap();

    let approver = spawn_approver(kernel.clone());
    let result = run_procedure_via_agent(&kernel, "Runner").await;
    approver.cancel();

    let error = result["error"].as_str().unwrap_or_default();
    assert!(error.contains("requires input 'title'"), "{result}");

    handle.abort();
}

/// The agent identity is the security boundary. Another agent's private recipe
/// must not be reachable by name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn another_agents_private_recipe_is_not_reachable() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let owner = AgentID::new();
    let _caller = common::register_mock_agent_with_permissions(
        &kernel,
        "Runner",
        one_procedure_call(json!({ "procedure": "someone-elses", "inputs": { "title": "x" } })),
        runner_permissions(),
    )
    .await;
    kernel
        .procedural_memory
        .store(&chained_recipe("someone-elses", Some(owner)))
        .await
        .unwrap();

    let approver = spawn_approver(kernel.clone());
    let result = run_procedure_via_agent(&kernel, "Runner").await;
    approver.cancel();

    let error = result["error"].as_str().unwrap_or_default();
    assert!(error.contains("no procedure named"), "{result}");

    handle.abort();
}

/// A prose SOP — which is what every procedure written before this feature is —
/// must be refused with the offending step named, never half-run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn a_prose_sop_is_refused() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent_with_permissions(
        &kernel,
        "Runner",
        one_procedure_call(json!({ "procedure": "prose-sop" })),
        runner_permissions(),
    )
    .await;

    let mut prose = chained_recipe("prose-sop", Some(agent_id));
    for step in &mut prose.steps {
        step.input = None;
    }
    prose.inputs.clear();
    kernel.procedural_memory.store(&prose).await.unwrap();

    let approver = spawn_approver(kernel.clone());
    let result = run_procedure_via_agent(&kernel, "Runner").await;
    approver.cancel();

    let error = result["error"].as_str().unwrap_or_default();
    assert!(error.contains("prose"), "{result}");
    assert!(
        page_content(&kernel, &agent_id, "e2e-note").await.is_none(),
        "a refused procedure still wrote a page"
    );

    handle.abort();
}
