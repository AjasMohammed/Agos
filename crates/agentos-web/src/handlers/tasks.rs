use crate::state::AppState;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use futures::stream::{self, StreamExt};
use minijinja::context;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::time::Duration;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
    pub search: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResumeTaskForm {
    pub confirm: String,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
) -> Response {
    use agentos_api::types::TaskFilter;

    let filter = TaskFilter {
        status: query.status.clone().filter(|s| !s.is_empty()),
        agent_name: None,
        offset: None,
        limit: Some(500),
    };
    let (tasks_api, _total) = match state.service.list_tasks(filter).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to list tasks: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to list tasks").into_response();
        }
    };

    let task_rows: Vec<_> = tasks_api
        .iter()
        .filter(|t| {
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
                state => t.status.clone(),
                agent_id => t.agent_name.clone().unwrap_or_default(),
                prompt_preview => t.prompt_preview.clone(),
                created_at => t.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                tool_calls => 0u32,
                tokens_used => 0u64,
                priority => 0u32,
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

    match state.service.cancel_task(task_id).await {
        Ok(()) => {
            // Re-fetch the task via service to render the updated row.
            if let Ok(task) = state.service.get_task(task_id).await {
                let ctx = context! {
                    tasks => vec![context! {
                        id => task.id.to_string(),
                        state => task.status.clone(),
                        agent_id => task.agent_name.clone().unwrap_or_default(),
                        prompt_preview => task.prompt.chars().take(100).collect::<String>(),
                        created_at => task.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                        tool_calls => 0u32,
                        tokens_used => 0u64,
                        priority => 0u32,
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
        Err(e) => {
            tracing::error!(task = %id, error = %e, "Failed to cancel task");
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

/// GET /tasks/{id} — task detail page.
///
/// TODO: Migrate to state.service.get_task() once ApiTaskDetail exposes:
/// - task.history (Vec<IntentMessage> — full turn history for the detail template)
/// - task.original_prompt (untruncated prompt, distinct from prompt_preview)
/// - task.priority (scheduling priority field)
/// - task.agent_id (typed AgentID, not just agent_name)
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
                    let payload_str = serde_json::to_string(&msg.payload).unwrap_or_default();
                    let preview = if payload_str.chars().count() > 80 {
                        format!("{}…", payload_str.chars().take(80).collect::<String>())
                    } else {
                        payload_str
                    };
                    // IntentMessage carries an IntentType (Read/Write/Message/…),
                    // not a chat-style User/Assistant/Tool role. Expose both so the
                    // template can label and class-scope the badge correctly.
                    let intent = format!("{:?}", msg.intent_type);
                    let intent_slug = intent.to_lowercase();
                    context! {
                        intent,
                        intent_slug,
                        preview,
                        payload => msg.payload.clone(),
                        timestamp => msg.timestamp.to_rfc3339(),
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
///
/// TODO: Migrate to state.service.get_task_trace() once ApiTaskTrace exposes the full
/// IterationRecord fields (tool_calls with permission_check, injection_score, snapshot_ref,
/// input_json) that the trace timeline template requires.
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
///
/// TODO: Migrate to state.service.get_task_trace() — same blocker as trace_page above.
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

pub async fn snapshots(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid task ID").into_response(),
    };

    let snapshots = state
        .kernel
        .checkpoint_store
        .list_checkpoints()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.task_id == task_id)
        .map(|c| {
            context! {
                checkpoint_id => c.checkpoint_id,
                task_id => c.task_id.to_string(),
                agent_id => c.agent_id.to_string(),
                step_num => c.step_num,
                updated_at => c.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                schema_version => c.schema_version,
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Task Snapshots",
        breadcrumbs => vec![
            context! { label => "Tasks", href => "/tasks" },
            context! { label => id.clone(), href => format!("/tasks/{}", id) },
            context! { label => "Snapshots" },
        ],
        task_id => id,
        snapshots,
        csrf_token,
    };
    super::render(&state.templates, "task_snapshots.html", ctx)
}

pub async fn resume(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<ResumeTaskForm>,
) -> Response {
    if !form.confirm.trim().eq_ignore_ascii_case("RESUME") {
        return (
            StatusCode::BAD_REQUEST,
            "Confirmation required: type RESUME to restore from checkpoint",
        )
            .into_response();
    }

    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid task ID").into_response(),
    };

    let record = match state.kernel.checkpoint_store.get_latest(&task_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "No checkpoint found for this task").into_response()
        }
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "Failed to load checkpoint");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load checkpoint",
            )
                .into_response();
        }
    };

    let payload: agentos_kernel::checkpoint_store::CheckpointPayload = match serde_json::from_slice(
        &record.state_blob,
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "Failed to deserialize checkpoint payload");
            return (StatusCode::BAD_REQUEST, "Checkpoint payload is invalid").into_response();
        }
    };

    if let Some(existing) = state.kernel.scheduler.get_task(&task_id).await {
        let is_terminal = matches!(
            existing.state,
            agentos_types::TaskState::Complete
                | agentos_types::TaskState::Failed
                | agentos_types::TaskState::Cancelled
        );
        if !is_terminal {
            return (
                StatusCode::CONFLICT,
                "Task already exists in scheduler and is not terminal; refusing resume",
            )
                .into_response();
        }
    }

    let agent = {
        let registry = state.kernel.agent_registry.read().await;
        match registry.get_by_id(&payload.task.agent_id) {
            Some(a) if a.status != agentos_types::AgentStatus::Offline => a.clone(),
            Some(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    "Agent is offline and task cannot be resumed",
                )
                    .into_response()
            }
            None => {
                return (StatusCode::NOT_FOUND, "Agent not found for checkpoint task")
                    .into_response()
            }
        }
    };

    let effective_permissions = {
        let registry = state.kernel.agent_registry.read().await;
        registry.compute_effective_permissions(&agent.id)
    };
    let task_timeout = Duration::from_secs(state.kernel.config.kernel.default_task_timeout_secs);

    let capability_token = match state.kernel.capability_engine.issue_token(
        task_id,
        agent.id,
        BTreeSet::new(),
        BTreeSet::from([
            agentos_types::IntentTypeFlag::Read,
            agentos_types::IntentTypeFlag::Write,
            agentos_types::IntentTypeFlag::Execute,
            agentos_types::IntentTypeFlag::Query,
            agentos_types::IntentTypeFlag::Observe,
            agentos_types::IntentTypeFlag::Message,
            agentos_types::IntentTypeFlag::Delegate,
            agentos_types::IntentTypeFlag::Broadcast,
            agentos_types::IntentTypeFlag::Escalate,
            agentos_types::IntentTypeFlag::Subscribe,
            agentos_types::IntentTypeFlag::Unsubscribe,
        ]),
        effective_permissions,
        task_timeout,
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "Failed to issue capability token for resumed task");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to issue capability token for resumed task",
            )
                .into_response();
        }
    };

    let resumed_task = agentos_types::AgentTask {
        id: task_id,
        state: agentos_types::TaskState::Queued,
        agent_id: agent.id,
        capability_token,
        assigned_llm: Some(agent.id),
        priority: payload.task.priority,
        created_at: chrono::Utc::now(),
        started_at: None,
        timeout: task_timeout,
        original_prompt: payload.task.original_prompt,
        history: Vec::new(),
        parent_task: payload.task.parent_task,
        reasoning_hints: payload.task.reasoning_hints,
        max_iterations: payload.task.max_iterations,
        trigger_source: None,
        autonomous: payload.task.autonomous,
        parent_task_id: payload.task.parent_task_id,
        spawn_depth: payload.task.spawn_depth,
        is_team_coordinator: payload.task.is_team_coordinator,
        skip_checkpoint: payload.task.skip_checkpoint,
        thinking_level: payload.task.thinking_level,
    };

    let _ = state
        .kernel
        .context_manager
        .replace_context(&task_id, payload.context.window)
        .await;
    state.kernel.scheduler.enqueue(resumed_task).await;
    let _ = state.kernel.audit.append(agentos_audit::AuditEntry {
        timestamp: chrono::Utc::now(),
        trace_id: agentos_types::TraceID::new(),
        event_type: agentos_audit::AuditEventType::CheckpointRestored,
        agent_id: Some(agent.id),
        task_id: Some(task_id),
        tool_id: None,
        details: serde_json::json!({
            "step_restored": record.step_num,
            "checkpoint_id": record.checkpoint_id,
            "source": "web",
        }),
        severity: agentos_audit::AuditSeverity::Info,
        reversible: false,
        rollback_ref: None,
    });

    Redirect::to(&format!("/tasks/{}", task_id)).into_response()
}

/// SSE endpoint for live task log streaming.
/// Streams audit events related to the given task using monotonic ID-based tracking.
///
/// TODO: This handler intentionally uses state.kernel.audit and state.kernel.scheduler
/// directly because it must clone both into a `'static` move closure for stream::unfold.
/// KernelService methods are async trait methods and cannot be easily moved into the
/// stream::unfold closure without holding an Arc ref across the future boundary.
/// Migrate once KernelService grows a dedicated log_stream method returning a Stream.
pub async fn log_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
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

    // Resume support: if the browser auto-reconnects after a transient disconnect,
    // it sends Last-Event-ID with the most recent audit row it saw. Start polling
    // from that cursor so we do not replay the entire task history on every reconnect.
    let resume_from: i64 = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .filter(|v: &i64| *v >= 0)
        .unwrap_or(0);

    // Poll audit log using monotonic row ID tracking.
    // - Hard-stop stale streams after 30 minutes so abandoned browser tabs stop polling.
    // - Back off from 1s to 10s intervals after 2 minutes of idle (no new entries).
    // First iteration uses Duration::ZERO so the browser receives data immediately
    // and transitions from CONNECTING to OPEN without a 1-second gap.
    let started_at = tokio::time::Instant::now();
    let stream = stream::unfold(
        Some((
            resume_from,
            tokio::time::Instant::now(),
            Duration::ZERO,
        )),
        move |state_opt| {
            let audit = audit.clone();
            let scheduler = scheduler.clone();
            let started_at = started_at;
            async move {
                let (last_seen_id, last_activity, interval) = match state_opt {
                    Some(s) => s,
                    None => {
                        // Previous iteration saw terminal state; send closing event.
                        return Some((
                            vec![Ok(Event::default().event("done").data("stream closed"))],
                            None,
                        ));
                    }
                };

                tokio::time::sleep(interval).await;

                let stream_timed_out =
                    started_at.elapsed() >= Duration::from_secs(30 * 60 /* 30 minutes */);

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
                let is_terminal = stream_timed_out
                    || match scheduler.get_task(&task_id).await {
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

                // Idle backoff: 1s while entries are arriving, 10s after 2 min of silence.
                let now = tokio::time::Instant::now();
                let new_last_activity = if entries.is_empty() {
                    last_activity
                } else {
                    now
                };
                let idle_threshold = Duration::from_secs(120);
                let next_interval = if now.duration_since(new_last_activity) > idle_threshold {
                    Duration::from_secs(10)
                } else {
                    Duration::from_secs(1)
                };
                let next_state = if is_terminal {
                    None
                } else {
                    Some((max_id, new_last_activity, next_interval))
                };

                if entries.is_empty() {
                    let event = if is_terminal {
                        let reason = if stream_timed_out {
                            "stream timeout reached"
                        } else {
                            "stream closed"
                        };
                        Event::default().event("done").data(reason)
                    } else {
                        Event::default().comment("keepalive")
                    };
                    return Some((vec![Ok(event)], next_state));
                }

                let mut events: Vec<Result<Event, Infallible>> = Vec::new();
                let mut log_lines: Vec<String> = Vec::new();

                for (row_id, entry) in &entries {
                    if entry.event_type == agentos_audit::AuditEventType::TestFindingCaptured {
                        events.push(Ok(Event::default()
                            .id(row_id.to_string())
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
                        .id(max_id.to_string())
                        .data(log_lines.join("\n"))));
                }

                if is_terminal {
                    events.push(Ok(Event::default().event("done").data("stream closed")));
                }

                Some((events, next_state))
            }
        },
    )
    .flat_map(stream::iter);

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

/// GET /api/tasks/{id}/context/{idx}/raw — return a single context window message
/// payload as raw pretty-printed JSON. Linked from the task detail page so users can
/// inspect large payloads without embedding megabytes of JSON inline in the DOM.
pub async fn context_raw(
    State(state): State<AppState>,
    Path((id, idx)): Path<(String, usize)>,
) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid task ID").into_response(),
    };

    let task = match state.kernel.scheduler.get_task(&task_id).await {
        Some(t) => t,
        None => return (StatusCode::NOT_FOUND, "Task not found").into_response(),
    };

    let msg = match task.history.get(idx) {
        Some(m) => m,
        None => {
            return (StatusCode::NOT_FOUND, "Context message index out of range").into_response()
        }
    };

    let body = serde_json::to_string_pretty(&msg.payload).unwrap_or_default();
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}
