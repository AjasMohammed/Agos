use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
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
    pub search: Option<String>,
    pub status: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
) -> Response {
    let tasks = state.kernel.scheduler.list_tasks().await;
    let task_rows: Vec<_> = tasks
        .iter()
        .filter(|t| {
            if let Some(ref status) = query.status {
                if !status.is_empty() {
                    let state_str = format!("{:?}", t.state).to_lowercase();
                    if !state_str.contains(&status.to_lowercase()) {
                        return false;
                    }
                }
            }
            if let Some(ref search) = query.search {
                if !search.is_empty()
                    && !t
                        .prompt_preview
                        .to_lowercase()
                        .contains(&search.to_lowercase())
                {
                    return false;
                }
            }
            true
        })
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

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { tasks => task_rows };
        return super::render(&state.templates, "partials/task_row.html", ctx);
    }

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Tasks",
        breadcrumbs => vec![context! { label => "Tasks" }],
        tasks => task_rows,
        csrf_token,
    };
    super::render(&state.templates, "tasks.html", ctx)
}

pub async fn cancel(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid task ID").into_response();
        }
    };

    match state
        .kernel
        .scheduler
        .update_state(&task_id, agentos_types::TaskState::Cancelled)
        .await
    {
        Ok(()) => {
            if let Some(task) = state.kernel.scheduler.get_task(&task_id).await {
                let ctx = context! {
                    tasks => vec![context! {
                        id => task.id.to_string(),
                        state => format!("{:?}", task.state),
                        agent_id => task.agent_id.to_string(),
                        prompt_preview => task.original_prompt.chars().take(100).collect::<String>(),
                        created_at => task.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                        tool_calls => 0u32,
                        tokens_used => 0u64,
                        priority => task.priority,
                    }],
                };
                let mut response = super::render(&state.templates, "partials/task_row.html", ctx);
                response.headers_mut().insert(
                    "HX-Trigger",
                    axum::http::HeaderValue::from_static(
                        r#"{"showToast":{"message":"Task cancelled","type":"info"}}"#,
                    ),
                );
                return response;
            }
            let mut response = StatusCode::NO_CONTENT.into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Task cancelled","type":"info"}}"#,
                ),
            );
            response
        }
        Err(msg) => {
            tracing::error!(task = %id, error = %msg, "Failed to cancel task");
            let mut response = (StatusCode::BAD_REQUEST, "Failed to cancel task").into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Failed to cancel task","type":"error"}}"#,
                ),
            );
            response
        }
    }
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

            let short_id = task.id.to_string()[..8].to_string();
            let ctx = context! {
                page_title => format!("Task {}", task.id),
                breadcrumbs => vec![
                    context! { label => "Tasks", href => "/tasks" },
                    context! { label => format!("Task {}", short_id) },
                ],
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
                        vec![Ok(Event::default().event("done").data("stream closed"))],
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

            let max_id = entries.last().map(|(id, _)| *id).unwrap_or(last_seen_id);
            let next_state = if is_terminal { None } else { Some(max_id) };

            if entries.is_empty() {
                let event = if is_terminal {
                    Event::default().event("done").data("stream closed")
                } else {
                    Event::default().comment("keepalive")
                };
                return Some((vec![Ok(event)], next_state));
            }

            let mut events: Vec<Result<Event, Infallible>> = Vec::new();
            let mut log_lines: Vec<String> = Vec::new();

            for (_, entry) in &entries {
                if entry.event_type == agentos_audit::AuditEventType::TestFindingCaptured {
                    events.push(Ok(Event::default()
                        .event("finding")
                        .data(entry.details.to_string())));
                } else {
                    log_lines.push(format!(
                        "[{}] {:?} - {}",
                        entry.timestamp.format("%H:%M:%S"),
                        entry.event_type,
                        entry.details
                    ));
                }
            }

            if !log_lines.is_empty() {
                events.push(Ok(Event::default()
                    .data(log_lines.join("\n"))
                    .id(max_id.to_string())));
            }

            if is_terminal {
                events.push(Ok(Event::default().event("done").data("stream closed")));
            }

            Some((events, next_state))
        }
    })
    .flat_map(stream::iter);

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}
