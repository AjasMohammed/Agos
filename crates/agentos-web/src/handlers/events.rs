use crate::state::AppState;
use agentos_types::TaskState;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use futures::stream::{self, StreamExt};
use minijinja::context;
use std::convert::Infallible;
use std::time::Duration;

/// SSE endpoint for the dashboard page.
/// Rotates through three named events every 3 s: `dashboard-stats`, `dashboard-agents`,
/// `dashboard-audit`. Each event's data is a rendered HTML partial that replaces the
/// corresponding `sse-swap` target in dashboard.html.
pub async fn dashboard_stream(
    State(state): State<AppState>,
) -> Sse<KeepAliveStream<futures::stream::BoxStream<'static, Result<Event, Infallible>>>> {
    let kernel = state.kernel.clone();
    let templates = state.templates.clone();

    let stream = stream::unfold(0u64, move |tick| {
        let kernel = kernel.clone();
        let templates = templates.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(3)).await;

            let event = match tick % 3 {
                0 => {
                    let agent_count = kernel.agent_registry.read().await.list_online().len();
                    let tool_count = kernel.tool_registry.read().await.list_all().len();
                    let tasks = kernel.scheduler.list_tasks().await;
                    let total_task_count = tasks.len();
                    let active_task_count = tasks
                        .iter()
                        .filter(|t| {
                            matches!(
                                t.state,
                                TaskState::Running | TaskState::Waiting | TaskState::Suspended
                            )
                        })
                        .count();
                    let bg_running = kernel.background_pool.list_running().await.len();
                    let uptime_secs = chrono::Utc::now()
                        .signed_duration_since(kernel.started_at)
                        .num_seconds();
                    let uptime_display = super::dashboard::format_uptime(uptime_secs);
                    let ctx = context! {
                        agent_count,
                        tool_count,
                        active_task_count,
                        total_task_count,
                        bg_running,
                        uptime_display,
                    };
                    let html = render_partial(&templates, "partials/dashboard_stats.html", ctx);
                    Event::default().event("dashboard-stats").data(html)
                }
                1 => {
                    let registry = kernel.agent_registry.read().await;
                    let agents: Vec<_> = registry
                        .list_online()
                        .iter()
                        .map(|a| {
                            context! {
                                name => a.name.clone(),
                                provider => format!("{:?}", a.provider),
                                model => a.model.clone(),
                                status => format!("{:?}", a.status),
                                current_task => a.current_task.as_ref().map(|t| t.to_string()),
                            }
                        })
                        .collect();
                    drop(registry);
                    let ctx = context! { agents };
                    let html = render_partial(&templates, "partials/dashboard_agents.html", ctx);
                    Event::default().event("dashboard-agents").data(html)
                }
                _ => {
                    let audit = kernel.audit.clone();
                    let entries = tokio::task::spawn_blocking(move || {
                        audit.query_recent(10).unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default();
                    let recent_audit: Vec<_> = entries
                        .iter()
                        .map(|e| {
                            context! {
                                timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                                event_type => format!("{:?}", e.event_type),
                                severity => format!("{:?}", e.severity),
                                agent_id => e.agent_id.as_ref().map(|id| id.to_string()),
                            }
                        })
                        .collect();
                    let ctx = context! { recent_audit };
                    let html = render_partial(&templates, "partials/dashboard_audit.html", ctx);
                    Event::default().event("dashboard-audit").data(html)
                }
            };

            Some((Ok(event), tick + 1))
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

/// SSE endpoint for the agents page.
/// Sends a named `agent-update` event every 3 s with a freshly rendered agent card partial.
pub async fn agents_stream(
    State(state): State<AppState>,
) -> Sse<KeepAliveStream<futures::stream::BoxStream<'static, Result<Event, Infallible>>>> {
    let kernel = state.kernel.clone();
    let templates = state.templates.clone();

    let stream = stream::unfold((), move |()| {
        let kernel = kernel.clone();
        let templates = templates.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(3)).await;

            let registry = kernel.agent_registry.read().await;
            let agents: Vec<_> = registry
                .list_online()
                .iter()
                .map(|a| {
                    context! {
                        id => a.id.to_string(),
                        name => a.name.clone(),
                        provider => format!("{:?}", a.provider),
                        model => a.model.clone(),
                        status => format!("{:?}", a.status),
                        description => a.description.clone(),
                        roles => a.roles.clone(),
                        current_task => a.current_task.as_ref().map(|t| t.to_string()),
                        created_at => a.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                        last_active => a.last_active.format("%Y-%m-%d %H:%M:%S").to_string(),
                    }
                })
                .collect();
            drop(registry);

            let ctx = context! { agents };
            let html = render_partial(&templates, "partials/agent_card.html", ctx);

            Some((Ok(Event::default().event("agent-update").data(html)), ()))
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

/// SSE endpoint for the tasks page.
/// Sends a named `task-update` event every 2 s with a freshly rendered task row partial.
pub async fn tasks_stream(
    State(state): State<AppState>,
) -> Sse<KeepAliveStream<futures::stream::BoxStream<'static, Result<Event, Infallible>>>> {
    let kernel = state.kernel.clone();
    let templates = state.templates.clone();

    let stream = stream::unfold((), move |()| {
        let kernel = kernel.clone();
        let templates = templates.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(2)).await;

            let tasks = kernel.scheduler.list_tasks().await;
            let task_rows: Vec<_> = tasks
                .iter()
                .map(|t| {
                    context! {
                        id => t.id.to_string(),
                        state => format!("{:?}", t.state),
                        agent_id => t.agent_id.to_string(),
                        prompt_preview => t.prompt_preview.clone(),
                        created_at => t.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                        tool_calls => t.tool_calls,
                        tokens_used => t.tokens_used,
                        priority => t.priority,
                    }
                })
                .collect();

            let ctx = context! { tasks => task_rows };
            let html = render_partial(&templates, "partials/task_row.html", ctx);

            Some((Ok(Event::default().event("task-update").data(html)), ()))
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

fn render_partial(
    templates: &minijinja::Environment<'static>,
    name: &str,
    ctx: minijinja::Value,
) -> String {
    match templates.get_template(name).and_then(|t| t.render(ctx)) {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, template = name, "SSE partial render failed");
            String::new()
        }
    }
}
