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

            let short_id = task.id.to_string().chars().take(8).collect::<String>();
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

/// Render the execution trace timeline for a completed task.
pub async fn trace_page(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid task ID").into_response();
        }
    };

    match state.kernel.trace_collector.get_trace(&task_id).await {
        Ok(Some(trace)) => {
            let short_id = trace
                .task_id
                .to_string()
                .chars()
                .take(8)
                .collect::<String>();
            let short_id = short_id.as_str();
            let iterations: Vec<_> = trace
                .iterations
                .iter()
                .map(|it| {
                    let tool_calls: Vec<_> = it
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            let status = if !tc.permission_check.granted {
                                "denied"
                            } else if tc.error.is_some() {
                                "error"
                            } else {
                                "ok"
                            };
                            context! {
                                tool_name => tc.tool_name.clone(),
                                status,
                                duration_ms => tc.duration_ms,
                                error => tc.error.clone().unwrap_or_default(),
                                deny_reason => tc.permission_check.deny_reason.clone().unwrap_or_default(),
                                injection_score => tc.injection_score.map(|s| format!("{:.2}", s)).unwrap_or_default(),
                                has_snapshot => tc.snapshot_ref.is_some(),
                                input_preview => {
                                    let s = tc.input_json.to_string();
                                    if s.chars().count() > 120 {
                                        format!("{}…", s.chars().take(120).collect::<String>())
                                    } else {
                                        s
                                    }
                                },
                            }
                        })
                        .collect();
                    context! {
                        num => it.iteration,
                        model => it.model.clone(),
                        stop_reason => it.stop_reason.clone(),
                        input_tokens => it.input_tokens,
                        output_tokens => it.output_tokens,
                        tool_calls,
                    }
                })
                .collect();

            let elapsed_secs = trace
                .finished_at
                .map(|fin| (fin - trace.started_at).num_milliseconds() as f64 / 1000.0)
                .map(|s| format!("{:.1}s", s))
                .unwrap_or_default();

            let ctx = context! {
                page_title => format!("Trace {}", short_id),
                breadcrumbs => vec![
                    context! { label => "Tasks", href => "/tasks" },
                    context! { label => format!("Task {}", short_id), href => format!("/tasks/{}", trace.task_id) },
                    context! { label => "Trace" },
                ],
                task_id => trace.task_id.to_string(),
                agent_id => trace.agent_id.to_string(),
                status => trace.status.clone(),
                prompt_preview => trace.prompt_preview.clone(),
                started_at => trace.started_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                elapsed_secs,
                total_input_tokens => trace.total_input_tokens,
                total_output_tokens => trace.total_output_tokens,
                total_cost_usd => if trace.total_cost_usd > 0.0 { format!("${:.6}", trace.total_cost_usd) } else { String::new() },
                iterations,
            };
            super::render(&state.templates, "task_trace.html", ctx)
        }
        Ok(None) => (StatusCode::NOT_FOUND, "No trace found for this task").into_response(),
        Err(e) => {
            tracing::error!(task = %id, error = %e, "Failed to fetch task trace");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch trace").into_response()
        }
    }
}

/// JSON API — returns the raw trace for a task.
pub async fn trace_json(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid task ID").into_response();
        }
    };

    match state.kernel.trace_collector.get_trace(&task_id).await {
        Ok(Some(trace)) => axum::Json(trace).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "No trace found for this task").into_response(),
        Err(e) => {
            tracing::error!(task = %id, error = %e, "Failed to fetch task trace");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch trace").into_response()
        }
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
