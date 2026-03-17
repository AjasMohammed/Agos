use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
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
    jar: CookieJar,
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

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Tasks",
        tasks => task_rows,
        csrf_token,
    };
    super::render(&state.templates, "tasks.html", ctx)
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => {
            return (axum::http::StatusCode::BAD_REQUEST, "Invalid task ID").into_response();
        }
    };

    match state.kernel.scheduler.get_task(&task_id).await {
        Some(task) => {
            let history: Vec<_> = task
                .history
                .iter()
                .map(|msg| {
                    context! {
                        role => format!("{:?}", msg.intent_type),
                        content => serde_json::to_string(&msg.payload).unwrap_or_default(),
                    }
                })
                .collect();

            let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

            let ctx = context! {
                page_title => format!("Task {}", task.id),
                task_id => task.id.to_string(),
                state => format!("{:?}", task.state),
                agent_id => task.agent_id.to_string(),
                prompt => task.original_prompt.clone(),
                created_at => task.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                priority => task.priority,
                history,
                csrf_token,
            };
            super::render(&state.templates, "task_detail.html", ctx)
        }
        None => (axum::http::StatusCode::NOT_FOUND, "Task not found").into_response(),
    }
}

/// SSE endpoint for live task log streaming.
/// Streams audit events related to the given task using monotonic ID-based tracking.
pub async fn log_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Sse<KeepAliveStream<futures::stream::BoxStream<'static, Result<Event, Infallible>>>> {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => {
            return Sse::new(
                stream::once(async {
                    Ok::<Event, Infallible>(Event::default().data("Error: invalid task ID"))
                })
                .boxed(),
            )
            .keep_alive(KeepAlive::default());
        }
    };

    let audit = state.kernel.audit.clone();
    let scheduler = state.kernel.scheduler.clone();

    // Poll audit log every second, tracking by monotonic row ID.
    // State: Some(last_seen_id) = active; None = terminal (emit "done" then close).
    let stream = stream::unfold(Some(0i64), move |state_opt| {
        let audit = audit.clone();
        let scheduler = scheduler.clone();
        async move {
            let last_seen_id = match state_opt {
                Some(id) => id,
                None => {
                    // Previous iteration saw terminal state; send closing event.
                    return Some((
                        Ok(Event::default().event("done").data("stream closed")),
                        None,
                    ));
                }
            };

            tokio::time::sleep(Duration::from_secs(1)).await;

            let audit_clone = audit.clone();
            let entries = match tokio::task::spawn_blocking(move || {
                audit_clone.query_since_for_task(&task_id, last_seen_id, 100)
            })
            .await
            {
                Ok(Ok(e)) => e,
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "SSE audit query error");
                    vec![]
                }
                Err(e) => {
                    tracing::warn!(error = %e, "SSE audit query panicked");
                    vec![]
                }
            };

            // Check terminal state after audit query to capture final entries.
            let is_terminal = match scheduler.get_task(&task_id).await {
                None => true, // task not found — treat as terminal
                Some(task) => {
                    use agentos_types::TaskState;
                    matches!(
                        task.state,
                        TaskState::Complete | TaskState::Failed | TaskState::Cancelled
                    )
                }
            };

            if !entries.is_empty() {
                let max_id = entries.last().map(|(id, _)| *id).unwrap_or(last_seen_id);
                let data: Vec<String> = entries
                    .iter()
                    .map(|(_, e)| {
                        format!(
                            "[{}] {:?} - {}",
                            e.timestamp.format("%H:%M:%S"),
                            e.event_type,
                            e.details
                        )
                    })
                    .collect();
                let next_state = if is_terminal { None } else { Some(max_id) };
                Some((
                    Ok(Event::default()
                        .data(data.join("\n"))
                        .id(max_id.to_string())),
                    next_state,
                ))
            } else if is_terminal {
                Some((
                    Ok(Event::default().event("done").data("stream closed")),
                    None,
                ))
            } else {
                Some((
                    Ok(Event::default().comment("keepalive")),
                    Some(last_seen_id),
                ))
            }
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}
