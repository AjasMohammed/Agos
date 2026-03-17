use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
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
        return super::render(&state.templates, "partials/pipeline_row.html", ctx);
    }

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Pipelines",
        pipelines => pipeline_rows,
        csrf_token,
    };
    super::render(&state.templates, "pipelines.html", ctx)
}

#[derive(Deserialize)]
pub struct RunForm {
    pub pipeline_name: String,
    pub input: String,
    pub agent_name: Option<String>,
}

pub async fn run(State(state): State<AppState>, axum::Form(form): axum::Form<RunForm>) -> Response {
    // If an agent name is explicitly provided it must not be blank; None means "use kernel default".
    if let Some(ref n) = form.agent_name {
        if n.trim().is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                "Agent name must not be empty. Omit the field to use the kernel default.",
            )
                .into_response();
        }
    }

    // Sanity-check input sizes before passing into the pipeline engine.
    if form.pipeline_name.len() > 256 {
        return (StatusCode::BAD_REQUEST, "Pipeline name too long").into_response();
    }
    if form.input.len() > 65536 {
        return (StatusCode::BAD_REQUEST, "Pipeline input too long").into_response();
    }

    match state
        .kernel
        .run_pipeline(
            form.pipeline_name.clone(),
            form.input.clone(),
            true,
            form.agent_name.clone(),
        )
        .await
    {
        Ok(data) => {
            if let Some(run_id) = data.get("id").and_then(|v| v.as_str()) {
                tracing::info!(run_id = %run_id, pipeline = %form.pipeline_name, "Pipeline started from web UI");
            }
            axum::response::Redirect::to("/pipelines").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, pipeline = %form.pipeline_name, "Pipeline run failed");
            (StatusCode::BAD_REQUEST, "Failed to start pipeline run").into_response()
        }
    }
}
