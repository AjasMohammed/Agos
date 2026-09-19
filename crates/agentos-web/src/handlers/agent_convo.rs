use crate::convo_inflight::{ConvoStreamEvent, InFlightConvo};
use crate::state::AppState;
use agentos_kernel::convo_runner::ConvoEvent;
use agentos_kernel::kernel::ChatStreamEvent;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use futures::FutureExt as _;
use minijinja::context;
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

const AGENT_COLORS: &[&str] = &[
    "#4f86c6", "#e07b39", "#5aab61", "#a855f7", "#e84393", "#0ea5e9", "#f59e0b", "#10b981",
    "#ef4444", "#8b5cf6",
];

fn agent_color(agent_name: &str, participants: &[String]) -> &'static str {
    let idx = participants
        .iter()
        .position(|n| n == agent_name)
        .unwrap_or(0);
    AGENT_COLORS[idx % AGENT_COLORS.len()]
}

#[derive(Deserialize)]
pub struct NewConvoForm {
    pub topic: String,
    /// Comma-separated agent names.
    pub participants: String,
    pub max_turns: Option<u32>,
}

/// GET /agent-chat — list all conversations.
pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Response {
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let convos = {
        let store = Arc::clone(&state.convo_store);
        tokio::task::spawn_blocking(move || store.list_convos())
            .await
            .unwrap_or_else(|_| Ok(vec![]))
            .unwrap_or_default()
    };

    let agents: Vec<_> = match state.service.list_agents().await {
        Ok(list) => list
            .iter()
            .map(|a| context! { name => a.name.clone(), status => a.status.clone() })
            .collect(),
        Err(e) => {
            tracing::error!("Failed to list agents for agent-chat: {e}");
            vec![]
        }
    };

    let convos_ctx: Vec<_> = convos
        .iter()
        .map(|c| {
            let participant_colors: Vec<_> = c
                .participants
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    context! {
                        name => name.clone(),
                        color => AGENT_COLORS[i % AGENT_COLORS.len()],
                        initial => name.chars().next().unwrap_or('?').to_uppercase().to_string(),
                    }
                })
                .collect();
            let short_id: String = c.id.chars().take(8).collect();
            context! {
                id => c.id.clone(),
                topic => c.topic.clone(),
                participants => participant_colors,
                max_turns => c.max_turns,
                status => c.status.clone(),
                updated_at => c.updated_at.clone(),
                short_id,
            }
        })
        .collect();

    let ctx = context! {
        page_title => "Agent Chat",
        csrf_token,
        breadcrumbs => vec![context! { label => "Agent Chat" }],
        convos => convos_ctx,
        agents,
    };
    super::render(&state.templates, "agent_convo_list.html", ctx)
}

/// POST /agent-chat/new — create and start a conversation.
pub async fn new_convo(State(state): State<AppState>, Form(form): Form<NewConvoForm>) -> Response {
    let participants: Vec<String> = form
        .participants
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let max_turns = form.max_turns.unwrap_or(8).clamp(2, 50);

    // Topic, name, duplicate and online checks live in the service so the REST
    // and web paths cannot drift apart.
    let summary = match state
        .service
        .create_agent_chat(form.topic, participants, max_turns)
        .await
    {
        Ok(s) => s,
        Err(agentos_api::ApiError::BadRequest(msg)) => {
            return (StatusCode::BAD_REQUEST, msg).into_response();
        }
        Err(e) => {
            tracing::error!("Failed to create convo: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create conversation",
            )
                .into_response();
        }
    };
    let convo_id = summary.id;
    let topic = summary.topic;
    let participants = summary.participants;

    let inflight = match state.inflight_convos.try_start(&convo_id) {
        Some(h) => h,
        None => {
            // A fresh UUID should never collide with an active entry, but if it somehow
            // does, mark the DB row as error so it doesn't linger as "running" forever.
            let store = Arc::clone(&state.convo_store);
            let id = convo_id.clone();
            let _ = tokio::task::spawn_blocking(move || store.set_status(&id, "error")).await;
            return (StatusCode::CONFLICT, "Conversation already running").into_response();
        }
    };

    spawn_conversation_orchestrator(
        state,
        convo_id.clone(),
        topic,
        participants,
        max_turns,
        inflight,
    );

    Redirect::to(&format!("/agent-chat/{}", convo_id)).into_response()
}

/// GET /agent-chat/{id} — view a conversation page.
pub async fn detail(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(convo_id): Path<String>,
) -> Response {
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    if uuid::Uuid::parse_str(&convo_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid conversation ID").into_response();
    }

    let convo = {
        let store = Arc::clone(&state.convo_store);
        let id = convo_id.clone();
        match tokio::task::spawn_blocking(move || store.get_convo(&id)).await {
            Ok(Ok(Some(c))) => c,
            Ok(Ok(None)) => {
                return (StatusCode::NOT_FOUND, "Conversation not found").into_response()
            }
            _ => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response(),
        }
    };

    let turns = {
        let store = Arc::clone(&state.convo_store);
        let id = convo_id.clone();
        tokio::task::spawn_blocking(move || store.get_turns(&id))
            .await
            .unwrap_or_else(|_| Ok(vec![]))
            .unwrap_or_default()
    };

    // Active only when DB status is still "running" AND the orchestrator slot is live.
    // Checking DB status means the Stop button disappears immediately after the user
    // clicks Stop (which flips the DB to "stopped") rather than waiting for the
    // orchestrator to observe the cancellation and call mark_done().
    let is_active = !matches!(convo.status.as_str(), "complete" | "stopped" | "error")
        && state
            .inflight_convos
            .get(&convo_id)
            .map(|inf| !inf.is_done())
            .unwrap_or(false);

    let participant_colors: Vec<_> = convo
        .participants
        .iter()
        .enumerate()
        .map(|(i, name)| {
            context! {
                name => name.clone(),
                color => AGENT_COLORS[i % AGENT_COLORS.len()],
                initial => name.chars().next().unwrap_or('?').to_uppercase().to_string(),
            }
        })
        .collect();

    let turns_ctx: Vec<_> = turns
        .iter()
        .map(|t| {
            let color = agent_color(&t.agent_name, &convo.participants);
            // Operator rows posted through the REST API / panel.
            let agent_name = if t.agent_name == agentos_kernel::convo_store::USER_SPEAKER {
                "You"
            } else {
                t.agent_name.as_str()
            };
            let initial = agent_name
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string();
            context! {
                turn_number => t.turn_number,
                agent_name,
                content => t.content.clone(),
                tool_call_count => t.tool_call_count,
                color,
                initial,
                created_at => t.created_at.clone(),
            }
        })
        .collect();

    let short_id: String = convo_id.chars().take(8).collect();
    let ctx = context! {
        page_title => format!("Agent Chat — {}", short_id),
        csrf_token,
        breadcrumbs => vec![
            context! { label => "Agent Chat", href => "/agent-chat" },
            context! { label => short_id.clone() },
        ],
        convo_id,
        topic => convo.topic.clone(),
        participants => participant_colors,
        max_turns => convo.max_turns,
        status => convo.status.clone(),
        turns => turns_ctx,
        is_active,
        short_id,
    };
    super::render(&state.templates, "agent_convo.html", ctx)
}

/// POST /agent-chat/{id}/stop — stop a running conversation.
pub async fn stop(State(state): State<AppState>, Path(convo_id): Path<String>) -> Response {
    if uuid::Uuid::parse_str(&convo_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid conversation ID").into_response();
    }
    // Mark as stopped in DB — the orchestrator observes this at the next iteration boundary.
    let store = Arc::clone(&state.convo_store);
    let id = convo_id.clone();
    let _ = tokio::task::spawn_blocking(move || store.set_status(&id, "stopped")).await;

    // Also mark the inflight slot as done immediately so the SSE stream terminates
    // and the detail page's is_active flag goes false without waiting for the
    // orchestrator to finish the current (possibly long) LLM call.
    if let Some(inflight) = state.inflight_convos.get(&convo_id) {
        inflight.mark_done();
    }

    Redirect::to(&format!("/agent-chat/{}", convo_id)).into_response()
}

/// GET /agent-chat/{id}/stream — SSE stream for a live conversation.
pub async fn stream(
    State(state): State<AppState>,
    Path(convo_id): Path<String>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, Response> {
    if uuid::Uuid::parse_str(&convo_id).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid conversation ID").into_response());
    }

    let inflight = match state.inflight_convos.get(&convo_id) {
        Some(h) => h,
        None => {
            // The inflight entry is gone — either it expired (POST_DONE_RETENTION elapsed)
            // or it was never created. Check the DB: if the convo is in a terminal state,
            // return 204 so the browser's EventSource stops reconnecting. A 410 GONE causes
            // the browser to stop too, but 204 is semantically cleaner for "nothing to stream".
            let store = Arc::clone(&state.convo_store);
            let id = convo_id.clone();
            let status = tokio::task::spawn_blocking(move || {
                store
                    .get_convo(&id)
                    .ok()
                    .flatten()
                    .map(|c| c.status)
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            if matches!(status.as_str(), "complete" | "stopped" | "error") {
                return Err(StatusCode::NO_CONTENT.into_response());
            }
            return Err((StatusCode::GONE, "No active conversation for this ID").into_response());
        }
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<ConvoStreamEvent>(128);
    {
        let inf = Arc::clone(&inflight);
        tokio::spawn(async move {
            inf.subscribe_events(tx).await;
        });
    }

    let sse_stream = ReceiverStream::new(rx).map(|event| {
        let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, Infallible>(Event::default().event("convo-stream").data(data))
    });

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}

// ── Orchestrator ─────────────────────────────────────────────────────────────

fn spawn_conversation_orchestrator(
    state: AppState,
    convo_id: String,
    topic: String,
    participants: Vec<String>,
    max_turns: u32,
    inflight: Arc<InFlightConvo>,
) {
    let inflight_convos = Arc::clone(&state.inflight_convos);
    let inf_guard = Arc::clone(&inflight);
    let cid_guard = convo_id.clone();
    tokio::spawn(async move {
        // catch_unwind ensures mark_done + cleanup always fire, even on panic.
        let result = std::panic::AssertUnwindSafe(run_conversation(
            state,
            convo_id,
            topic,
            participants,
            max_turns,
            inflight,
        ))
        .catch_unwind()
        .await;

        if result.is_err() {
            tracing::error!(convo_id = %cid_guard, "Conversation orchestrator panicked — cleaning up");
            inf_guard.mark_done();
            inflight_convos.schedule_cleanup(cid_guard);
        }
    });
}

async fn run_conversation(
    state: AppState,
    convo_id: String,
    topic: String,
    participants: Vec<String>,
    max_turns: u32,
    inflight: Arc<InFlightConvo>,
) {
    // The loop lives in `agentos_kernel::convo_runner`, shared with the REST
    // orchestrator. This surface keeps only what is web-specific: draining the
    // runner's events into the SSE replay buffer, and the in-flight bookkeeping.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ConvoEvent>(64);

    let inflight_pump = Arc::clone(&inflight);
    let pump = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                ConvoEvent::TurnStart { agent, turn } => {
                    inflight_pump.push(ConvoStreamEvent::TurnStart { agent, turn })
                }
                ConvoEvent::Chat { agent, turn, event } => {
                    if let Some(translated) = translate_event(event, &agent, turn) {
                        inflight_pump.push(translated);
                    }
                }
                ConvoEvent::TurnEnd {
                    agent,
                    turn,
                    answer,
                } => inflight_pump.push(ConvoStreamEvent::TurnEnd {
                    agent,
                    turn,
                    answer,
                }),
                ConvoEvent::Error { message } => {
                    inflight_pump.push(ConvoStreamEvent::Error { message })
                }
                ConvoEvent::Done { total_turns } => {
                    inflight_pump.push(ConvoStreamEvent::ConversationDone { total_turns })
                }
            }
        }
    });

    agentos_kernel::convo_runner::run_convo(
        &state.kernel,
        &convo_id,
        &topic,
        &participants,
        max_turns,
        Some(tx),
    )
    .await;

    // The sender is dropped by now, so the pump drains and exits.
    let _ = pump.await;

    inflight.mark_done();
    state.inflight_convos.schedule_cleanup(convo_id);
}

fn translate_event(ev: ChatStreamEvent, agent: &str, turn: u32) -> Option<ConvoStreamEvent> {
    // Reasoning deltas are panel-facing. This conversation's replay buffer has no
    // coalescing and a 2 000-event cap spanning every turn, so forwarding one
    // reasoning-heavy turn would evict all the earlier turns' text.
    if matches!(ev, ChatStreamEvent::Thinking { text: Some(_), .. }) {
        return None;
    }
    Some(match ev {
        ChatStreamEvent::Thinking { iteration, .. } => ConvoStreamEvent::Thinking {
            agent: agent.to_string(),
            turn,
            iteration,
        },
        ChatStreamEvent::TextChunk { text } => ConvoStreamEvent::TextChunk {
            agent: agent.to_string(),
            turn,
            text,
        },
        ChatStreamEvent::ToolStart {
            tool_name,
            iteration,
            ..
        } => ConvoStreamEvent::ToolStart {
            agent: agent.to_string(),
            turn,
            tool_name,
            iteration,
        },
        ChatStreamEvent::ToolResult {
            tool_name,
            result_preview,
            duration_ms,
            success,
        } => ConvoStreamEvent::ToolResult {
            agent: agent.to_string(),
            turn,
            tool_name,
            result_preview,
            duration_ms,
            success,
        },
        ChatStreamEvent::Error { message } => ConvoStreamEvent::Error {
            message: format!("{agent}: {message}"),
        },
        // Done is handled by the orchestrator — skip the duplicate.
        ChatStreamEvent::Done { .. } => return None,
    })
}

/// Render a simple HTML response for the conversation list (partial for HTMX if needed).
pub async fn list_partial(State(state): State<AppState>, jar: CookieJar) -> Response {
    list(State(state), jar).await
}
