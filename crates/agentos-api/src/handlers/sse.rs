//! Server-Sent Events (SSE) streaming — a per-channel realtime stream for
//! clients that prefer `EventSource` over the WebSocket endpoint. Both are fed
//! by the kernel's realtime event broadcast (see `ws::broadcaster`).

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Extension;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;

/// Query parameters for the realtime SSE stream.
#[derive(Debug, Clone, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventStreamQuery {
    /// Channel to subscribe to (`tasks`, `agents`, `audit`, `schedules`,
    /// `system`, or `events`). Subscribing requires the matching `<channel>:r`
    /// scope on the API key.
    pub channel: String,
}

/// `GET /api/v1/events/stream?channel=<name>` — SSE stream of realtime events.
///
/// Each event is emitted as an SSE frame whose `event:` is the event name and
/// `data:` is the JSON payload. The connection is kept alive with periodic
/// comments. Mirrors the WebSocket `subscribe` flow over a one-way stream.
#[utoipa::path(
    get,
    path = "/api/v1/events/stream",
    tag = "events",
    operation_id = "events_stream",
    params(EventStreamQuery),
    responses(
        (status = 200, description = "SSE stream of realtime events (text/event-stream)"),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 403, description = "Forbidden", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn events_stream(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(q): Query<EventStreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // Subscribing to a channel requires its read scope. Use the same channel→scope
    // derivation as the WS subscribe path so both transports gate identically
    // (base resource before any `:id` suffix; `agent-chat` maps to `chat`).
    require_permission(
        &key,
        &crate::ws::session::channel_required_scope(&q.channel),
    )?;

    let channel = q.channel.clone();
    let prefix = format!("{channel}:");
    let rx = svc.subscribe_realtime();
    let stream = BroadcastStream::new(rx).filter_map(move |res| match res {
        // Forward events on the requested channel (exact or `channel:id` prefix).
        Ok(ev) if ev.channel == channel || ev.channel.starts_with(&prefix) => {
            Some(Ok(Event::default()
                .event(&ev.event)
                .data(ev.data.to_string())))
        }
        // Event on a different channel → filter out.
        Ok(_) => None,
        // Consumer fell behind: surface a `lagged` marker so the client knows it
        // missed `n` events and can resync, while keeping the connection alive.
        // The broadcast is intentionally lossy — there is no replay buffer, so
        // `Last-Event-ID` resume is deliberately unsupported.
        Err(BroadcastStreamRecvError::Lagged(n)) => Some(Ok(Event::default()
            .event("lagged")
            .data(serde_json::json!({ "skipped": n }).to_string()))),
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
