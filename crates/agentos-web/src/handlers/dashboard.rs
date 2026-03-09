use crate::state::AppState;
use axum::extract::State;
use axum::response::Response;
use minijinja::context;

pub async fn index(State(state): State<AppState>) -> Response {
    let agent_count = state.kernel.agent_registry.read().await.list_all().len();
    let tool_count = state.kernel.tool_registry.read().await.list_all().len();
    let task_count = state.kernel.scheduler.running_count().await;
    let tasks = state.kernel.scheduler.list_tasks().await;
    let recent_audit = state
        .kernel
        .audit
        .query_recent(10)
        .unwrap_or_default();
    let uptime = chrono::Utc::now()
        .signed_duration_since(state.kernel.started_at)
        .num_seconds();
    let bg_running = state.kernel.background_pool.list_running().await.len();

    let ctx = context! {
        page_title => "Dashboard",
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
