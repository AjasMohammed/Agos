use crate::handlers::render;
use crate::state::AppState;
use agentos_types::{DeliveryChannel, NotificationID};
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio::sync::broadcast::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

/// GET /notifications — full inbox page.
pub async fn inbox(State(state): State<AppState>, jar: CookieJar) -> Response {
    let notifications = match state
        .kernel
        .notification_router
        .inbox()
        .list(false, 50)
        .await
    {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::error!(error = %e, "Failed to load notification inbox");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load notifications",
            )
                .into_response();
        }
    };

    let unread_count = notifications.iter().filter(|m| !m.read).count();
    let notifs_ctx: Vec<_> = notifications.iter().map(notification_to_ctx).collect();
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Notifications",
        breadcrumbs => vec![context! { label => "Notifications" }],
        notifications => notifs_ctx,
        unread_count,
        csrf_token,
    };
    render(&state.templates, "notifications/inbox.html", ctx)
}

/// GET /notifications/unread-count — lightweight JSON endpoint for the bell counter.
pub async fn unread_count(State(state): State<AppState>) -> axum::response::Json<UnreadCount> {
    let count = state
        .kernel
        .notification_router
        .inbox()
        .count_unread()
        .await;
    axum::response::Json(UnreadCount { count })
}

#[derive(Serialize)]
pub struct UnreadCount {
    pub count: usize,
}

/// GET /notifications/{id} — detail view; also marks the notification as read.
pub async fn get_notification(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Response {
    let notification_id: NotificationID = match id.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid notification ID").into_response(),
    };

    let msg = match state
        .kernel
        .notification_router
        .inbox()
        .get(&notification_id)
        .await
    {
        Ok(Some(m)) => m,
        Ok(None) => return (StatusCode::NOT_FOUND, "Notification not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch notification");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    // Mark as read (best-effort — don't fail the render if this fails).
    state
        .kernel
        .notification_router
        .inbox()
        .mark_read(&notification_id)
        .await
        .ok();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => format!("Notification — {}", msg.subject),
        breadcrumbs => vec![
            context! { label => "Notifications", href => "/notifications" },
            context! { label => msg.subject.chars().take(40).collect::<String>() },
        ],
        notification => notification_to_ctx(&msg),
        csrf_token,
    };
    render(&state.templates, "notifications/detail.html", ctx)
}

/// POST /notifications/{id}/respond — submit a user response to a Question.
pub async fn respond_to_notification(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<RespondForm>,
) -> Response {
    let notification_id: NotificationID = match id.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid notification ID").into_response(),
    };

    let response_text = form.response.trim().to_string();
    if response_text.is_empty() {
        return (StatusCode::BAD_REQUEST, "Response cannot be empty").into_response();
    }
    if response_text.len() > 8192 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "Response exceeds maximum allowed length",
        )
            .into_response();
    }

    let response = agentos_types::UserResponse {
        text: response_text.clone(),
        responded_at: chrono::Utc::now(),
        channel: DeliveryChannel::web(),
    };

    match state
        .kernel
        .notification_router
        .route_response(notification_id, response)
        .await
    {
        Ok(()) => {
            // Return an HTMX partial confirming the response was sent.
            axum::response::Html(format!(
                r#"<article id="notif-{}" class="notification-responded">
                    <p><strong>Response submitted:</strong> {}</p>
                </article>"#,
                html_escape(&notification_id.to_string()),
                html_escape(&response_text),
            ))
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, notification_id = %id, "Failed to route notification response");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "Failed to submit response",
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct RespondForm {
    pub response: String,
}

/// GET /notifications/stream — SSE endpoint that pushes new notifications to the browser.
///
/// The browser connects here once on page load.  Each new notification triggers a
/// `notification-new` SSE event with JSON payload for the bell counter and toast.
pub async fn notification_stream(
    State(state): State<AppState>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(32);
    let mut broadcast_rx = state.notification_tx.subscribe();

    // Translate broadcast messages to SSE Events in a background task.
    // When the SSE connection closes, `tx.send` will fail and the task exits.
    tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(payload) => match serde_json::to_string(&payload) {
                    Ok(data) => {
                        if tx
                            .send(Event::default().event("notification-new").data(data))
                            .await
                            .is_err()
                        {
                            break; // client disconnected
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to serialize SSE notification payload");
                    }
                },
                Err(RecvError::Lagged(n)) => {
                    // Subscriber was too slow; tell the browser to reload the count.
                    let _ = tx
                        .send(
                            Event::default()
                                .event("notification-reload")
                                .data(n.to_string()),
                        )
                        .await;
                }
                Err(RecvError::Closed) => break,
            }
        }
    });

    let stream = ReceiverStream::new(rx).map(Ok::<Event, Infallible>);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a `UserMessage` into a MiniJinja context value.
fn notification_to_ctx(msg: &agentos_types::UserMessage) -> minijinja::Value {
    let from_str = match &msg.from {
        agentos_types::NotificationSource::Agent(id) => {
            let s = id.to_string();
            format!("agent:{}", &s[..s.len().min(8)])
        }
        agentos_types::NotificationSource::Kernel => "kernel".to_string(),
        agentos_types::NotificationSource::System => "system".to_string(),
    };

    let (kind_tag, question, options) = match &msg.kind {
        agentos_types::UserMessageKind::Question {
            question, options, ..
        } => ("question", Some(question.clone()), options.clone()),
        agentos_types::UserMessageKind::TaskComplete { .. } => ("task_complete", None, None),
        agentos_types::UserMessageKind::StatusUpdate { .. } => ("status_update", None, None),
        agentos_types::UserMessageKind::Notification => ("notification", None, None),
    };

    let response_text = msg.response.as_ref().map(|r| r.text.clone());
    let expires_at = msg
        .expires_at
        .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string());

    context! {
        id => msg.id.to_string(),
        from => from_str,
        priority => msg.priority.to_string().to_ascii_lowercase(),
        subject => msg.subject.clone(),
        body => msg.body.clone(),
        kind_tag,
        question,
        options,
        response_text,
        read => msg.read,
        created_at => msg.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        expires_at,
        requires_response => msg.interaction.is_some() && msg.response.is_none(),
    }
}

/// Escape HTML special characters to prevent XSS in inline HTML responses.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
