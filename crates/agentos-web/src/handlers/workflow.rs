use crate::handlers::render;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use minijinja::context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use uuid::Uuid;

use agentos_pipeline::WorkflowSpec;

/* ── helpers ──────────────────────────────────────────────────────────── */

fn workflows_dir(state: &AppState) -> PathBuf {
    state.kernel.data_dir().join("workflows")
}

fn ensure_workflows_dir(state: &AppState) -> std::io::Result<PathBuf> {
    let dir = workflows_dir(state);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn load_spec(path: &std::path::Path) -> Option<WorkflowSpec> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn persist_spec(dir: &std::path::Path, spec: &WorkflowSpec) -> std::io::Result<()> {
    let path = dir.join(format!("{}.json", spec.id));
    let raw = serde_json::to_string_pretty(spec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, raw)
}

/* ── workflow summary for list page ──────────────────────────────────── */

#[derive(Serialize)]
struct WorkflowSummary {
    id: String,
    name: String,
    version: String,
    node_count: usize,
    status: String,
    updated_at: String,
}

impl From<WorkflowSpec> for WorkflowSummary {
    fn from(s: WorkflowSpec) -> Self {
        WorkflowSummary {
            node_count: s.nodes.len(),
            id: s.id,
            name: s.name,
            version: s.version,
            status: "active".into(),
            updated_at: String::new(),
        }
    }
}

/* ── GET /workflows ───────────────────────────────────────────────────── */

pub async fn list(State(state): State<AppState>) -> Response {
    let dir = workflows_dir(&state);
    let mut workflows: Vec<WorkflowSummary> = vec![];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(spec) = load_spec(&path) {
                workflows.push(spec.into());
            }
        }
    }
    workflows.sort_by(|a, b| a.name.cmp(&b.name));
    let ctx = context! { page_title => "Workflows", workflows };
    render(&state.templates, "workflow/list.html", ctx)
}

/* ── GET /workflows/new ───────────────────────────────────────────────── */

pub async fn new_workflow(State(state): State<AppState>) -> Response {
    let spec = WorkflowSpec {
        id: String::new(),
        name: "Untitled Workflow".into(),
        ..Default::default()
    };
    builder_page(&state, spec).await
}

/* ── GET /workflows/:id/edit ─────────────────────────────────────────── */

pub async fn edit_workflow(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if id.contains("..") || id.contains('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let dir = workflows_dir(&state);
    let path = dir.join(format!("{id}.json"));
    let Some(spec) = load_spec(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    builder_page(&state, spec).await
}

/* ── shared builder page renderer ────────────────────────────────────── */

async fn builder_page(state: &AppState, spec: WorkflowSpec) -> Response {
    let palette = state.service.node_palette().await;
    let vault_keys: Vec<String> = state
        .service
        .list_secrets()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.name)
        .collect();
    let page_title = format!("{} — Workflow Builder", spec.name);
    let spec_json = serde_json::to_string(&spec).unwrap_or_else(|_| "{}".into());
    let ctx = context! {
        page_title,
        spec,
        spec_json,
        palette,
        vault_keys,
    };
    render(&state.templates, "workflow/builder.html", ctx)
}

/* ── POST /api/workflows ─────────────────────────────────────────────── */

pub async fn create(State(state): State<AppState>, Json(mut spec): Json<WorkflowSpec>) -> Response {
    if spec.id.is_empty() {
        spec.id = Uuid::new_v4().to_string();
    }
    let dir = match ensure_workflows_dir(&state) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "Failed to create workflows dir");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if let Err(e) = persist_spec(&dir, &spec) {
        tracing::error!(error = %e, "Failed to persist workflow");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Json(json!({ "id": spec.id })).into_response()
}

/* ── PUT /api/workflows/:id ──────────────────────────────────────────── */

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut spec): Json<WorkflowSpec>,
) -> Response {
    if id.contains("..") || id.contains('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    spec.id = id;
    let dir = match ensure_workflows_dir(&state) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "Failed to create workflows dir");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if let Err(e) = persist_spec(&dir, &spec) {
        tracing::error!(error = %e, "Failed to persist workflow");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Json(json!({ "id": spec.id })).into_response()
}

/* ── POST /api/workflows/node-properties ─────────────────────────────── */

#[derive(Deserialize)]
pub struct NodePropertiesForm {
    pub node_type: String,
    pub node_id: String,
    pub parameters: String, // JSON string
}

pub async fn node_properties(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<NodePropertiesForm>,
) -> Response {
    if form.node_type.len() > 256 || form.node_id.len() > 256 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let manifest = state.service.get_node_type(&form.node_type).await;
    let parameters: serde_json::Value = serde_json::from_str(&form.parameters)
        .unwrap_or(serde_json::Value::Object(Default::default()));

    /* populate resource_picker options dynamically from live registry */
    let mut properties = manifest
        .as_ref()
        .map(|m| m.node.properties.clone())
        .unwrap_or_default();

    use agentos_nodes::manifest::{PropertyOption, PropertyType};
    for prop in &mut properties {
        if prop.property_type != PropertyType::ResourcePicker || !prop.options.is_empty() {
            continue;
        }
        let resource = prop
            .type_options
            .get("resource")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        prop.options = match resource {
            "agents" => state
                .service
                .list_agents()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|a| PropertyOption {
                    value: serde_json::Value::String(a.name.clone()),
                    label: a.name,
                    description: None,
                    icon: None,
                })
                .collect(),
            "tools" => state
                .service
                .list_tools()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|t| PropertyOption {
                    value: serde_json::Value::String(t.name.clone()),
                    label: format!("{} — {}", t.name, t.description),
                    description: None,
                    icon: None,
                })
                .collect(),
            "workflows" => {
                let dir = workflows_dir(&state);
                std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| {
                        let p = e.path();
                        if p.extension().and_then(|x| x.to_str()) != Some("json") {
                            return None;
                        }
                        let spec = load_spec(&p)?;
                        Some(PropertyOption {
                            value: serde_json::Value::String(spec.id.clone()),
                            label: spec.name,
                            description: None,
                            icon: None,
                        })
                    })
                    .collect()
            }
            _ => vec![],
        };
    }

    let ctx = context! {
        node_type => form.node_type,
        node_id   => form.node_id,
        properties,
        parameters,
    };
    render(
        &state.templates,
        "workflow/partials/node_properties.html",
        ctx,
    )
}

/* ── POST /api/workflows/:id/run ─────────────────────────────────────── */

#[derive(Deserialize)]
pub struct RunRequest {
    pub input: Option<String>,
}

pub async fn run_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RunRequest>,
) -> Response {
    if id.contains("..") || id.contains('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let dir = workflows_dir(&state);
    let path = dir.join(format!("{id}.json"));
    let Some(spec) = load_spec(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    /* compile to PipelineDefinition */
    let pipeline_def = match spec.compile_to_pipeline(&state.kernel.node_registry).await {
        Ok(d) => d,
        Err(e) => {
            return (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response();
        }
    };

    let input = req.input.unwrap_or_default();
    let pipeline_name = format!("workflow-{id}");

    /* save compiled pipeline so the engine can find it by name */
    let def_value = match serde_json::to_value(&pipeline_def) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize compiled pipeline");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let save_req = agentos_api::types::SavePipelineRequest {
        name: pipeline_name.clone(),
        definition: def_value,
    };
    if let Err(e) = state.service.save_pipeline(save_req).await {
        tracing::warn!(error = %e, "Failed to save compiled pipeline before run");
    }

    use agentos_api::types::RunPipelineRequest;
    let pipeline_req = RunPipelineRequest {
        name: pipeline_name,
        input,
        detach: true,
        agent_name: None,
    };
    match state.service.run_pipeline(pipeline_req).await {
        Ok(data) => {
            // kernel returns { "run_id": "..." } for detach=true
            let run_id = data
                .get("run_id")
                .or_else(|| data.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Json(json!({ "run_id": run_id })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/* ── GET /api/workflows/runs/:run_id/events ──────────────────────────── */

pub async fn run_events(State(state): State<AppState>, Path(run_id): Path<String>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use std::time::Duration;
    use tokio::time::interval;

    if run_id.contains("..") || run_id.len() > 128 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let stream = async_stream::stream! {
        let mut tick = interval(Duration::from_secs(1));
        let mut last_step_statuses: std::collections::HashMap<String, String> = Default::default();
        let mut poll_count = 0u32;

        loop {
            tick.tick().await;
            poll_count += 1;

            // Give up after 10 minutes of polling
            if poll_count > 600 {
                yield Ok::<_, std::convert::Infallible>(
                    Event::default().data(
                        serde_json::json!({ "type": "error", "message": "timeout" }).to_string()
                    )
                );
                break;
            }

            let run_data = match state.service.get_pipeline_run(&run_id).await {
                Ok(d) => d,
                Err(_) => {
                    // Run not found yet — still starting up
                    if poll_count > 10 {
                        yield Ok(Event::default().data(
                            serde_json::json!({ "type": "error", "message": "run not found" }).to_string()
                        ));
                        break;
                    }
                    continue;
                }
            };

            // Emit events for each step whose status changed
            if let Some(steps) = run_data.get("step_results").and_then(|v| v.as_object()) {
                for (step_id, step_val) in steps {
                    let status = step_val
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    if last_step_statuses.get(step_id).map(|s| s.as_str()) != Some(status.as_str()) {
                        last_step_statuses.insert(step_id.clone(), status.clone());
                        yield Ok(Event::default().data(
                            serde_json::json!({
                                "type": "step",
                                "node_id": step_id,
                                "status": status,
                            })
                            .to_string()
                        ));
                    }
                }
            }

            // Check overall run status
            let run_status = run_data
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("running");
            if run_status == "complete" || run_status == "Complete" {
                yield Ok(Event::default().data(
                    serde_json::json!({
                        "type": "complete",
                        "output": run_data.get("output"),
                    })
                    .to_string()
                ));
                break;
            } else if run_status == "failed" || run_status == "Failed" {
                yield Ok(Event::default().data(
                    serde_json::json!({
                        "type": "error",
                        "message": run_data.get("error"),
                    })
                    .to_string()
                ));
                break;
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
