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
    let pipelines = match state.service.list_pipelines().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list pipelines");
            vec![]
        }
    };

    let pipeline_rows: Vec<_> = pipelines
        .iter()
        .map(|p| {
            context! {
                name => p.name.clone(),
                version => String::new(),
                description => p.description.clone().unwrap_or_default(),
                step_count => p.step_count,
                installed_at => String::new(),
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
        breadcrumbs => vec![context! { label => "Pipelines" }],
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

    use agentos_api::types::RunPipelineRequest;
    let req = RunPipelineRequest {
        name: form.pipeline_name.clone(),
        input: form.input.clone(),
        detach: true,
        agent_name: form.agent_name.clone(),
    };
    match state.service.run_pipeline(req).await {
        Ok(data) => {
            if let Some(run_id) = data.get("id").and_then(|v| v.as_str()) {
                tracing::info!(run_id = %run_id, pipeline = %form.pipeline_name, "Pipeline started from web UI");
            }
            let mut response = axum::response::Redirect::to("/pipelines").into_response();
            let trigger = serde_json::json!({
                "showToast": {"message": format!("Pipeline '{}' started", form.pipeline_name), "type": "success"}
            })
            .to_string();
            if let Ok(hv) = axum::http::HeaderValue::from_str(&trigger) {
                response.headers_mut().insert("HX-Trigger", hv);
            }
            response
        }
        Err(e) => {
            tracing::error!(error = %e, pipeline = %form.pipeline_name, "Pipeline run failed");
            let mut response =
                (StatusCode::BAD_REQUEST, "Failed to start pipeline run").into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Failed to start pipeline","type":"error"}}"#,
                ),
            );
            response
        }
    }
}
