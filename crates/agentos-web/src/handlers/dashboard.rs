use crate::state::AppState;
use agentos_api::types::AuditFilter;
use agentos_types::TaskState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn index(State(state): State<AppState>, jar: CookieJar) -> Response {
    let (agents, agent_count) = build_agent_list(&state).await;
    let tool_count = match state.service.list_tools().await {
        Ok(tools) => tools.len(),
        Err(e) => {
            tracing::error!("Failed to list tools for dashboard: {e}");
            0
        }
    };
    let (tasks, task_summary) = build_task_summary(&state).await;
    // TODO(streaming): background_pool and started_at are kernel-internal; not in KernelService
    let uptime_secs = state.service.get_uptime().await.as_secs() as i64;
    let uptime_display = format_uptime(uptime_secs);
    let bg_running = state.kernel.background_pool.list_running().await.len();
    let recent_audit = fetch_recent_audit(&state, 10).await;
    let recent_audit = match recent_audit {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("dashboard audit query failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Dashboard",
        breadcrumbs => vec![context! { label => "Dashboard" }],
        csrf_token,
        agent_count,
        agents,
        tool_count,
        active_task_count => task_summary.running,
        total_task_count => tasks,
        task_summary => context! {
            queued => task_summary.queued,
            running => task_summary.running,
            completed => task_summary.completed,
            failed => task_summary.failed,
        },
        recent_audit,
        uptime_secs,
        uptime_display,
        bg_running,
    };

    super::render(&state.templates, "dashboard.html", ctx)
}

pub async fn stats_partial(State(state): State<AppState>) -> Response {
    let agent_count = match state.service.list_agents().await {
        Ok(agents) => agents.len(),
        Err(e) => {
            tracing::error!("Failed to list agents for stats partial: {e}");
            0
        }
    };
    let tool_count = match state.service.list_tools().await {
        Ok(tools) => tools.len(),
        Err(e) => {
            tracing::error!("Failed to list tools for stats partial: {e}");
            0
        }
    };
    let (total_task_count, task_summary) = build_task_summary(&state).await;
    let uptime_secs = state.service.get_uptime().await.as_secs() as i64;
    let uptime_display = format_uptime(uptime_secs);
    // TODO(streaming): background_pool not exposed in KernelService
    let bg_running = state.kernel.background_pool.list_running().await.len();

    let ctx = context! {
        agent_count,
        tool_count,
        active_task_count => task_summary.running,
        total_task_count,
        uptime_display,
        bg_running,
    };
    super::render(&state.templates, "partials/dashboard_stats.html", ctx)
}

pub async fn agents_partial(State(state): State<AppState>) -> Response {
    let (agents, _) = build_agent_list(&state).await;
    let ctx = context! { agents };
    super::render(&state.templates, "partials/dashboard_agents.html", ctx)
}

pub async fn tasks_partial(State(state): State<AppState>) -> Response {
    let (_, task_summary) = build_task_summary(&state).await;
    let ctx = context! {
        task_summary => context! {
            queued => task_summary.queued,
            running => task_summary.running,
            completed => task_summary.completed,
            failed => task_summary.failed,
        }
    };
    super::render(&state.templates, "partials/dashboard_tasks.html", ctx)
}

pub async fn recent_audit_partial(State(state): State<AppState>) -> Response {
    let recent_audit = match fetch_recent_audit(&state, 10).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("dashboard audit partial query failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };
    let ctx = context! { recent_audit };
    super::render(&state.templates, "partials/dashboard_audit.html", ctx)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

struct TaskSummary {
    queued: u32,
    running: u32,
    completed: u32,
    failed: u32,
}

async fn build_agent_list(state: &AppState) -> (Vec<minijinja::Value>, usize) {
    let agents_api = match state.service.list_agents().await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Failed to list agents: {e}");
            vec![]
        }
    };
    let agents: Vec<_> = agents_api
        .iter()
        .map(|a| {
            context! {
                name => a.name.clone(),
                provider => a.provider.clone(),
                model => a.model.clone(),
                status => a.status.clone(),
                current_task => Option::<String>::None,
            }
        })
        .collect();
    let count = agents.len();
    (agents, count)
}

async fn build_task_summary(state: &AppState) -> (usize, TaskSummary) {
    // TODO: Replace with state.service.get_dashboard_summary().task_counts once
    // DashboardSummary.TaskCounts exposes queued and suspended state breakdowns.
    // Currently TaskCounts only tracks running/completed/failed/total (kernel_impl.rs
    // lines 592-604), so we fall back to kernel.scheduler to get the queued/suspended
    // distinction that the dashboard template renders.
    let tasks = state.kernel.scheduler.list_tasks().await;
    let mut summary = TaskSummary {
        queued: 0,
        running: 0,
        completed: 0,
        failed: 0,
    };
    for t in &tasks {
        match t.state {
            TaskState::Queued => summary.queued += 1,
            TaskState::Running | TaskState::Waiting | TaskState::Suspended => summary.running += 1,
            TaskState::Complete => summary.completed += 1,
            TaskState::Failed | TaskState::Cancelled => summary.failed += 1,
        }
    }
    (tasks.len(), summary)
}

async fn fetch_recent_audit(
    state: &AppState,
    limit: u32,
) -> Result<Vec<minijinja::Value>, agentos_api::ApiError> {
    let filter = AuditFilter {
        limit: Some(limit),
        ..Default::default()
    };
    let entries = state.service.query_audit(filter).await?;
    Ok(entries
        .iter()
        .map(|e| {
            context! {
                timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                event_type => e.event_type.trim_matches('"').to_string(),
                severity => String::new(),
                agent_id => e.agent_id.clone(),
            }
        })
        .collect())
}

pub(crate) fn format_uptime(secs: i64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{}d {}h {}m", days, hours, mins)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}
