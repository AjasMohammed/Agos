use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn index(State(state): State<AppState>, jar: CookieJar) -> Response {
    let agent_count = state.kernel.agent_registry.read().await.list_all().len();
    let tool_count = state.kernel.tool_registry.read().await.list_all().len();
    let task_count = state.kernel.scheduler.running_count().await;
    let tasks = state.kernel.scheduler.list_tasks().await;
    let audit = state.kernel.audit.clone();
    let recent_audit = match tokio::task::spawn_blocking(move || audit.query_recent(10)).await {
        Ok(result) => result.unwrap_or_default(),
        Err(e) => {
            tracing::error!("dashboard audit query panicked: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };
    let uptime = chrono::Utc::now()
        .signed_duration_since(state.kernel.started_at)
        .num_seconds();
    let bg_running = state.kernel.background_pool.list_running().await.len();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Dashboard",
        csrf_token,
        agent_count,
        tool_count,
        active_task_count => task_count,
        total_task_count => tasks.len(),
        recent_audit => recent_audit.iter().map(|e| context! {
            timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            event_type => format!("{:?}", e.event_type),
            severity => format!("{:?}", e.severity),
            agent_id => e.agent_id.as_ref().map(|id| id.to_string()),
        }).collect::<Vec<_>>(),
        uptime_secs => uptime,
        bg_running,
    };

    super::render(&state.templates, "dashboard.html", ctx)
}
