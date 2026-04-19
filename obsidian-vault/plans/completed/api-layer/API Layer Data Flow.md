---
title: API Layer Data Flow
tags:
  - api
  - kernel
  - web
  - v3
  - flow
date: 2026-03-30
status: complete
effort: 0.5d
priority: high
---

# API Layer Data Flow

> Request lifecycle through the new API architecture — from external client to kernel response.

---

## REST Request Flow

```
External Client
    │
    ├── GET /api/v1/tasks?status=running&limit=10
    │   Authorization: Bearer eyJ...
    │
    ▼
┌─────────────────────────────────┐
│  Axum Middleware Stack           │
│  1. Rate limiter (governor)     │
│  2. CORS                        │
│  3. Trace layer                 │
│  4. Compression (gzip)          │
│  5. Security headers            │
│  6. JWT auth middleware ────────┼──→ Extract + validate JWT
│     └─ Attach AuthClaims        │     Verify RS256 signature
│                                 │     Check expiry
│                                 │     Check revocation set
└───────────┬─────────────────────┘
            │
            ▼
┌─────────────────────────────────┐
│  REST Handler (tasks.rs)        │
│  1. claims.require("tasks:r")   │──→ 403 if missing
│  2. Parse query params → TaskFilter
│  3. svc.list_tasks(filter).await│
└───────────┬─────────────────────┘
            │
            ▼
┌─────────────────────────────────┐
│  KernelService trait            │
│  fn list_tasks(&self, filter)   │
└───────────┬─────────────────────┘
            │
            ▼
┌─────────────────────────────────┐
│  impl KernelService for Kernel  │
│  (kernel_impl.rs)               │
│  1. self.scheduler.list_tasks() │
│  2. Apply filter (status, agent)│
│  3. Apply pagination (limit,    │
│     offset)                     │
│  4. Convert → Vec<TaskSummary>  │
│  5. Count total for meta        │
└───────────┬─────────────────────┘
            │
            ▼
┌─────────────────────────────────┐
│  Response                       │
│  {                              │
│    "data": [TaskSummary, ...],  │
│    "meta": {                    │
│      "total": 42,               │
│      "limit": 10,               │
│      "offset": 0                │
│    }                            │
│  }                              │
│  HTTP 200                       │
└─────────────────────────────────┘
```

## WebSocket Connection Flow

```
Client
    │
    ├── GET /api/v1/ws?token=eyJ...
    │   Upgrade: websocket
    │
    ▼
┌─────────────────────────────────┐
│  WS Upgrade Handler             │
│  1. Extract JWT from query      │
│  2. Validate token              │
│  3. Create WsSession            │
│  4. Accept upgrade              │
└───────────┬─────────────────────┘
            │
            ▼
┌─────────────────────────────────┐
│  WsSession (per-connection)     │
│  ┌──────────┐  ┌─────────────┐  │
│  │ Read loop │  │ Write loop  │  │
│  │ (client→) │  │ (→client)   │  │
│  └─────┬─────┘  └──────▲──────┘  │
│        │               │         │
│        ▼               │         │
│  ┌─────────────────────┘         │
│  │ Subscription Manager          │
│  │ - channels: HashMap           │
│  │ - filters: per-channel        │
│  └───────────┬───────────────────┘
└──────────────┼───────────────────┘
               │
               ▼
┌─────────────────────────────────┐
│  WsBroadcaster                  │
│                                 │
│  Kernel EventBus ──────────────→│──→ Match against subscriptions
│  status_update_sender ─────────→│──→ Filter by channel + criteria
│  notification_tx ──────────────→│──→ Serialize to JSON frame
│                                 │──→ Send to subscribed sessions
└─────────────────────────────────┘
```

## WebSocket Chat Streaming Flow

```
Client                          Server
  │                               │
  ├─ { "type": "chat.send",      │
  │    "session_id": "s1",       │
  │    "message": "Hello" }      │
  │                               │
  │                               ├── WsSession read loop receives frame
  │                               ├── Validate session ownership
  │                               ├── claims.require("chat:w")
  │                               ├── svc.chat_stream(req).await
  │                               │
  │                               │   ┌─ KernelService impl ─────────┐
  │                               │   │ 1. Look up agent for session  │
  │                               │   │ 2. Build context window       │
  │                               │   │ 3. Call LLM with tool defs    │
  │                               │   │ 4. Stream tokens via channel  │
  │                               │   │ 5. If tool_call: execute tool │
  │                               │   │ 6. Re-infer with tool result  │
  │                               │   │ 7. Repeat until done          │
  │                               │   └───────────────────────────────┘
  │                               │
  │ ←─ { "type": "chat.chunk",   │
  │      "session_id": "s1",     │
  │      "delta": "Hello" }      │
  │ ←─ { "type": "chat.chunk",   │
  │      "delta": "! How can" }  │
  │ ←─ { "type": "chat.chunk",   │
  │      "delta": " I help?" }   │
  │ ←─ { "type": "chat.done",    │
  │      "session_id": "s1",     │
  │      "tool_calls": [] }      │
  │                               │
  ├─ { "type": "chat.cancel",    │  (optional: mid-stream cancel)
  │    "session_id": "s1" }      │
  │                               ├── Drop the streaming channel
  │ ←─ { "type": "chat.cancelled",│
  │      "session_id": "s1" }    │
```

## Auth Token Exchange Flow

```
Client                          Server
  │                               │
  ├─ POST /api/v1/auth/token      │
  │  { "api_key": "agos_abc.." } │
  │                               │
  │                               ├── Hash key with SHA-256
  │                               ├── Look up in api_keys table
  │                               ├── Verify not expired/revoked
  │                               ├── Load PermissionSet from key
  │                               ├── Generate JWT (RS256, 1h expiry)
  │                               ├── Generate refresh token (24h)
  │                               ├── Log ApiKeyUsed audit event
  │                               │
  │ ←─ {                          │
  │      "access_token": "eyJ..", │
  │      "refresh_token": "ref_.",│
  │      "expires_in": 3600,     │
  │      "token_type": "Bearer"  │
  │    }                          │
  │                               │
  │  ... later, before expiry ... │
  │                               │
  ├─ POST /api/v1/auth/refresh    │
  │  { "refresh_token": "ref_."} │
  │                               │
  │                               ├── Validate refresh token
  │                               ├── Generate new access JWT
  │                               │
  │ ←─ { "access_token": "eyJ.."}│
```

## HTML Web UI Flow (After Migration)

```
Browser
    │
    ├── GET /tasks
    │   Cookie: session=abc...
    │
    ▼
┌─────────────────────────────────┐
│  Web Middleware Stack            │
│  (unchanged: rate limit, CORS,  │
│   auth cookie, CSRF)            │
└───────────┬─────────────────────┘
            │
            ▼
┌─────────────────────────────────┐
│  HTML Handler (tasks.rs)        │
│  1. svc.list_tasks(filter).await│  ← Same KernelService call
│  2. Render template with data   │    as REST handler
│  3. Return text/html            │
└─────────────────────────────────┘
```

The key insight: HTML handlers and REST handlers make the exact same `KernelService` calls. The only difference is the response format (HTML template vs JSON envelope).

## Related

- [[API Layer Plan]] — master plan
- [[API Layer Research]] — coupling analysis
