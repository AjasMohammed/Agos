---
title: "Phase 5: WebSocket Layer"
tags:
  - api
  - websocket
  - v3
  - phase-5
date: 2026-03-30
status: planned
effort: 3d
priority: high
---

# Phase 5: WebSocket Layer

> Build a WebSocket endpoint at `/api/v1/ws` with channel-based subscriptions, real-time event fan-out, and bidirectional actions (chat streaming, task cancellation).

---

## Why This Phase

REST handles request/response well, but external consumers need real-time updates: task progress, agent status changes, streaming LLM chat responses, and event subscriptions. SSE is one-directional and already serves the HTML UI; WebSocket gives programmatic consumers bidirectional communication with mid-stream cancellation.

## Current State

- REST API at `/api/v1/` (Phases 3-4)
- 6 SSE endpoints serve the HTML UI (`/events/dashboard`, `/events/agents`, `/events/tasks`, `/tasks/{id}/logs/stream`, `/chat/{id}/stream`, `/notifications/stream`)
- Kernel has `event_bus: Arc<EventBus>` and `status_update_sender: broadcast::Sender<StatusUpdate>` for internal event distribution
- No WebSocket support

## Target State

- `GET /api/v1/ws?token=<jwt>` — WebSocket upgrade endpoint
- Channel-based subscription protocol (JSON frames)
- 7 subscription channels: dashboard, agents, tasks, tasks:{id}, notifications, pipelines:{run_id}, costs
- 4 bidirectional actions: chat.send, chat.cancel, task.cancel, notification.respond
- `WsBroadcaster` wired to kernel's event_bus and status_update_sender
- Per-connection backpressure and heartbeat
- Integration tests

## Detailed Subtasks

### 1. WebSocket protocol types

**New file: `crates/agentos-api/src/ws/protocol.rs`**

```rust
/// Client → Server frames
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientFrame {
    #[serde(rename = "subscribe")]
    Subscribe {
        channel: String,
        #[serde(default)]
        filter: serde_json::Value,
    },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { subscription_id: String },
    #[serde(rename = "chat.send")]
    ChatSend {
        session_id: String,
        message: String,
    },
    #[serde(rename = "chat.cancel")]
    ChatCancel { session_id: String },
    #[serde(rename = "task.cancel")]
    TaskCancel { task_id: String },
    #[serde(rename = "notification.respond")]
    NotificationRespond { id: String, text: String },
    #[serde(rename = "ping")]
    Ping,
}

/// Server → Client frames
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ServerFrame {
    #[serde(rename = "subscribed")]
    Subscribed {
        channel: String,
        subscription_id: String,
    },
    #[serde(rename = "unsubscribed")]
    Unsubscribed { subscription_id: String },
    #[serde(rename = "event")]
    Event {
        channel: String,
        event: String,
        data: serde_json::Value,
    },
    #[serde(rename = "chat.chunk")]
    ChatChunk { session_id: String, delta: String },
    #[serde(rename = "chat.done")]
    ChatDone {
        session_id: String,
        tool_calls: Vec<serde_json::Value>,
    },
    #[serde(rename = "chat.cancelled")]
    ChatCancelled { session_id: String },
    #[serde(rename = "error")]
    Error { code: String, message: String },
    #[serde(rename = "pong")]
    Pong,
}
```

### 2. WebSocket session

**New file: `crates/agentos-api/src/ws/session.rs`**

```rust
pub struct WsSession {
    claims: AuthClaims,
    subscriptions: HashMap<String, Subscription>,  // sub_id → Subscription
    next_sub_id: u64,
    outbound_tx: mpsc::Sender<ServerFrame>,
    chat_cancellations: HashMap<String, CancellationToken>,
}

struct Subscription {
    channel: String,
    filter: serde_json::Value,
}

impl WsSession {
    pub fn new(claims: AuthClaims, outbound_tx: mpsc::Sender<ServerFrame>) -> Self { ... }

    pub async fn handle_frame(
        &mut self,
        frame: ClientFrame,
        service: &dyn KernelService,
        broadcaster: &WsBroadcaster,
    ) -> Result<(), ApiError> {
        match frame {
            ClientFrame::Subscribe { channel, filter } => {
                // Validate channel name
                // Check permissions (e.g., "tasks:r" for tasks channel)
                // Register with broadcaster
                // Send Subscribed frame
                let sub_id = self.next_sub_id();
                self.subscriptions.insert(sub_id.clone(), Subscription { channel: channel.clone(), filter });
                broadcaster.register(sub_id.clone(), channel.clone(), self.outbound_tx.clone());
                self.send(ServerFrame::Subscribed { channel, subscription_id: sub_id }).await;
            }
            ClientFrame::Unsubscribe { subscription_id } => {
                if let Some(sub) = self.subscriptions.remove(&subscription_id) {
                    broadcaster.unregister(&subscription_id);
                    self.send(ServerFrame::Unsubscribed { subscription_id }).await;
                }
            }
            ClientFrame::ChatSend { session_id, message } => {
                self.claims.require("chat:w")?;
                let cancel_token = CancellationToken::new();
                self.chat_cancellations.insert(session_id.clone(), cancel_token.clone());
                let tx = self.outbound_tx.clone();
                let svc = service.clone(); // requires service to be cloneable or use Arc
                tokio::spawn(async move {
                    // Stream chat response, sending ChatChunk frames
                    // On completion, send ChatDone
                    // On cancellation, send ChatCancelled
                });
            }
            ClientFrame::ChatCancel { session_id } => {
                if let Some(token) = self.chat_cancellations.remove(&session_id) {
                    token.cancel();
                }
            }
            ClientFrame::TaskCancel { task_id } => {
                self.claims.require("tasks:w")?;
                let id = task_id.parse().map_err(|_| ApiError::BadRequest("invalid task ID".into()))?;
                service.cancel_task(id).await?;
            }
            ClientFrame::NotificationRespond { id, text } => {
                self.claims.require("notifications:w")?;
                let nid = id.parse().map_err(|_| ApiError::BadRequest("invalid notification ID".into()))?;
                service.respond_to_notification(NotificationResponse { id: nid, text }).await?;
            }
            ClientFrame::Ping => {
                self.send(ServerFrame::Pong).await;
            }
        }
        Ok(())
    }
}
```

### 3. WebSocket broadcaster

**New file: `crates/agentos-api/src/ws/broadcaster.rs`**

```rust
/// Fans out kernel events to subscribed WebSocket sessions
pub struct WsBroadcaster {
    subscriptions: Arc<RwLock<HashMap<String, BroadcastEntry>>>,
}

struct BroadcastEntry {
    channel: String,
    sender: mpsc::Sender<ServerFrame>,
}

impl WsBroadcaster {
    pub fn new() -> Self { ... }

    pub fn register(&self, sub_id: String, channel: String, sender: mpsc::Sender<ServerFrame>) {
        self.subscriptions.write().unwrap().insert(sub_id, BroadcastEntry { channel, sender });
    }

    pub fn unregister(&self, sub_id: &str) {
        self.subscriptions.write().unwrap().remove(sub_id);
    }

    /// Start background task that reads from kernel event sources and fans out
    pub fn start(
        self: Arc<Self>,
        mut event_rx: broadcast::Receiver<EventBusMessage>,
        mut status_rx: broadcast::Receiver<StatusUpdate>,
        mut notification_rx: broadcast::Receiver<NotificationSsePayload>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Ok(event) = event_rx.recv() => {
                        let channel = map_event_to_channel(&event);
                        let event_name = event.event_type_name();
                        let data = serde_json::to_value(&event).unwrap_or_default();
                        self.broadcast(channel, event_name, data).await;
                    }
                    Ok(status) = status_rx.recv() => {
                        // Map StatusUpdate to appropriate channel
                        let (channel, event_name, data) = map_status_update(&status);
                        self.broadcast(channel, event_name, data).await;
                    }
                    Ok(notif) = notification_rx.recv() => {
                        let data = serde_json::to_value(&notif).unwrap_or_default();
                        self.broadcast("notifications", "notification.new", data).await;
                    }
                }
            }
        })
    }

    async fn broadcast(&self, channel: &str, event: &str, data: serde_json::Value) {
        let subs = self.subscriptions.read().unwrap();
        let mut dead_subs = Vec::new();
        for (sub_id, entry) in subs.iter() {
            if entry.channel == channel || channel_matches(&entry.channel, channel) {
                let frame = ServerFrame::Event {
                    channel: channel.to_string(),
                    event: event.to_string(),
                    data: data.clone(),
                };
                if entry.sender.try_send(frame).is_err() {
                    dead_subs.push(sub_id.clone());
                }
            }
        }
        drop(subs);
        // Clean up dead subscriptions
        if !dead_subs.is_empty() {
            let mut subs = self.subscriptions.write().unwrap();
            for id in dead_subs { subs.remove(&id); }
        }
    }
}

/// Match "tasks" channel to "tasks:abc-123" subscription
fn channel_matches(subscribed: &str, event_channel: &str) -> bool {
    event_channel.starts_with(subscribed) && event_channel[subscribed.len()..].starts_with(':')
}

/// Map kernel EventBusMessage to a WebSocket channel name
fn map_event_to_channel(event: &EventBusMessage) -> &str {
    // TaskCreated, TaskUpdated, TaskCompleted, TaskFailed → "tasks"
    // AgentConnected, AgentDisconnected → "agents"
    // CostUpdated → "costs"
    // PipelineStepStarted, PipelineStepCompleted → "pipelines:{run_id}"
    // etc.
}
```

### 4. WebSocket upgrade handler

**New file: `crates/agentos-api/src/ws/mod.rs`**

```rust
pub async fn upgrade_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<WsQueryParams>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    // Validate JWT from query param or Authorization header
    let claims = state.jwt_manager.verify(&params.token)
        .map_err(|_| ApiError::Unauthorized)?;

    Ok(ws.on_upgrade(move |socket| handle_connection(socket, claims, state)))
}

async fn handle_connection(
    socket: WebSocket,
    claims: AuthClaims,
    state: Arc<ApiState>,
) {
    let (ws_sink, ws_stream) = socket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<ServerFrame>(256);
    let mut session = WsSession::new(claims, outbound_tx);

    // Write loop: outbound_rx → ws_sink
    let write_handle = tokio::spawn(async move {
        while let Some(frame) = outbound_rx.recv().await {
            let json = serde_json::to_string(&frame).unwrap();
            if ws_sink.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    // Heartbeat: send ping every 30s
    let heartbeat_tx = session.outbound_tx.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            if heartbeat_tx.send(ServerFrame::Pong).await.is_err() {
                break;
            }
        }
    });

    // Read loop: ws_stream → session.handle_frame()
    let mut last_pong = Instant::now();
    while let Some(Ok(msg)) = ws_stream.next().await {
        match msg {
            Message::Text(text) => {
                match serde_json::from_str::<ClientFrame>(&text) {
                    Ok(frame) => {
                        if matches!(frame, ClientFrame::Ping) {
                            last_pong = Instant::now();
                        }
                        if let Err(e) = session.handle_frame(frame, &*state.service, &state.broadcaster).await {
                            let _ = session.send(ServerFrame::Error {
                                code: e.error_code().to_string(),
                                message: e.to_string(),
                            }).await;
                        }
                    }
                    Err(e) => {
                        let _ = session.send(ServerFrame::Error {
                            code: "INVALID_FRAME".into(),
                            message: e.to_string(),
                        }).await;
                    }
                }
            }
            Message::Close(_) => break,
            _ => {} // ignore binary frames
        }

        // Disconnect if no pong in 90s
        if last_pong.elapsed() > Duration::from_secs(90) {
            break;
        }
    }

    // Cleanup
    heartbeat_handle.abort();
    write_handle.abort();
    // Unregister all subscriptions
    for sub_id in session.subscriptions.keys() {
        state.broadcaster.unregister(sub_id);
    }
}

#[derive(Deserialize)]
pub struct WsQueryParams {
    pub token: String,
}
```

### 5. Wire broadcaster to kernel events

In `crates/agentos-web/src/server.rs` (or `agentos-api` init):

```rust
// Create broadcaster
let broadcaster = Arc::new(WsBroadcaster::new());

// Subscribe to kernel event sources
let event_rx = kernel.event_bus.subscribe();
let status_rx = kernel.status_update_sender.subscribe();
let notification_rx = notification_tx.subscribe();

// Start fan-out background task
broadcaster.clone().start(event_rx, status_rx, notification_rx);

// Pass to ApiState
let api_state = Arc::new(ApiState {
    service,
    jwt_manager,
    key_store,
    audit: kernel.audit.clone(),
    broadcaster,
});
```

### 6. Mount WebSocket route

In `rest/mod.rs` router builder:
```rust
.route("/ws", get(ws::upgrade_handler))
```

The `/ws` route is outside the auth middleware layer — auth happens during the upgrade via query param JWT.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-api/src/ws/mod.rs` | Upgrade handler, connection lifecycle |
| `crates/agentos-api/src/ws/session.rs` | WsSession with subscription and action handling |
| `crates/agentos-api/src/ws/protocol.rs` | ClientFrame, ServerFrame types |
| `crates/agentos-api/src/ws/broadcaster.rs` | WsBroadcaster fan-out from kernel events |
| `crates/agentos-api/src/rest/mod.rs` | Mount `/ws` route |
| `crates/agentos-web/src/server.rs` | Wire broadcaster to kernel event sources |
| `crates/agentos-api/tests/ws_integration.rs` | WebSocket integration tests |

## Dependencies

- **Requires:** Phase 3 (REST router + ApiState exist)
- **Blocks:** Phase 6 (web migration — WebSocket types shared with web layer)

## Test Plan

1. **Connect + auth:** valid JWT → 101 upgrade; invalid JWT → 401
2. **Subscribe/unsubscribe:** subscribe to "tasks" → receive subscribed frame → unsubscribe → no more events
3. **Event delivery:** subscribe to "agents" → connect agent via REST → receive `agent.connected` event
4. **Task channel:** subscribe to `tasks:{id}` → run task → receive `task.updated`, `task.completed` events
5. **Chat streaming:** send `chat.send` → receive multiple `chat.chunk` frames → `chat.done`
6. **Chat cancel:** send `chat.send` → send `chat.cancel` → receive `chat.cancelled`
7. **Task cancel:** send `task.cancel` → verify task cancelled via REST
8. **Notification respond:** send `notification.respond` → verify notification marked responded via REST
9. **Heartbeat:** verify server sends ping, client responds with pong, connection stays alive
10. **Disconnect:** close connection → verify all subscriptions cleaned up
11. **Backpressure:** slow consumer → verify oldest messages dropped, not OOM
12. **Permission enforcement:** subscribe to "tasks" without `tasks:r` → receive error frame

## Verification

```bash
cargo build -p agentos-api
cargo test -p agentos-api
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
