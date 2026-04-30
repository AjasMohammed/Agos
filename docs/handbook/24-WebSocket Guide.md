---
title: WebSocket Guide
tags:
  - api
  - websocket
  - reference
  - handbook
date: 2026-04-02
status: complete
effort: 1.5h
priority: high
---

# WebSocket Guide

> Real-time bidirectional communication with AgentOS — subscribe to kernel events, stream chat responses, and send task actions over a single persistent connection.

---

## Overview

The WebSocket endpoint upgrades a standard HTTP connection into a persistent JSON message stream. It is the preferred interface for:

- **Real-time event feeds** — watch task completions, agent state changes, audit events
- **Streaming chat** — receive LLM response tokens as they generate
- **Interactive notifications** — respond to `ask-user` questions from agents without polling
- **Task control** — cancel running tasks mid-execution

The WebSocket endpoint is public (on the same port as the REST API) but requires authentication via query parameter.

---

## Connecting

```
GET /api/v1/ws?token=agos_<64-hex-chars>
```

The API key is passed as a query parameter because the WebSocket HTTP upgrade does not support custom headers in all clients. The key must be a valid, non-revoked, non-expired `agos_*` key.

> **Security note — log redaction required before public exposure.**
> URL query parameters (including `?token=…`) are recorded verbatim by HTTP server access logs, reverse proxies, CDNs, and browser history. Before exposing this endpoint on a public or shared network, configure your reverse proxy (nginx, Caddy, etc.) to redact or strip the `token` query parameter from access logs. Failure to do so means long-lived API keys will appear in plaintext in multiple external systems.
>
> If your deployment cannot guarantee log redaction, consider issuing a short-lived, single-use ticket via `POST /api/v1/auth/ws-ticket` (planned) so the long-lived key never appears in the URL.

**Example (JavaScript):**

```javascript
const ws = new WebSocket('ws://localhost:8080/api/v1/ws?token=agos_abc123...');

ws.onopen = () => console.log('connected');
ws.onmessage = (e) => console.log('frame:', JSON.parse(e.data));
ws.onclose = () => console.log('disconnected');
```

**Example (Python):**

```python
import asyncio
import json
import websockets

async def main():
    uri = "ws://localhost:8080/api/v1/ws?token=agos_abc123..."
    async with websockets.connect(uri) as ws:
        # Subscribe to task events
        await ws.send(json.dumps({
            "type": "subscribe",
            "channel": "tasks"
        }))
        async for message in ws:
            print(json.loads(message))

asyncio.run(main())
```

On successful connection the server immediately begins processing frames. On auth failure, the upgrade returns `401 Unauthorized` before the WebSocket handshake completes.

---

## Heartbeat

The server sends a `pong` frame every 30 seconds to keep the connection alive through proxies and NAT. Clients may optionally send `ping` frames:

```json
{ "type": "ping" }
```

Server response:

```json
{ "type": "pong" }
```

---

## Frame Format

All frames are UTF-8 JSON objects with a `type` discriminator field. Binary frames are ignored.

### Client → Server Frames

#### `subscribe` — Subscribe to a channel

```json
{
  "type": "subscribe",
  "channel": "tasks",
  "filter": {}
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | Yes | Channel name (see [[#Available Channels]]) |
| `filter` | object | No | Channel-specific filter (reserved for future use) |

Server confirms with a `subscribed` frame:

```json
{
  "type": "subscribed",
  "channel": "tasks",
  "subscription_id": "sub_abc123"
}
```

---

#### `unsubscribe` — Cancel a subscription

```json
{
  "type": "unsubscribe",
  "subscription_id": "sub_abc123"
}
```

Server confirms with `unsubscribed`:

```json
{
  "type": "unsubscribed",
  "subscription_id": "sub_abc123"
}
```

---

#### `chat.send` — Send a chat message

Sends a message to an agent and begins a streaming response.

```json
{
  "type": "chat.send",
  "session_id": "sess_xyz",
  "agent_name": "worker",
  "message": "Summarize the quarterly report"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `session_id` | string | Yes | Client-generated session identifier (for correlating response chunks) |
| `agent_name` | string | Yes | Target agent name |
| `message` | string | Yes | User message text |

---

#### `chat.cancel` — Cancel a streaming response

```json
{
  "type": "chat.cancel",
  "session_id": "sess_xyz"
}
```

Server confirms with `chat.cancelled`:

```json
{
  "type": "chat.cancelled",
  "session_id": "sess_xyz"
}
```

---

#### `task.cancel` — Cancel a running task

```json
{
  "type": "task.cancel",
  "task_id": "uuid"
}
```

---

#### `notification.respond` — Respond to an agent question

```json
{
  "type": "notification.respond",
  "id": "notif_uuid",
  "text": "Yes, proceed with the deployment."
}
```

---

### Server → Client Frames

#### `event` — Real-time channel event

Delivered for all active subscriptions when a matching event occurs.

```json
{
  "type": "event",
  "channel": "tasks",
  "event": "TaskCompleted",
  "data": {
    "task_id": "uuid",
    "agent_name": "worker",
    "status": "completed",
    "result": "..."
  }
}
```

---

#### `chat.chunk` — Streaming response token

```json
{
  "type": "chat.chunk",
  "session_id": "sess_xyz",
  "delta": "The quarterly report shows..."
}
```

Chunks arrive in order. Concatenate `delta` values to build the full response.

---

#### `chat.done` — Response complete

```json
{
  "type": "chat.done",
  "session_id": "sess_xyz",
  "tool_calls": []
}
```

The `tool_calls` array contains any tool calls the agent made during inference (may be empty).

---

#### `error` — Protocol or processing error

```json
{
  "type": "error",
  "code": "INVALID_FRAME",
  "message": "Failed to parse JSON: missing field 'type'"
}
```

Common error codes:

| Code | Description |
|------|-------------|
| `INVALID_FRAME` | Malformed JSON or unknown `type` |
| `CHANNEL_NOT_FOUND` | Subscribed channel name not recognized |
| `AGENT_NOT_FOUND` | Target agent for `chat.send` not connected |
| `TASK_NOT_FOUND` | Task ID for `task.cancel` not found |

---

## Available Channels

| Channel | Events pushed | Description |
|---------|--------------|-------------|
| `tasks` | `TaskStarted`, `TaskCompleted`, `TaskFailed`, `TaskCancelled` | Task lifecycle changes for all agents |
| `agents` | `AgentConnected`, `AgentDisconnected`, `AgentStatusChanged` | Agent registry changes |
| `audit` | All 83+ event types | Full audit log stream (high volume — use filters) |
| `costs` | `BudgetAlert`, `HardLimitExceeded`, `CostAttribution` | Budget threshold events |
| `notifications` | `NotificationCreated`, `EscalationCreated` | Operator inbox events |

---

## Complete Example: Task Monitoring

```javascript
const ws = new WebSocket('ws://localhost:8080/api/v1/ws?token=agos_...');

ws.onopen = () => {
  // Subscribe to task events
  ws.send(JSON.stringify({ type: 'subscribe', channel: 'tasks' }));
};

ws.onmessage = (e) => {
  const frame = JSON.parse(e.data);

  switch (frame.type) {
    case 'subscribed':
      console.log(`Subscribed to ${frame.channel} (${frame.subscription_id})`);
      // Now start a task via REST API
      fetch('/api/v1/tasks/run', {
        method: 'POST',
        headers: { 'Authorization': 'Bearer agos_...', 'Content-Type': 'application/json' },
        body: JSON.stringify({ agent_name: 'worker', prompt: 'Analyze sales data' })
      });
      break;

    case 'event':
      if (frame.channel === 'tasks') {
        console.log(`Task event: ${frame.event}`, frame.data);
      }
      break;
  }
};
```

---

## Complete Example: Streaming Chat

```javascript
const ws = new WebSocket('ws://localhost:8080/api/v1/ws?token=agos_...');
let response = '';

ws.onopen = () => {
  ws.send(JSON.stringify({
    type: 'chat.send',
    session_id: 'session-1',
    agent_name: 'worker',
    message: 'Write a haiku about Rust'
  }));
};

ws.onmessage = (e) => {
  const frame = JSON.parse(e.data);

  if (frame.type === 'chat.chunk' && frame.session_id === 'session-1') {
    process.stdout.write(frame.delta);
    response += frame.delta;
  } else if (frame.type === 'chat.done' && frame.session_id === 'session-1') {
    console.log('\n--- done ---');
    ws.close();
  } else if (frame.type === 'error') {
    console.error(frame.code, frame.message);
  }
};
```

---

## Reconnection

The WebSocket connection can close due to network interruptions, server restarts, or idle timeouts. Implement exponential backoff reconnection:

```javascript
let retryDelay = 1000;

function connect() {
  const ws = new WebSocket('ws://localhost:8080/api/v1/ws?token=agos_...');

  ws.onopen = () => {
    retryDelay = 1000; // reset on successful connection
    // Re-subscribe to channels after reconnect
    ws.send(JSON.stringify({ type: 'subscribe', channel: 'tasks' }));
  };

  ws.onclose = () => {
    setTimeout(connect, retryDelay);
    retryDelay = Math.min(retryDelay * 2, 30000); // cap at 30s
  };
}

connect();
```

Subscriptions are not automatically restored after reconnection — re-send `subscribe` frames after the `onopen` event.

---

## Related

- [[23-REST API Reference]] — REST endpoints for mutations and queries
- [[25-API Authentication and Keys]] — API key management
- [[12-Event System]] — Kernel event types and the internal event bus
- [[21-User Notifications and Channels]] — `ask-user`, `notify-user`, and notification inbox
