use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{self, StreamExt};
use minijinja::context;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let tasks = state.kernel.scheduler.list_tasks().await;
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
            }
        })
        .collect();

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { tasks => task_rows };
        return super::render(&state.templates, "partials/task_row.html", ctx);
    }

    let ctx = context! {
        page_title => "Tasks",
        tasks => task_rows,
    };
    super::render(&state.templates, "tasks.html", ctx)
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => {
            return (axum::http::StatusCode::BAD_REQUEST, "Invalid task ID").into_response();
        }
    };

    use axum::response::IntoResponse;
    match state.kernel.scheduler.get_task(&task_id).await {
        Some(task) => {
            let history: Vec<_> = task
                .history
                .iter()
                .map(|msg| {
                    context! {
                        role => format!("{:?}", msg.role),
                        content => msg.content.clone(),
                    }
                })
                .collect();

            let ctx = context! {
                page_title => format!("Task {}", task.id),
                task_id => task.id.to_string(),
                state => format!("{:?}", task.state),
                agent_id => task.agent_id.to_string(),
                prompt => task.original_prompt.clone(),
                created_at => task.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                priority => task.priority,
                history,
            };
            super::render(&state.templates, "task_detail.html", ctx)
        }
        None => (axum::http::StatusCode::NOT_FOUND, "Task not found").into_response(),
    }
}

/// SSE endpoint for live task log streaming.
/// Streams audit events related to the given task.
pub async fn log_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let task_id_str = id.clone();
    let audit = state.kernel.audit.clone();

    // Poll audit log for new task-related events every second
    let stream = stream::unfold(0u32, move |last_count| {
        let audit = audit.clone();
        let tid = task_id_str.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let entries = audit.query_recent(50).unwrap_or_default();
            let relevant: Vec<_> = entries
                .iter()
                .filter(|e| {
                    e.task_id
                        .as_ref()
                        .map(|t| t.to_string() == tid)
                        .unwrap_or(false)
                })
                .collect();

            let count = relevant.len() as u32;
            if count > last_count {
                let new_entries: Vec<_> = relevant
                    .iter()
                    .skip(last_count as usize)
                    .map(|e| {
                        format!(
                            "[{}] {:?} - {}",
                            e.timestamp.format("%H:%M:%S"),
                            e.event_type,
                            e.details
                        )
                    })
                    .collect();
                let data = new_entries.join("\n");
                Some((Ok(Event::default().data(data)), count))
            } else {
                Some((Ok(Event::default().comment("keepalive")), last_count))
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
