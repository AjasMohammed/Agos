---
title: "Phase 2: SSE Delivery + Web Notification Center"
tags:
  - web
  - kernel
  - htmx
  - sse
  - plan
  - phase-2
date: 2026-03-24
status: planned
effort: 2d
priority: high
---

# Phase 2: SSE Delivery + Web Notification Center

> Wire the `NotificationRouter` to the existing SSE infrastructure in `agentos-web`. Add a web-based notification inbox at `/notifications` with real-time push, a notification bell indicator, and inline response forms for Questions. No new dependencies — reuse Pico CSS, HTMX, Alpine.js, SSE already present.

**Depends on**: [[01-user-message-type-and-router]] (Phase 1)
**Blocks**: [[03-ask-user-tool]] (Phase 3 needs at least one interactive channel)

---

## Why This Phase

The CLI polling model from Phase 1 works in headless environments but is terrible UX. The AgentOS web UI already has SSE streaming implemented for the chat interface (`handlers/chat.rs`). Phase 2 extends that infrastructure to push notifications to the browser in real time, and adds a dedicated `/notifications` page where users can:
- See the full notification inbox
- Read notifications inline
- Respond to Questions without leaving the browser

The web channel transforms notifications from "go check the CLI" to "it comes to you."

---

## Current State vs. Target

| Item | Current | Target |
|------|---------|--------|
| SSE in chat handler | ✅ Implemented | Reuse broadcast pattern for notifications |
| Notification SSE endpoint | Does not exist | `GET /notifications/stream` → SSE push |
| Notification inbox page | Does not exist | `GET /notifications` — full inbox with history |
| Notification bell indicator | Does not exist | Icon in nav bar showing unread count, live-updated |
| Inline response form | Does not exist | HTMX POST form inline for Question-type notifications |
| `SseDeliveryAdapter` | Does not exist | Adapter in kernel that pushes to web SSE channel |
| Global broadcast channel (web) | Per-chat-session only | New kernel-level broadcast for notifications |

---

## Architecture: How SSE Push Works

The existing chat SSE uses a per-session `mpsc` channel that the Axum handler reads from. For notifications, we need a **broadcast channel** because multiple browser tabs / sessions should all receive the same notification.

```
NotificationRouter::deliver(msg)
    │
    ▼
SseDeliveryAdapter::deliver(&msg)
    │  publish to tokio::sync::broadcast::Sender<NotificationSseEvent>
    ▼
GET /notifications/stream (per browser tab)
    │  subscribes to broadcast::Receiver
    ├── SSE event: "notification-new" { id, subject, priority, kind }
    └── Browser: Alpine.js updates notification bell count; HTMX swaps inbox partial
```

The broadcast channel is stored in `AppState` (same place chat session state lives).

---

## Detailed Subtasks

### 2.1 — Add notification broadcast channel to `AppState`

**File**: `crates/agentos-web/src/state.rs` (or wherever `AppState` is defined)

```rust
use tokio::sync::broadcast;

pub struct AppState {
    // ... existing fields ...
    pub notification_tx: broadcast::Sender<NotificationSseEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationSseEvent {
    pub id: String,             // NotificationID as string
    pub subject: String,
    pub priority: String,       // "info" | "warning" | "urgent" | "critical"
    pub kind: String,           // "notification" | "question" | "task_complete" | "status_update"
    pub body_preview: String,   // first 100 chars of body
    pub requires_response: bool,
    pub created_at: String,     // ISO 8601
}

impl AppState {
    pub fn new(...) -> Self {
        let (notification_tx, _) = broadcast::channel(256);
        Self { ..., notification_tx }
    }
}
```

---

### 2.2 — Create `SseDeliveryAdapter`

**File**: `crates/agentos-kernel/src/notification_router.rs` (add to existing file)

```rust
pub struct SseDeliveryAdapter {
    tx: tokio::sync::broadcast::Sender<NotificationSsePayload>,
}

#[derive(Clone)]
pub struct NotificationSsePayload {
    pub id: NotificationID,
    pub subject: String,
    pub priority: NotificationPriority,
    pub kind_tag: String,      // "notification" | "question" | "task_complete" | "status_update"
    pub body_preview: String,
    pub requires_response: bool,
}

#[async_trait]
impl DeliveryAdapter for SseDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel { DeliveryChannel::Web }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        let payload = NotificationSsePayload {
            id: msg.id.clone(),
            subject: msg.subject.clone(),
            priority: msg.priority.clone(),
            kind_tag: kind_to_tag(&msg.kind),
            body_preview: msg.body.chars().take(100).collect(),
            requires_response: msg.interaction.is_some(),
        };
        // ignore SendError (no active SSE subscribers is fine)
        let _ = self.tx.send(payload);
        Ok(())
    }

    async fn is_available(&self) -> bool {
        self.tx.receiver_count() > 0
    }
}
```

The `SseDeliveryAdapter` needs its `tx` wired from `AppState.notification_tx`. This is done during kernel startup: the web server creates `AppState`, passes `notification_tx.clone()` to the adapter during `NotificationRouter::new()`.

---

### 2.3 — Add SSE stream endpoint

**File**: `crates/agentos-web/src/handlers/notifications.rs` (new file)

```rust
// GET /notifications/stream
pub async fn notification_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.notification_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(payload) => {
                    let data = serde_json::to_string(&payload).unwrap_or_default();
                    yield Ok(Event::default().event("notification-new").data(data));
                }
                Err(RecvError::Lagged(n)) => {
                    // subscriber was too slow; send a "reload" event
                    yield Ok(Event::default().event("notification-reload").data(n.to_string()));
                }
                Err(RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

---

### 2.4 — Add notification inbox page

**File**: `crates/agentos-web/src/handlers/notifications.rs`

```rust
// GET /notifications
pub async fn inbox(State(state): State<AppState>) -> Response {
    let msgs = state.bus_client.list_notifications(unread_only: false, limit: 50).await;
    // render template: notifications/inbox.html
}

// GET /notifications/{id}
pub async fn get_notification(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    // fetch + mark as read
    // render template: notifications/detail.html
}

// POST /notifications/{id}/respond
pub async fn respond_to_notification(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<RespondForm>,
) -> Response {
    // send KernelCommand::RespondToNotification
    // return HTMX partial: "Responded!" confirmation
}
```

---

### 2.5 — Create MiniJinja2 templates

**Files in `crates/agentos-web/templates/notifications/`** (new directory):

#### `inbox.html`
```html
{% extends "base.html" %}
{% block content %}
<section>
  <hgroup>
    <h2>Notifications</h2>
    <p>Messages from your agents</p>
  </hgroup>

  <!-- SSE connection for real-time updates -->
  <div hx-ext="sse" sse-connect="/notifications/stream"
       sse-swap="notification-new"
       hx-target="#notification-list"
       hx-swap="afterbegin">
  </div>

  <div id="notification-list">
    {% for msg in notifications %}
      {% include "notifications/_notification_row.html" %}
    {% else %}
      <article>
        <p>No notifications yet. Agents will send messages here.</p>
      </article>
    {% endfor %}
  </div>
</section>
{% endblock %}
```

#### `_notification_row.html` (HTMX partial)
```html
<article id="notif-{{ msg.id }}" {% if not msg.read %}aria-current="true"{% endif %}>
  <header>
    <span data-priority="{{ msg.priority | lower }}">{{ msg.priority }}</span>
    <strong>{{ msg.subject }}</strong>
    <small>{{ msg.created_at | relative_time }}</small>
    <small>from {{ msg.from }}</small>
  </header>
  <p>{{ msg.body | truncate(200) | markdown }}</p>

  {% if msg.interaction and not msg.response %}
    <!-- Inline response form for Questions -->
    {% include "notifications/_respond_form.html" %}
  {% elif msg.response %}
    <footer><em>You responded: "{{ msg.response.text }}"</em></footer>
  {% endif %}

  <footer>
    <a href="/notifications/{{ msg.id }}"
       hx-get="/notifications/{{ msg.id }}"
       hx-target="#notif-{{ msg.id }}"
       hx-swap="outerHTML">Read more</a>
  </footer>
</article>
```

#### `_respond_form.html`
```html
<form hx-post="/notifications/{{ msg.id }}/respond"
      hx-target="#notif-{{ msg.id }}"
      hx-swap="outerHTML">
  {% if msg.options %}
    <fieldset>
      {% for option in msg.options %}
        <label>
          <input type="radio" name="response" value="{{ option }}">
          {{ option }}
        </label>
      {% endfor %}
    </fieldset>
  {% else %}
    <textarea name="response" placeholder="Type your response..."></textarea>
  {% endif %}
  <button type="submit">Send Response</button>
  <small>Auto-action in {{ msg.expires_in }}</small>
</form>
```

---

### 2.6 — Notification bell in navigation bar

**File**: `crates/agentos-web/templates/partials/nav.html` (or equivalent base template)

```html
<!-- Alpine.js state for unread count -->
<div x-data="notificationBell()" x-init="init()">
  <a href="/notifications" role="button" class="outline">
    🔔 <span x-show="unreadCount > 0" x-text="unreadCount"
             style="background: var(--pico-color-red-500); border-radius: 50%; padding: 2px 6px; font-size: 0.75rem;"></span>
  </a>
</div>

<script>
function notificationBell() {
  return {
    unreadCount: 0,
    evtSource: null,
    init() {
      // Initial count from page data
      this.unreadCount = parseInt(document.body.dataset.unreadNotifications || '0');
      // SSE for real-time increments
      this.evtSource = new EventSource('/notifications/stream');
      this.evtSource.addEventListener('notification-new', (e) => {
        this.unreadCount++;
        // Also show a toast
        const data = JSON.parse(e.data);
        window.dispatchEvent(new CustomEvent('showToast', {
          detail: {
            message: data.subject,
            type: data.priority === 'critical' ? 'error' :
                  data.priority === 'urgent' ? 'warning' : 'info'
          }
        }));
      });
    }
  }
}
</script>
```

---

### 2.7 — Register routes

**File**: `crates/agentos-web/src/router.rs` (or wherever routes are defined)

```rust
use handlers::notifications;

let notification_routes = Router::new()
    .route("/notifications", get(notifications::inbox))
    .route("/notifications/stream", get(notifications::notification_stream))
    .route("/notifications/:id", get(notifications::get_notification))
    .route("/notifications/:id/respond", post(notifications::respond_to_notification));

// merge into app router
app.merge(notification_routes)
```

---

### 2.8 — Wire `SseDeliveryAdapter` into kernel startup

**File**: `crates/agentos-kernel/src/kernel.rs` (or startup code)

During web server startup, the `notification_tx` channel is created in `AppState`. A clone of the `Sender` must be passed to the kernel's `NotificationRouter` as an `SseDeliveryAdapter`.

The challenge: web and kernel are separate processes communicating via Unix domain socket. The SSE adapter cannot hold a direct reference to the web server's channel.

**Solution**: The `SseDeliveryAdapter` is implemented in `agentos-web`, not the kernel. It subscribes to kernel `StatusUpdate` and notification events by acting as a **bus subscriber**. The web server opens a persistent bus connection and receives `BusMessage::StatusUpdate` and (new) `BusMessage::NotificationPush` messages, then publishes to its local broadcast channel.

```rust
// In agentos-web startup:
// spawn task that maintains bus subscription and feeds notification_tx
tokio::spawn(async move {
    let mut stream = bus_client.subscribe_notifications().await;
    while let Some(msg) = stream.next().await {
        let _ = notification_tx.send(msg.into());
    }
});
```

Add `BusMessage::NotificationPush(UserMessage)` to the bus message enum for this purpose.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/state.rs` | Add `notification_tx: broadcast::Sender<...>` |
| `crates/agentos-web/src/handlers/notifications.rs` | NEW — inbox, stream, get, respond handlers |
| `crates/agentos-web/templates/notifications/inbox.html` | NEW |
| `crates/agentos-web/templates/notifications/_notification_row.html` | NEW |
| `crates/agentos-web/templates/notifications/_respond_form.html` | NEW |
| `crates/agentos-web/templates/partials/nav.html` | Add notification bell + Alpine.js |
| `crates/agentos-web/src/router.rs` | Register notification routes |
| `crates/agentos-kernel/src/notification_router.rs` | Add `SseDeliveryAdapter` |
| `crates/agentos-bus/src/message.rs` | Add `BusMessage::NotificationPush` |

---

## Test Plan

```rust
#[tokio::test]
async fn test_sse_adapter_publishes_on_deliver() {
    let (tx, mut rx) = broadcast::channel(10);
    let adapter = SseDeliveryAdapter { tx };
    let msg = make_test_notification("test subject");
    adapter.deliver(&msg).await.unwrap();
    let payload = rx.try_recv().unwrap();
    assert_eq!(payload.subject, "test subject");
}

#[tokio::test]
async fn test_sse_adapter_is_available_when_subscriber_exists() {
    let (tx, _rx) = broadcast::channel(10);
    let adapter = SseDeliveryAdapter { tx };
    assert!(adapter.is_available().await);
}

#[tokio::test]
async fn test_inbox_page_renders() {
    // integration test using test web client
    let client = make_test_web_client().await;
    let resp = client.get("/notifications").await;
    assert_eq!(resp.status(), 200);
    assert!(resp.text().contains("Notifications"));
}

#[tokio::test]
async fn test_respond_form_submits() {
    let client = make_test_web_client().await;
    // first post a question notification
    // then submit response via POST /notifications/{id}/respond
    // assert response is stored in inbox
}
```

---

## Verification

```bash
# Build web crate
cargo build -p agentos-web

# Run kernel + web
agentctl kernel start &
agentctl web start &

# Send a notification from CLI
# (Phase 3 adds the tool; for now test via direct bus command)

# Open browser → http://localhost:PORT/notifications
# → expect inbox page with empty state

# Bell indicator in nav bar shows 0

# Trigger a test notification → bell increments without page refresh
# (use a test command or direct SQLite insert for Phase 2 testing)
```
