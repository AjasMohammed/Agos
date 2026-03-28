use crate::state::AppState;
use agentos_pipeline::{
    PipelineDefinition, PipelineRunStatus, PipelineStep, PipelineSummary, StepAction,
};
use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use futures::Stream;
use minijinja::context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

const START_NODE_ID: &str = "__start__";
const END_NODE_ID: &str = "__end__";

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPipelineGraph {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
    #[serde(default)]
    pub max_wall_time_minutes: Option<u64>,
    pub nodes: Vec<VisualNode>,
    pub edges: Vec<VisualEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualNode {
    pub id: String,
    pub node_type: VisualNodeType,
    pub label: String,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub config: VisualNodeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisualNodeType {
    Start,
    Agent,
    Tool,
    End,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VisualNodeConfig {
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input_json: String,
    #[serde(default)]
    pub output_var: String,
    #[serde(default)]
    pub timeout_minutes: Option<u64>,
    #[serde(default)]
    pub retry_on_failure: Option<u32>,
    #[serde(default)]
    pub retry_backoff_ms: Option<u64>,
    #[serde(default)]
    pub retry_max_delay_ms: Option<u64>,
    #[serde(default)]
    pub on_failure: String,
    #[serde(default)]
    pub default_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisualEdge {
    pub id: String,
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEnvelope {
    pub graph: VisualPipelineGraph,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRequest {
    pub yaml: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRequest {
    pub graph: VisualPipelineGraph,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveResponse {
    pub name: String,
    pub version: String,
    pub step_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub pipeline_name: String,
    pub input: String,
    pub agent_name: String,
}

#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub run_id: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct CloneForm {
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
struct StepEventPayload {
    step_id: String,
    status: String,
    output_preview: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct RunEventPayload {
    run_id: String,
    status: String,
    output: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct VisualMeta {
    positions: HashMap<String, (i32, i32)>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
) -> Response {
    let store = state.kernel.pipeline_engine.store_arc();
    let pipelines = match tokio::task::spawn_blocking(move || store.list_pipelines()).await {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Failed to list pipelines");
            vec![]
        }
        Err(e) => {
            tracing::warn!(error = %e, "Pipeline list task panicked");
            vec![]
        }
    };

    let pipeline_rows = pipeline_rows(&pipelines);
    if query.partial.as_deref() == Some("list") {
        let ctx = context! { pipelines => pipeline_rows };
        return super::render(&state.templates, "partials/pipeline_row.html", ctx);
    }

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Pipelines",
        breadcrumbs => vec![context! { label => "Pipelines" }],
        pipelines => pipeline_rows,
        csrf_token,
    };
    super::render(&state.templates, "pipelines/list.html", ctx)
}

pub async fn new_builder(State(state): State<AppState>, jar: CookieJar) -> Response {
    render_builder(state, jar, default_graph("untitled-pipeline"), true).await
}

pub async fn edit_builder(
    State(state): State<AppState>,
    Path(name): Path<String>,
    jar: CookieJar,
) -> Response {
    let store = state.kernel.pipeline_engine.store_arc();
    let yaml = match tokio::task::spawn_blocking({
        let name = name.clone();
        move || store.get_pipeline_yaml(&name)
    })
    .await
    {
        Ok(Ok(yaml)) => yaml,
        Ok(Err(e)) => {
            tracing::warn!(pipeline = %name, error = %e, "Failed to load pipeline YAML");
            return (StatusCode::NOT_FOUND, "Pipeline not found").into_response();
        }
        Err(e) => {
            tracing::error!(pipeline = %name, error = %e, "Pipeline load task panicked");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    match pipeline_yaml_to_visual_graph(&yaml) {
        Ok(graph) => render_builder(state, jar, graph, false).await,
        Err(e) => {
            tracing::error!(pipeline = %name, error = %e, "Failed to convert pipeline YAML to visual graph");
            (StatusCode::BAD_REQUEST, "Stored pipeline is invalid").into_response()
        }
    }
}

pub async fn save_pipeline(
    State(state): State<AppState>,
    Json(payload): Json<GraphEnvelope>,
) -> Response {
    let graph = payload.graph;
    let definition = match visual_graph_to_pipeline(&graph) {
        Ok(definition) => definition,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let yaml = match pipeline_to_yaml_with_meta(&definition, &graph) {
        Ok(yaml) => yaml,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    let store = state.kernel.pipeline_engine.store_arc();
    let name = definition.name.clone();
    let version = definition.version.clone();
    let step_count = definition.steps.len();
    let install_name = name.clone();
    let install_version = version.clone();
    match tokio::task::spawn_blocking(move || {
        store.install_pipeline(&install_name, &install_version, &yaml)
    })
    .await
    {
        Ok(Ok(())) => Json(SaveResponse {
            name,
            version,
            step_count,
        })
        .into_response(),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "Failed to save visual pipeline");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to save pipeline").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Pipeline save task panicked");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}

pub async fn import_yaml(Json(payload): Json<ImportRequest>) -> Response {
    match pipeline_yaml_to_visual_graph(&payload.yaml) {
        Ok(graph) => Json(graph).into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, message).into_response(),
    }
}

pub async fn export_yaml(Json(payload): Json<ExportRequest>) -> Response {
    let definition = match visual_graph_to_pipeline(&payload.graph) {
        Ok(definition) => definition,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match pipeline_to_yaml_with_meta(&definition, &payload.graph) {
        Ok(yaml) => Json(json!({ "yaml": yaml })).into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, message).into_response(),
    }
}

pub async fn run_pipeline(
    State(state): State<AppState>,
    Json(payload): Json<RunRequest>,
) -> Response {
    if payload.agent_name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "Agent name is required").into_response();
    }
    if payload.pipeline_name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "Pipeline name is required").into_response();
    }

    match state
        .kernel
        .run_pipeline(
            payload.pipeline_name.clone(),
            payload.input.clone(),
            true,
            Some(payload.agent_name.clone()),
        )
        .await
    {
        Ok(data) => {
            let run_id = data
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Json(RunResponse {
                run_id,
                status: "running".to_string(),
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(pipeline = %payload.pipeline_name, error = %e, "Failed to run pipeline from builder");
            (StatusCode::BAD_REQUEST, "Failed to start pipeline").into_response()
        }
    }
}

pub async fn clone_pipeline(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::Form(form): axum::Form<CloneForm>,
) -> Response {
    let store = state.kernel.pipeline_engine.store_arc();
    let yaml = match tokio::task::spawn_blocking({
        let name = name.clone();
        let store = store.clone();
        move || store.get_pipeline_yaml(&name)
    })
    .await
    {
        Ok(Ok(yaml)) => yaml,
        Ok(Err(e)) => {
            tracing::warn!(pipeline = %name, error = %e, "Failed to load pipeline for clone");
            return (StatusCode::NOT_FOUND, "Pipeline not found").into_response();
        }
        Err(e) => {
            tracing::error!(pipeline = %name, error = %e, "Pipeline clone load task panicked");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    let mut graph = match pipeline_yaml_to_visual_graph(&yaml) {
        Ok(graph) => graph,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    graph.name = form
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| format!("{}-copy", graph.name));
    let definition = match visual_graph_to_pipeline(&graph) {
        Ok(definition) => definition,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let yaml = match pipeline_to_yaml_with_meta(&definition, &graph) {
        Ok(yaml) => yaml,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    match tokio::task::spawn_blocking(move || {
        store.install_pipeline(&definition.name, &definition.version, &yaml)
    })
    .await
    {
        Ok(Ok(())) => axum::response::Redirect::to("/pipelines").into_response(),
        Ok(Err(e)) => {
            tracing::error!(pipeline = %name, error = %e, "Failed to clone pipeline");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to clone pipeline",
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(pipeline = %name, error = %e, "Pipeline clone save task panicked");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}

pub async fn delete_pipeline(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let store = state.kernel.pipeline_engine.store_arc();
    let delete_name = name.clone();
    match tokio::task::spawn_blocking(move || store.remove_pipeline(&delete_name)).await {
        Ok(Ok(())) => axum::response::Redirect::to("/pipelines").into_response(),
        Ok(Err(e)) => {
            tracing::warn!(pipeline = %name, error = %e, "Failed to delete pipeline");
            (StatusCode::BAD_REQUEST, "Failed to delete pipeline").into_response()
        }
        Err(e) => {
            tracing::error!(pipeline = %name, error = %e, "Pipeline delete task panicked");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}

pub async fn run_events(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(32);
    let store = state.kernel.pipeline_engine.store_arc();

    tokio::spawn(async move {
        let run_uuid = match uuid::Uuid::parse_str(&run_id) {
            Ok(uuid) => agentos_types::RunID::from_uuid(uuid),
            Err(_) => {
                let _ = tx
                    .send(
                        Event::default()
                            .event("pipeline-error")
                            .data(r#"{"message":"Invalid run ID"}"#),
                    )
                    .await;
                return;
            }
        };

        let mut previous_statuses: HashMap<String, String> = HashMap::new();
        loop {
            let result = tokio::task::spawn_blocking({
                let store = store.clone();
                move || store.get_run(&run_uuid)
            })
            .await;

            let run = match result {
                Ok(Ok(run)) => run,
                Ok(Err(e)) => {
                    let _ = tx
                        .send(
                            Event::default().event("pipeline-error").data(
                                serde_json::to_string(&json!({ "message": e.to_string() }))
                                    .unwrap_or_else(|_| {
                                        r#"{"message":"Pipeline run not found"}"#.to_string()
                                    }),
                            ),
                        )
                        .await;
                    break;
                }
                Err(e) => {
                    let _ = tx
                        .send(
                            Event::default().event("pipeline-error").data(
                                serde_json::to_string(&json!({ "message": e.to_string() }))
                                    .unwrap_or_else(|_| {
                                        r#"{"message":"Internal error"}"#.to_string()
                                    }),
                            ),
                        )
                        .await;
                    break;
                }
            };

            for step_result in run.step_results.values() {
                let status = step_result.status.to_string();
                let changed = previous_statuses
                    .get(&step_result.step_id)
                    .map(|prev| prev != &status)
                    .unwrap_or(true);
                if changed {
                    previous_statuses.insert(step_result.step_id.clone(), status.clone());
                    let payload = StepEventPayload {
                        step_id: step_result.step_id.clone(),
                        status,
                        output_preview: step_result.output.as_ref().map(|s| truncate(s, 160)),
                        error: step_result.error.clone(),
                    };
                    let data = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
                    if tx
                        .send(Event::default().event("step-status").data(data))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }

            let run_payload = RunEventPayload {
                run_id: run.id.to_string(),
                status: run.status.to_string(),
                output: run.output.clone().map(|s| truncate(&s, 240)),
                error: run.error.clone(),
            };
            let data = serde_json::to_string(&run_payload).unwrap_or_else(|_| "{}".to_string());
            if tx
                .send(Event::default().event("run-status").data(data))
                .await
                .is_err()
            {
                return;
            }

            if run.status != PipelineRunStatus::Running {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let stream = ReceiverStream::new(rx).map(Ok::<Event, Infallible>);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn render_builder(
    state: AppState,
    jar: CookieJar,
    graph: VisualPipelineGraph,
    is_new: bool,
) -> Response {
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let (agent_names, tool_names) = load_builder_catalogs(&state).await;
    let graph_json = match serde_json::to_string(&graph) {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(error = %e, "Failed to serialize builder graph");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to render builder",
            )
                .into_response();
        }
    };
    let agent_names_json = serde_json::to_string(&agent_names).unwrap_or_else(|_| "[]".to_string());
    let tool_names_json = serde_json::to_string(&tool_names).unwrap_or_else(|_| "[]".to_string());
    let title = if is_new {
        "New Pipeline".to_string()
    } else {
        format!("Edit Pipeline — {}", graph.name)
    };

    let ctx = context! {
        page_title => title,
        breadcrumbs => vec![
            context! { label => "Pipelines", href => "/pipelines" },
            context! { label => if is_new { "New Pipeline" } else { graph.name.as_str() } },
        ],
        csrf_token,
        graph_json,
        agent_names_json,
        tool_names_json,
        is_new,
    };
    super::render(&state.templates, "pipelines/builder.html", ctx)
}

async fn load_builder_catalogs(state: &AppState) -> (Vec<String>, Vec<String>) {
    let agents = {
        let registry = state.kernel.agent_registry.read().await;
        let mut names: Vec<String> = registry
            .list_online()
            .iter()
            .map(|a| a.name.clone())
            .collect();
        names.sort();
        names
    };
    let tools = {
        let registry = state.kernel.tool_registry.read().await;
        let mut names: Vec<String> = registry
            .list_all()
            .iter()
            .map(|t| t.manifest.manifest.name.clone())
            .collect();
        names.sort();
        names
    };
    (agents, tools)
}

fn pipeline_rows(pipelines: &[PipelineSummary]) -> Vec<minijinja::Value> {
    pipelines
        .iter()
        .map(|p| {
            context! {
                name => p.name.clone(),
                version => p.version.clone(),
                description => p.description.clone(),
                step_count => p.step_count,
                installed_at => p.installed_at.clone(),
                last_run_at => p.last_run_at.clone(),
                last_run_status => p.last_run_status.clone(),
            }
        })
        .collect()
}

pub(crate) fn visual_graph_to_pipeline(
    graph: &VisualPipelineGraph,
) -> Result<PipelineDefinition, String> {
    let name = graph.name.trim();
    if name.is_empty() {
        return Err("Pipeline name is required".to_string());
    }
    let version = graph.version.trim();
    if version.is_empty() {
        return Err("Pipeline version is required".to_string());
    }

    let mut node_map: HashMap<&str, &VisualNode> = HashMap::new();
    let mut ids = HashSet::new();
    for node in &graph.nodes {
        if !ids.insert(node.id.clone()) {
            return Err(format!("Duplicate node ID '{}'", node.id));
        }
        node_map.insert(node.id.as_str(), node);
    }
    if !node_map.contains_key(START_NODE_ID) {
        return Err("Visual graph is missing a Start node".to_string());
    }
    if !node_map.contains_key(END_NODE_ID) {
        return Err("Visual graph is missing an End node".to_string());
    }

    for edge in &graph.edges {
        if !node_map.contains_key(edge.source.as_str())
            || !node_map.contains_key(edge.target.as_str())
        {
            return Err(format!("Edge '{}' points to a missing node", edge.id));
        }
    }

    let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        incoming
            .entry(edge.target.as_str())
            .or_default()
            .push(edge.source.as_str());
        outgoing
            .entry(edge.source.as_str())
            .or_default()
            .push(edge.target.as_str());
    }

    let mut steps = Vec::new();
    let mut output = None;
    for node in &graph.nodes {
        match node.node_type {
            VisualNodeType::Start => continue,
            VisualNodeType::End => {
                let inputs = incoming.get(node.id.as_str()).cloned().unwrap_or_default();
                if inputs.len() > 1 {
                    return Err("End node can only accept one incoming connection".to_string());
                }
                if let Some(source_id) = inputs.first() {
                    let source = node_map
                        .get(source_id)
                        .ok_or_else(|| "End node depends on a missing source node".to_string())?;
                    let output_var = step_output_var(source);
                    output = Some(output_var);
                }
                continue;
            }
            VisualNodeType::Agent | VisualNodeType::Tool => {}
        }

        let output_var = step_output_var(node);
        let depends_on = incoming
            .get(node.id.as_str())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|source| *source != START_NODE_ID)
            .map(str::to_string)
            .collect::<Vec<_>>();

        let on_failure = parse_on_failure(&node.config.on_failure)?;
        let default_value = if node.config.default_value.trim().is_empty() {
            None
        } else {
            Some(node.config.default_value.clone())
        };

        let action = match node.node_type {
            VisualNodeType::Agent => {
                let agent_name = node.config.agent_name.trim();
                let task = node.config.task.trim();
                if agent_name.is_empty() {
                    return Err(format!(
                        "Agent step '{}' must specify an agent name",
                        node.label
                    ));
                }
                if task.is_empty() {
                    return Err(format!("Agent step '{}' must specify a task", node.label));
                }
                StepAction::Agent {
                    agent: agent_name.to_string(),
                    task: task.to_string(),
                }
            }
            VisualNodeType::Tool => {
                let tool_name = node.config.tool_name.trim();
                if tool_name.is_empty() {
                    return Err(format!(
                        "Tool step '{}' must specify a tool name",
                        node.label
                    ));
                }
                let input = if node.config.tool_input_json.trim().is_empty() {
                    serde_json::Value::Object(Default::default())
                } else {
                    serde_json::from_str(&node.config.tool_input_json).map_err(|e| {
                        format!("Tool input JSON for '{}' is invalid: {}", node.label, e)
                    })?
                };
                StepAction::Tool {
                    tool: tool_name.to_string(),
                    input,
                }
            }
            VisualNodeType::Start | VisualNodeType::End => unreachable!(),
        };

        steps.push(PipelineStep {
            id: node.id.clone(),
            action,
            output_var: Some(output_var),
            depends_on,
            timeout_minutes: node.config.timeout_minutes,
            retry_on_failure: node.config.retry_on_failure,
            retry_backoff_ms: node.config.retry_backoff_ms,
            retry_max_delay_ms: node.config.retry_max_delay_ms,
            on_failure,
            default_value,
        });
    }

    let step_ids: HashSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    for step in &steps {
        if let Some(targets) = outgoing.get(step.id.as_str()) {
            for target in targets {
                if *target != END_NODE_ID && !step_ids.contains(target) {
                    return Err(format!(
                        "Step '{}' has an invalid outgoing connection",
                        step.id
                    ));
                }
            }
        }
    }

    Ok(PipelineDefinition {
        name: name.to_string(),
        version: version.to_string(),
        description: if graph.description.trim().is_empty() {
            None
        } else {
            Some(graph.description.trim().to_string())
        },
        permissions: vec![],
        steps,
        output,
        max_cost_usd: graph.max_cost_usd,
        max_wall_time_minutes: graph.max_wall_time_minutes,
    })
}

pub(crate) fn pipeline_yaml_to_visual_graph(yaml: &str) -> Result<VisualPipelineGraph, String> {
    let definition = PipelineDefinition::from_yaml(yaml)
        .map_err(|e| format!("Failed to parse pipeline YAML: {}", e))?;
    let root: serde_yaml::Value = serde_yaml::from_str(yaml)
        .map_err(|e| format!("Failed to parse pipeline YAML metadata: {}", e))?;
    let meta = extract_visual_meta(&root);
    Ok(pipeline_to_visual_graph(&definition, &meta))
}

pub(crate) fn pipeline_to_yaml_with_meta(
    definition: &PipelineDefinition,
    graph: &VisualPipelineGraph,
) -> Result<String, String> {
    let mut root = serde_yaml::to_value(definition)
        .map_err(|e| format!("Failed to serialize pipeline: {}", e))?;
    let mapping = root
        .as_mapping_mut()
        .ok_or_else(|| "Pipeline YAML root must be a mapping".to_string())?;

    let mut positions = BTreeMap::<String, BTreeMap<String, i32>>::new();
    for node in &graph.nodes {
        let mut pos = BTreeMap::new();
        pos.insert("x".to_string(), node.x);
        pos.insert("y".to_string(), node.y);
        positions.insert(node.id.clone(), pos);
    }
    let meta_value = serde_yaml::to_value(json!({
        "visual": {
            "positions": positions,
        }
    }))
    .map_err(|e| format!("Failed to serialize builder metadata: {}", e))?;
    mapping.insert(serde_yaml::Value::String("meta".to_string()), meta_value);

    serde_yaml::to_string(&root).map_err(|e| format!("Failed to serialize pipeline YAML: {}", e))
}

fn pipeline_to_visual_graph(
    definition: &PipelineDefinition,
    meta: &VisualMeta,
) -> VisualPipelineGraph {
    let mut depths = HashMap::<String, usize>::new();
    let mut steps_by_id = HashMap::<&str, &PipelineStep>::new();
    for step in &definition.steps {
        steps_by_id.insert(step.id.as_str(), step);
    }
    let mut in_progress = HashSet::new();
    for step in &definition.steps {
        compute_depth(step, &steps_by_id, &mut depths, &mut in_progress);
    }

    let mut lanes = HashMap::<usize, usize>::new();
    let mut nodes = vec![visual_start_node(meta)];
    let mut edges = Vec::new();

    for step in &definition.steps {
        let depth = *depths.get(&step.id).unwrap_or(&1);
        let lane = lanes.entry(depth).or_insert(0);
        let (x, y) = meta
            .positions
            .get(&step.id)
            .copied()
            .unwrap_or((220 + (depth as i32 * 220), 120 + (*lane as i32 * 150)));
        *lane += 1;

        nodes.push(VisualNode {
            id: step.id.clone(),
            node_type: match step.action {
                StepAction::Agent { .. } => VisualNodeType::Agent,
                StepAction::Tool { .. } => VisualNodeType::Tool,
            },
            label: humanize_step_label(step),
            x,
            y,
            config: step_to_config(step),
        });

        if step.depends_on.is_empty() {
            edges.push(VisualEdge {
                id: format!("edge-{}-{}", START_NODE_ID, step.id),
                source: START_NODE_ID.to_string(),
                target: step.id.clone(),
            });
        } else {
            for dep in &step.depends_on {
                edges.push(VisualEdge {
                    id: format!("edge-{}-{}", dep, step.id),
                    source: dep.clone(),
                    target: step.id.clone(),
                });
            }
        }
    }

    let end_position = meta.positions.get(END_NODE_ID).copied().unwrap_or_else(|| {
        let max_depth = depths.values().copied().max().unwrap_or(1);
        (220 + ((max_depth + 1) as i32 * 220), 220)
    });
    nodes.push(VisualNode {
        id: END_NODE_ID.to_string(),
        node_type: VisualNodeType::End,
        label: "End".to_string(),
        x: end_position.0,
        y: end_position.1,
        config: VisualNodeConfig::default(),
    });

    if let Some(output_var) = &definition.output {
        if let Some(step) = definition
            .steps
            .iter()
            .find(|step| step.output_var.as_deref() == Some(output_var.as_str()))
        {
            edges.push(VisualEdge {
                id: format!("edge-{}-{}", step.id, END_NODE_ID),
                source: step.id.clone(),
                target: END_NODE_ID.to_string(),
            });
        }
    }

    VisualPipelineGraph {
        name: definition.name.clone(),
        version: definition.version.clone(),
        description: definition.description.clone().unwrap_or_default(),
        output: definition.output.clone(),
        max_cost_usd: definition.max_cost_usd,
        max_wall_time_minutes: definition.max_wall_time_minutes,
        nodes,
        edges,
    }
}

fn visual_start_node(meta: &VisualMeta) -> VisualNode {
    let (x, y) = meta
        .positions
        .get(START_NODE_ID)
        .copied()
        .unwrap_or((40, 220));
    VisualNode {
        id: START_NODE_ID.to_string(),
        node_type: VisualNodeType::Start,
        label: "Start".to_string(),
        x,
        y,
        config: VisualNodeConfig::default(),
    }
}

fn step_output_var(node: &VisualNode) -> String {
    let trimmed = node.config.output_var.trim();
    if !trimmed.is_empty() {
        trimmed.to_string()
    } else {
        format!("{}_output", sanitize_identifier(&node.id))
    }
}

fn sanitize_identifier(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

fn parse_on_failure(value: &str) -> Result<agentos_pipeline::definition::OnFailure, String> {
    match value.trim() {
        "" | "fail" => Ok(agentos_pipeline::definition::OnFailure::Fail),
        "skip" => Ok(agentos_pipeline::definition::OnFailure::Skip),
        "use_default" => Ok(agentos_pipeline::definition::OnFailure::UseDefault),
        other => Err(format!("Unsupported on_failure value '{}'", other)),
    }
}

fn extract_visual_meta(root: &serde_yaml::Value) -> VisualMeta {
    let mut meta = VisualMeta::default();
    let Some(mapping) = root.as_mapping() else {
        return meta;
    };
    let Some(meta_value) = mapping.get(serde_yaml::Value::String("meta".to_string())) else {
        return meta;
    };
    let Some(meta_mapping) = meta_value.as_mapping() else {
        return meta;
    };
    let Some(visual) = meta_mapping.get(serde_yaml::Value::String("visual".to_string())) else {
        return meta;
    };
    let Some(visual_mapping) = visual.as_mapping() else {
        return meta;
    };
    let Some(positions) = visual_mapping.get(serde_yaml::Value::String("positions".to_string()))
    else {
        return meta;
    };
    let Some(positions_mapping) = positions.as_mapping() else {
        return meta;
    };

    for (node_id, coords) in positions_mapping {
        let Some(node_id) = node_id.as_str() else {
            continue;
        };
        let Some(coords) = coords.as_mapping() else {
            continue;
        };
        let x = coords
            .get(serde_yaml::Value::String("x".to_string()))
            .and_then(serde_yaml::Value::as_i64)
            .unwrap_or_default() as i32;
        let y = coords
            .get(serde_yaml::Value::String("y".to_string()))
            .and_then(serde_yaml::Value::as_i64)
            .unwrap_or_default() as i32;
        meta.positions.insert(node_id.to_string(), (x, y));
    }

    meta
}

fn compute_depth<'a>(
    step: &'a PipelineStep,
    steps_by_id: &HashMap<&'a str, &'a PipelineStep>,
    cache: &mut HashMap<String, usize>,
    in_progress: &mut HashSet<String>,
) -> usize {
    if let Some(existing) = cache.get(&step.id) {
        return *existing;
    }
    if !in_progress.insert(step.id.clone()) {
        // Cycle detected — break with depth 1
        return 1;
    }
    let depth = if step.depends_on.is_empty() {
        1
    } else {
        1 + step
            .depends_on
            .iter()
            .filter_map(|dep| steps_by_id.get(dep.as_str()).copied())
            .map(|dep| compute_depth(dep, steps_by_id, cache, in_progress))
            .max()
            .unwrap_or(0)
    };
    in_progress.remove(&step.id);
    cache.insert(step.id.clone(), depth);
    depth
}

fn step_to_config(step: &PipelineStep) -> VisualNodeConfig {
    let (agent_name, task, tool_name, tool_input_json) = match &step.action {
        StepAction::Agent { agent, task } => {
            (agent.clone(), task.clone(), String::new(), String::new())
        }
        StepAction::Tool { tool, input } => (
            String::new(),
            String::new(),
            tool.clone(),
            serde_json::to_string_pretty(input).unwrap_or_else(|_| "{}".to_string()),
        ),
    };
    VisualNodeConfig {
        agent_name,
        task,
        tool_name,
        tool_input_json,
        output_var: step.output_var.clone().unwrap_or_default(),
        timeout_minutes: step.timeout_minutes,
        retry_on_failure: step.retry_on_failure,
        retry_backoff_ms: step.retry_backoff_ms,
        retry_max_delay_ms: step.retry_max_delay_ms,
        on_failure: match step.on_failure {
            agentos_pipeline::definition::OnFailure::Fail => "fail".to_string(),
            agentos_pipeline::definition::OnFailure::Skip => "skip".to_string(),
            agentos_pipeline::definition::OnFailure::UseDefault => "use_default".to_string(),
        },
        default_value: step.default_value.clone().unwrap_or_default(),
    }
}

fn humanize_step_label(step: &PipelineStep) -> String {
    match &step.action {
        StepAction::Agent { agent, .. } => format!("Agent: {}", agent),
        StepAction::Tool { tool, .. } => format!("Tool: {}", tool),
    }
}

fn default_graph(name: &str) -> VisualPipelineGraph {
    VisualPipelineGraph {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        description: String::new(),
        output: None,
        max_cost_usd: None,
        max_wall_time_minutes: None,
        nodes: vec![
            VisualNode {
                id: START_NODE_ID.to_string(),
                node_type: VisualNodeType::Start,
                label: "Start".to_string(),
                x: 40,
                y: 220,
                config: VisualNodeConfig::default(),
            },
            VisualNode {
                id: END_NODE_ID.to_string(),
                node_type: VisualNodeType::End,
                label: "End".to_string(),
                x: 820,
                y: 220,
                config: VisualNodeConfig::default(),
            },
        ],
        edges: Vec::new(),
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_pipeline::definition::OnFailure;

    #[test]
    fn visual_graph_round_trip_preserves_steps_and_meta() {
        let graph = VisualPipelineGraph {
            name: "review-docs".to_string(),
            version: "1.2.3".to_string(),
            description: "Pipeline generated by the builder".to_string(),
            output: None,
            max_cost_usd: Some(3.5),
            max_wall_time_minutes: Some(15),
            nodes: vec![
                VisualNode {
                    id: START_NODE_ID.to_string(),
                    node_type: VisualNodeType::Start,
                    label: "Start".to_string(),
                    x: 10,
                    y: 20,
                    config: VisualNodeConfig::default(),
                },
                VisualNode {
                    id: "collect".to_string(),
                    node_type: VisualNodeType::Agent,
                    label: "Collect".to_string(),
                    x: 250,
                    y: 120,
                    config: VisualNodeConfig {
                        agent_name: "researcher".to_string(),
                        task: "Summarize {{input}}".to_string(),
                        output_var: "summary".to_string(),
                        on_failure: "skip".to_string(),
                        ..VisualNodeConfig::default()
                    },
                },
                VisualNode {
                    id: "notify".to_string(),
                    node_type: VisualNodeType::Tool,
                    label: "Notify".to_string(),
                    x: 520,
                    y: 120,
                    config: VisualNodeConfig {
                        tool_name: "notify_user".to_string(),
                        tool_input_json: "{\n  \"message\": \"{{summary}}\"\n}".to_string(),
                        output_var: "sent".to_string(),
                        on_failure: "use_default".to_string(),
                        default_value: "queued".to_string(),
                        ..VisualNodeConfig::default()
                    },
                },
                VisualNode {
                    id: END_NODE_ID.to_string(),
                    node_type: VisualNodeType::End,
                    label: "End".to_string(),
                    x: 820,
                    y: 160,
                    config: VisualNodeConfig::default(),
                },
            ],
            edges: vec![
                VisualEdge {
                    id: "edge-1".to_string(),
                    source: START_NODE_ID.to_string(),
                    target: "collect".to_string(),
                },
                VisualEdge {
                    id: "edge-2".to_string(),
                    source: "collect".to_string(),
                    target: "notify".to_string(),
                },
                VisualEdge {
                    id: "edge-3".to_string(),
                    source: "notify".to_string(),
                    target: END_NODE_ID.to_string(),
                },
            ],
        };

        let definition = visual_graph_to_pipeline(&graph).expect("graph converts");
        assert_eq!(definition.name, "review-docs");
        assert_eq!(definition.output.as_deref(), Some("sent"));
        assert_eq!(definition.steps.len(), 2);
        assert_eq!(definition.steps[0].depends_on, Vec::<String>::new());
        assert_eq!(definition.steps[0].on_failure, OnFailure::Skip);
        assert_eq!(definition.steps[1].depends_on, vec!["collect".to_string()]);
        assert_eq!(definition.steps[1].on_failure, OnFailure::UseDefault);
        assert_eq!(definition.steps[1].default_value.as_deref(), Some("queued"));

        let yaml = pipeline_to_yaml_with_meta(&definition, &graph).expect("yaml serializes");
        assert!(yaml.contains("meta:"));
        assert!(yaml.contains("positions:"));

        let round_trip = pipeline_yaml_to_visual_graph(&yaml).expect("yaml imports");
        assert_eq!(round_trip.name, graph.name);
        assert_eq!(round_trip.version, graph.version);
        assert_eq!(round_trip.nodes.len(), graph.nodes.len());
        assert_eq!(
            round_trip
                .nodes
                .iter()
                .find(|n| n.id == "notify")
                .map(|n| (n.x, n.y)),
            Some((520, 120))
        );
    }

    #[test]
    fn json_serialization_uses_node_type_field_name() {
        let graph = default_graph("json-test");
        let json = serde_json::to_string(&graph).unwrap();
        // Frontend JS reads `node.node_type` — ensure Rust serializes to that name
        assert!(
            json.contains("\"node_type\""),
            "JSON must use 'node_type' field for frontend compatibility"
        );
        // Ensure the old `type` rename is gone
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let first_node = &parsed["nodes"][0];
        assert!(
            first_node.get("node_type").is_some(),
            "node_type field must be present in serialized JSON"
        );
        assert!(
            first_node.get("type").is_none(),
            "'type' field must not appear in serialized JSON"
        );
        // Round-trip: deserialize back
        let round_trip: VisualPipelineGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip, graph);
    }

    #[test]
    fn compute_depth_handles_cycle_without_stack_overflow() {
        let step_a = PipelineStep {
            id: "a".to_string(),
            action: StepAction::Agent {
                agent: "x".into(),
                task: "t".into(),
            },
            output_var: None,
            depends_on: vec!["b".to_string()],
            timeout_minutes: None,
            retry_on_failure: None,
            retry_backoff_ms: None,
            retry_max_delay_ms: None,
            on_failure: OnFailure::Fail,
            default_value: None,
        };
        let step_b = PipelineStep {
            id: "b".to_string(),
            action: StepAction::Agent {
                agent: "y".into(),
                task: "t".into(),
            },
            output_var: None,
            depends_on: vec!["a".to_string()],
            timeout_minutes: None,
            retry_on_failure: None,
            retry_backoff_ms: None,
            retry_max_delay_ms: None,
            on_failure: OnFailure::Fail,
            default_value: None,
        };

        let mut steps_by_id = HashMap::new();
        steps_by_id.insert("a", &step_a);
        steps_by_id.insert("b", &step_b);
        let mut cache = HashMap::new();
        let mut in_progress = HashSet::new();

        // Must terminate without stack overflow
        let depth_a = compute_depth(&step_a, &steps_by_id, &mut cache, &mut in_progress);
        let depth_b = compute_depth(&step_b, &steps_by_id, &mut cache, &mut in_progress);
        assert!(depth_a > 0);
        assert!(depth_b > 0);
        // in_progress should be empty after all calls complete
        assert!(in_progress.is_empty());
    }

    #[test]
    fn invalid_tool_json_is_rejected() {
        let graph = VisualPipelineGraph {
            name: "bad-json".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            output: None,
            max_cost_usd: None,
            max_wall_time_minutes: None,
            nodes: vec![
                VisualNode {
                    id: START_NODE_ID.to_string(),
                    node_type: VisualNodeType::Start,
                    label: "Start".to_string(),
                    x: 0,
                    y: 0,
                    config: VisualNodeConfig::default(),
                },
                VisualNode {
                    id: "tool-step".to_string(),
                    node_type: VisualNodeType::Tool,
                    label: "Tool".to_string(),
                    x: 0,
                    y: 0,
                    config: VisualNodeConfig {
                        tool_name: "notify_user".to_string(),
                        tool_input_json: "{invalid".to_string(),
                        on_failure: "fail".to_string(),
                        ..VisualNodeConfig::default()
                    },
                },
                VisualNode {
                    id: END_NODE_ID.to_string(),
                    node_type: VisualNodeType::End,
                    label: "End".to_string(),
                    x: 0,
                    y: 0,
                    config: VisualNodeConfig::default(),
                },
            ],
            edges: vec![],
        };

        let error = visual_graph_to_pipeline(&graph).expect_err("bad json should fail");
        assert!(error.contains("invalid"));
    }
}
