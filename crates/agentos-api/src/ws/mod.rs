//! WebSocket endpoint — channel subscriptions, real-time events, bidirectional
//! actions (chat, task cancel).
//!
//! Upgrade at `GET /v1/ws?token=agos_<key>`.

pub mod broadcaster;
pub mod protocol;
pub mod session;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Extension;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::api_key::ApiKeyStore;
use crate::error::ApiError;
use crate::service::KernelService;
use broadcaster::WsBroadcaster;
use protocol::{ClientFrame, ServerFrame};
use session::WsSession;

/// Query parameters for the WebSocket upgrade request.
#[derive(Debug, Deserialize)]
pub struct WsParams {
    /// API key (`agos_<hex>`) used for authentication.
    pub token: String,
}

/// `GET /v1/ws?token=agos_...` — Upgrade to WebSocket.
pub async fn ws_upgrade(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key_store): Extension<ApiKeyStore>,
    Extension(broadcaster): Extension<WsBroadcaster>,
    Query(params): Query<WsParams>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    // Validate API key from query param.
    let _record = key_store
        .validate(&params.token)
        .await
        .ok_or(ApiError::Unauthorized)?;

    Ok(ws.on_upgrade(move |socket| handle_connection(socket, svc, broadcaster)))
}

/// Main connection handler — spawns read/write loops and heartbeat.
async fn handle_connection(
    socket: WebSocket,
    svc: Arc<dyn KernelService>,
    broadcaster: WsBroadcaster,
) {
    let (ws_sink, mut ws_stream) = socket.split();
    let (outbound_tx, outbound_rx) = mpsc::channel::<ServerFrame>(256);

    let mut session = WsSession::new(outbound_tx.clone());

    // Spawn write loop: outbound channel → WebSocket sink.
    let write_handle = tokio::spawn(write_loop(ws_sink, outbound_rx));

    // Spawn heartbeat: send pong every 30s to keep connection alive.
    let heartbeat_tx = outbound_tx.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            if heartbeat_tx.send(ServerFrame::Pong).await.is_err() {
                break;
            }
        }
    });

    // Read loop: WebSocket stream → session handler.
    while let Some(Ok(msg)) = ws_stream.next().await {
        match msg {
            Message::Text(text) => match serde_json::from_str::<ClientFrame>(&text) {
                Ok(frame) => {
                    session.handle_frame(frame, &*svc, &broadcaster).await;
                }
                Err(e) => {
                    let _ = outbound_tx
                        .send(ServerFrame::Error {
                            code: "INVALID_FRAME".into(),
                            message: e.to_string(),
                        })
                        .await;
                }
            },
            Message::Close(_) => break,
            _ => {} // Ignore binary/ping/pong frames
        }
    }

    // Cleanup.
    heartbeat_handle.abort();
    write_handle.abort();
    let sub_ids = session.subscription_ids();
    broadcaster.remove_all_for_sender(&sub_ids).await;
}

/// Drains the outbound channel and writes JSON frames to the WebSocket sink.
async fn write_loop(mut sink: SplitSink<WebSocket, Message>, mut rx: mpsc::Receiver<ServerFrame>) {
    while let Some(frame) = rx.recv().await {
        let json = match serde_json::to_string(&frame) {
            Ok(j) => j,
            Err(_) => continue,
        };
        if sink.send(Message::Text(json.into())).await.is_err() {
            break;
        }
    }
}
