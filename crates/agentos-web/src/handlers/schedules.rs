use crate::state::AppState;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;
use std::str::FromStr;

#[derive(Debug, Deserialize)]
pub struct CreateScheduleForm {
    pub name: String,
    pub agent_name: String,
    pub cron: String,
    pub prompt: String,
}

#[derive(Debug, Deserialize)]
pub struct PreviewScheduleForm {
    pub cron: String,
}

#[derive(Debug, serde::Serialize)]
struct CronPreviewResponse {
    ok: bool,
    message: String,
    next_runs: Vec<String>,
}

pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Response {
    let schedules = state
        .kernel
        .schedule_manager
        .list_jobs()
        .await
        .into_iter()
        .map(|s| {
            context! {
                id => s.id.to_string(),
                name => s.name,
                cron_expression => s.cron_expression,
                state => format!("{:?}", s.state),
                agent_name => s.agent_name,
                task_prompt => s.task_prompt,
                run_count => s.run_count,
                created_at => s.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                last_run_at => s.last_run_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default(),
                next_run_at => s.next_run_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    let agents = {
        let reg = state.kernel.agent_registry.read().await;
        reg.list_all()
            .into_iter()
            .map(|a| context! { name => a.name.clone() })
            .collect::<Vec<_>>()
    };

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Schedules",
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Schedules" },
        ],
        schedules,
        agents,
        csrf_token,
    };
    super::render(&state.templates, "schedules.html", ctx)
}

pub async fn preview(Form(form): Form<PreviewScheduleForm>) -> impl IntoResponse {
    let expr = form.cron.trim();
    if expr.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(CronPreviewResponse {
                ok: false,
                message: "Cron expression is required".to_string(),
                next_runs: Vec::new(),
            }),
        );
    }

    let schedule = match cron::Schedule::from_str(expr) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(CronPreviewResponse {
                    ok: false,
                    message: format!("Invalid cron expression: {e}"),
                    next_runs: Vec::new(),
                }),
            )
        }
    };

    let next_runs = schedule
        .upcoming(chrono::Utc)
        .take(3)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .collect::<Vec<_>>();

    (
        StatusCode::OK,
        axum::Json(CronPreviewResponse {
            ok: true,
            message: "OK".to_string(),
            next_runs,
        }),
    )
}

pub async fn create(
    State(state): State<AppState>,
    Form(form): Form<CreateScheduleForm>,
) -> Response {
    if form.name.trim().is_empty()
        || form.agent_name.trim().is_empty()
        || form.cron.trim().is_empty()
        || form.prompt.trim().is_empty()
    {
        return (StatusCode::BAD_REQUEST, "Missing required fields").into_response();
    }

    match state
        .kernel
        .schedule_manager
        .create_job(
            form.name.trim().to_string(),
            form.cron.trim().to_string(),
            form.agent_name.trim().to_string(),
            form.prompt.trim().to_string(),
            Vec::new(),
        )
        .await
    {
        Ok(_) => Redirect::to("/schedules").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to create schedule: {e}"),
        )
            .into_response(),
    }
}

pub async fn pause(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let parsed: agentos_types::ScheduleID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid schedule ID").into_response(),
    };

    match state.kernel.schedule_manager.pause(&parsed).await {
        Ok(()) => Redirect::to("/schedules").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to pause schedule '{id}': {e}"),
        )
            .into_response(),
    }
}

pub async fn resume(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let parsed: agentos_types::ScheduleID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid schedule ID").into_response(),
    };

    match state.kernel.schedule_manager.resume(&parsed).await {
        Ok(()) => Redirect::to("/schedules").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to resume schedule '{id}': {e}"),
        )
            .into_response(),
    }
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let parsed: agentos_types::ScheduleID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid schedule ID").into_response(),
    };

    match state.kernel.schedule_manager.delete(&parsed).await {
        Ok(()) => Redirect::to("/schedules").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to delete schedule '{id}': {e}"),
        )
            .into_response(),
    }
}
