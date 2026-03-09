use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use minijinja::context;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let store = state.kernel.pipeline_engine.store();
    let pipelines = store.list_pipelines().unwrap_or_default();

    let pipeline_rows: Vec<_> = pipelines
        .iter()
        .map(|p| {
            context! {
                name => p.name.clone(),
                version => p.version.clone(),
                description => p.description.clone(),
                step_count => p.step_count,
                installed_at => p.installed_at.clone(),
            }
        })
        .collect();

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { pipelines => pipeline_rows };
        return super::render(&state.templates, "partials/tool_card.html", ctx);
    }

    let ctx = context! {
        page_title => "Pipelines",
        pipelines => pipeline_rows,
    };
    super::render(&state.templates, "pipelines.html", ctx)
}

#[derive(Deserialize)]
pub struct RunForm {
    pub pipeline_name: String,
    pub input: String,
}

pub async fn run(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<RunForm>,
) -> Response {
    let store = state.kernel.pipeline_engine.store();

    // Load the pipeline definition
    let yaml = match store.get_pipeline_yaml(&form.pipeline_name) {
        Ok(y) => y,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                format!("Pipeline not found: {}", e),
            )
                .into_response();
        }
    };

    let definition: agentos_pipeline::PipelineDefinition = match serde_yaml::from_str(&yaml) {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid pipeline YAML: {}", e),
            )
                .into_response();
        }
    };

    let run_id = agentos_types::RunID::new();

    // Create the run record
    let pipeline_run = agentos_pipeline::PipelineRun {
        id: run_id.clone(),
        pipeline_name: form.pipeline_name.clone(),
        input: form.input.clone(),
        status: agentos_pipeline::PipelineRunStatus::Running,
        step_results: std::collections::HashMap::new(),
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        error: None,
    };

    if let Err(e) = store.create_run(&pipeline_run) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create run: {}", e),
        )
            .into_response();
    }

    // Return redirect to pipelines page with status
    axum::response::Redirect::to("/pipelines").into_response()
}
