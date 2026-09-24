---
title: REST API Reference
tags:
  - api
  - reference
  - handbook
date: 2026-04-02
status: complete
effort: 2h
priority: high
---

# REST API Reference

> Complete reference for the AgentOS HTTP REST API (`agentos-api`). All endpoints are under the base path `/api/v1`.

---

## Overview

The REST API is served by the `agentos-api` crate alongside the kernel. It is disabled by default and must be enabled in `config/default.toml`:

```toml
[api]
enabled = true
host = "127.0.0.1"
port = 8080
```

Once enabled, the API server starts when the kernel boots and listens at the configured `host:port`.

---

## Authentication

All endpoints except `GET /api/v1/health` require a Bearer token in the `Authorization` header:

```
Authorization: Bearer agos_<64-hex-chars>
```

API keys have the format `agos_` followed by 64 lowercase hex characters. They are issued through the `agentos-api` `ApiKeyStore`. See [[25-API Authentication and Keys]] for key management.

On missing or invalid auth:

```json
{
  "error": {
    "code": "UNAUTHORIZED",
    "message": "Missing or invalid Authorization header. Expected: Bearer agos_<key>",
    "status": 401
  }
}
```

### Permission Scopes

Each key carries a list of permission scopes. An empty list grants full access (bootstrap key). Scope format: `<resource>:<op>` where op is `r` (read) or `w` (write).

| Resource | Read scope | Write scope | Covers |
|----------|-----------|-------------|--------|
| agents | `agents:r` | `agents:w` | List, get, connect, disconnect, permissions |
| tasks | `tasks:r` | `tasks:w` | List, get, run, cancel, trace |
| tools | `tools:r` | `tools:w` | List, get, install, remove |
| secrets | `secrets:r` | `secrets:w` | List, set, revoke |
| pipelines | `pipelines:r` | `pipelines:w` | List, save, run, delete |
| audit | `audit:r` | — | Logs, detail, verify |
| costs | `costs:r` | — | Summary, per-agent costs |
| notifications | `notifications:r` | `notifications:w` | List, get, unread count, respond |
| chat | — | `chat:w` | OpenAI-compatible chat completions |
| system | `system:r` | — | Status |

Wildcard scope `*:r` or `*:w` grants read or write across all resources.

---

## Rate Limiting

- Burst: **120 requests**
- Steady state: **2 requests / second** per IP address
- Excess requests receive `429 Too Many Requests`

---

## Response Format

All responses wrap their payload in a `data` field:

```json
{ "data": { ... } }
```

List responses include pagination metadata:

```json
{ "data": [...], "meta": { "total": 42 } }
```

---

## Error Format

```json
{
  "error": {
    "code": "NOT_FOUND",
    "message": "Agent 'worker' not found",
    "status": 404
  }
}
```

Common error codes: `UNAUTHORIZED` (401), `FORBIDDEN` (403), `NOT_FOUND` (404), `BAD_REQUEST` (400), `INTERNAL_ERROR` (500).

---

## Endpoints

### System

#### `GET /api/v1/health` — Health check

**Auth:** None required.

```bash
curl http://localhost:8080/api/v1/health
```

**Response:**
```json
{ "status": "ok", "service": "agentos-api" }
```

---

#### `GET /api/v1/status` — System status

**Auth:** `system:r`

```bash
curl -H "Authorization: Bearer agos_..." http://localhost:8080/api/v1/status
```

**Response:**
```json
{
  "data": {
    "agent_count": 3,
    "running_task_count": 2,
    "tool_count": 15,
    "uptime_secs": 3600,
    "version": "0.1.0"
  }
}
```

---

### Agents

#### `GET /api/v1/agents` — List agents

**Auth:** `agents:r`

```bash
curl -H "Authorization: Bearer agos_..." http://localhost:8080/api/v1/agents
```

**Response:**
```json
{
  "data": [
    { "id": "uuid", "name": "worker", "provider": "anthropic", "model": "claude-sonnet-4-6", "status": "idle" }
  ]
}
```

---

#### `POST /api/v1/agents` — Connect an agent

**Auth:** `agents:w`

```bash
curl -X POST http://localhost:8080/api/v1/agents \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "name": "worker", "provider": "anthropic", "model": "claude-sonnet-4-6" }'
```

**Request fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Unique agent name |
| `provider` | string | Yes | `ollama`, `openai`, `anthropic`, `gemini`, `custom`, `mock` |
| `model` | string | Yes | Model identifier |
| `base_url` | string | No | Override the provider base URL |
| `roles` | array | No | Role names to assign at connect time |

---

#### `GET /api/v1/agents/{name}` — Agent detail

**Auth:** `agents:r`

Returns full agent detail including permissions, cost stats, and task history summary.

---

#### `DELETE /api/v1/agents/{name}` — Disconnect agent

**Auth:** `agents:w`

```bash
curl -X DELETE http://localhost:8080/api/v1/agents/worker \
  -H "Authorization: Bearer agos_..."
```

**Response:**
```json
{ "data": { "disconnected": "worker" } }
```

---

#### `POST /api/v1/agents/{name}/permissions` — Grant permission

**Auth:** `agents:w`

```bash
curl -X POST http://localhost:8080/api/v1/agents/worker/permissions \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "permission": "fs:/tmp/:rw" }'
```

---

#### `POST /api/v1/agents/{name}/permissions/revoke` — Revoke permission

**Auth:** `agents:w`

```bash
curl -X POST http://localhost:8080/api/v1/agents/worker/permissions/revoke \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "permission": "fs:/tmp/:rw" }'
```

---

#### `POST /api/v1/agents/{name}/settings` — Update agent settings

**Auth:** `agents:w`

Updates an agent's settings (e.g. provider/model overrides, runtime options).

```bash
curl -X POST http://localhost:8080/api/v1/agents/worker/settings \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ ... }'
```

---

### Tasks

#### `GET /api/v1/tasks` — List tasks

**Auth:** `tasks:r`

**Query parameters:**

| Param | Type | Description |
|-------|------|-------------|
| `status` | string | Filter by status: `pending`, `running`, `completed`, `failed`, `cancelled` |
| `agent_name` | string | Filter by agent name |
| `limit` | integer | Max results (default 50) |
| `offset` | integer | Pagination offset |

```bash
curl "http://localhost:8080/api/v1/tasks?status=running&limit=10" \
  -H "Authorization: Bearer agos_..."
```

**Response:**
```json
{
  "data": [
    { "id": "uuid", "agent_name": "worker", "status": "running", "prompt": "...", "created_at": "..." }
  ],
  "meta": { "total": 42 }
}
```

---

#### `POST /api/v1/tasks/run` — Run a task

**Auth:** `tasks:w`

```bash
curl -X POST http://localhost:8080/api/v1/tasks/run \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "agent_name": "worker", "prompt": "Summarize /tmp/report.txt", "autonomous": false }'
```

**Request fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `agent_name` | string | Yes | Target agent name |
| `prompt` | string | Yes | Task instruction |
| `autonomous` | bool | No | Run without iteration limits (default `false`) |

**Response:**
```json
{ "data": { "task_id": "uuid" } }
```

---

#### `GET /api/v1/tasks/{id}` — Task detail

**Auth:** `tasks:r`

Returns full task detail including status, result, error, and timing.

---

#### `POST /api/v1/tasks/{id}/cancel` — Cancel task

**Auth:** `tasks:w`

```bash
curl -X POST http://localhost:8080/api/v1/tasks/uuid/cancel \
  -H "Authorization: Bearer agos_..."
```

---

#### `GET /api/v1/tasks/{id}/trace` — Task trace

**Auth:** `tasks:r`

Returns the full execution trace — every LLM turn, tool call, and result. Useful for debugging and auditing.

---

### Chat (OpenAI-Compatible)

#### `POST /api/v1/chat/completions` — Chat completion

**Auth:** `chat:w`

Drop-in replacement for the OpenAI `/v1/chat/completions` endpoint. Routes the request through a connected agent. Supports streaming via `"stream": true`.

```bash
curl -X POST http://localhost:8080/api/v1/chat/completions \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{
    "model": "worker",
    "messages": [{ "role": "user", "content": "Hello" }],
    "stream": false
  }'
```

**Request fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `model` | string | Yes | Agent name used to select the target agent. Also accepts `"provider/model"` — the provider portion is used as the agent name. |
| `messages` | array | Yes | Conversation history. Must strictly alternate `user` → `assistant` → `user` → .... The last message must be a `user` message. |
| `stream` | bool | No | Return SSE-streamed chunks in OpenAI format. Default `false`. |
| `temperature` | float | No | Temperature hint (passed through; agent may ignore). |
| `max_tokens` | integer | No | Max tokens hint (passed through; agent may ignore). |

**Constraints:**

- Messages must strictly alternate `user` and `assistant` roles. Two consecutive messages with the same role return `400 Bad Request`:
  ```json
  { "error": { "code": "BAD_REQUEST", "message": "consecutive 'user' messages are not valid; messages must alternate roles", "status": 400 } }
  ```
- The final message must have `role: "user"`. If no user message is present, `400 Bad Request` is returned.

> [!note]
> Some OpenAI-compatible clients silently tolerate consecutive same-role messages. AgentOS enforces strict alternation and will reject these with a 400. If your client merges or deduplicates messages, ensure it produces a properly alternating history before sending.

---

### Tools

#### `GET /api/v1/tools` — List tools

**Auth:** `tools:r`

Returns all registered tools including MCP-sourced tools (identified by a `source: "mcp:<server>"` field).

---

#### `GET /api/v1/tools/{name}` — Tool detail

**Auth:** `tools:r`

Returns the full tool manifest — name, description, input schema, trust tier, permissions.

---

#### `POST /api/v1/tools` — Install tool

**Auth:** `tools:w`

```bash
curl -X POST http://localhost:8080/api/v1/tools \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "manifest_path": "/path/to/tool.toml" }'
```

---

#### `DELETE /api/v1/tools/{name}` — Remove tool

**Auth:** `tools:w`

---

### Secrets

#### `GET /api/v1/secrets` — List secrets

**Auth:** `secrets:r`

Returns secret metadata only — names, scopes, creation dates. Raw values are never returned.

---

#### `POST /api/v1/secrets` — Set secret

**Auth:** `secrets:w`

```bash
curl -X POST http://localhost:8080/api/v1/secrets \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "name": "OPENAI_API_KEY", "value": "sk-...", "scope": "global" }'
```

---

#### `DELETE /api/v1/secrets/{name}` — Revoke secret

**Auth:** `secrets:w`

---

### Pipelines

#### `GET /api/v1/pipelines` — List pipelines

**Auth:** `pipelines:r`

---

#### `POST /api/v1/pipelines` — Save pipeline

**Auth:** `pipelines:w`

```bash
curl -X POST http://localhost:8080/api/v1/pipelines \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "name": "my-pipeline", "yaml": "steps:\n  - name: step1\n    agent: worker\n    prompt: ..." }'
```

---

#### `POST /api/v1/pipelines/{name}/run` — Run pipeline

**Auth:** `pipelines:w`

```bash
curl -X POST http://localhost:8080/api/v1/pipelines/my-pipeline/run \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "inputs": { "topic": "quarterly report" } }'
```

---

#### `DELETE /api/v1/pipelines/{name}` — Delete pipeline

**Auth:** `pipelines:w`

---

### Audit

#### `GET /api/v1/audit/logs` — Query audit log

**Auth:** `audit:r`

**Query parameters:**

| Param | Type | Description |
|-------|------|-------------|
| `event_type` | string | Filter by event type (e.g. `ToolExecuted`) |
| `agent_id` | string | Filter by agent UUID |
| `from` | ISO 8601 | Start timestamp |
| `to` | ISO 8601 | End timestamp |
| `limit` | integer | Max results (default 100) |

---

#### `GET /api/v1/audit/logs/{trace_id}` — Audit entry detail

**Auth:** `audit:r`

Returns the full audit entry for a specific trace ID, including all chained events.

---

#### `GET /api/v1/audit/verify` — Verify audit chain

**Auth:** `audit:r`

Triggers integrity verification of the Merkle hash chain for the last N entries (configured by `audit.verify_last_n_entries`). Returns `{ "data": { "valid": true, "entries_checked": 1000 } }`.

---

### Costs

#### `GET /api/v1/costs/summary` — Cost summary

**Auth:** `costs:r`

Returns per-agent cost breakdown for the current 24-hour budget period.

---

#### `GET /api/v1/costs/agents/{name}` — Per-agent costs

**Auth:** `costs:r`

Returns the cost entry for a specific agent: tokens used, cost in micro-USD, tool call count, and budget threshold status.

---

### Notifications

#### `GET /api/v1/notifications` — List notifications

**Auth:** `notifications:r`

**Query parameters:** `read` (bool), `limit`, `offset`.

---

#### `GET /api/v1/notifications/unread` — Unread count

**Auth:** `notifications:r`

```json
{ "data": { "count": 3 } }
```

---

#### `GET /api/v1/notifications/{id}` — Get notification

**Auth:** `notifications:r`

---

#### `DELETE /api/v1/notifications/{id}` — Dismiss notification

**Auth:** `notifications:w`

Dismisses (removes) a single notification by ID.

```bash
curl -X DELETE http://localhost:8080/api/v1/notifications/uuid \
  -H "Authorization: Bearer agos_..."
```

---

#### `DELETE /api/v1/notifications/read` — Clear read notifications

**Auth:** `notifications:w`

Clears all notifications that have been marked as read.

```bash
curl -X DELETE http://localhost:8080/api/v1/notifications/read \
  -H "Authorization: Bearer agos_..."
```

---

#### `POST /api/v1/notifications/{id}/respond` — Respond to notification

**Auth:** `notifications:w`

Used to answer `ask-user` questions from agents.

```bash
curl -X POST http://localhost:8080/api/v1/notifications/uuid/respond \
  -H "Authorization: Bearer agos_..." \
  -H "Content-Type: application/json" \
  -d '{ "text": "Yes, proceed." }'
```

---

### Webhooks

#### `POST /api/v1/webhooks/telegram/{channel_id}` — Telegram webhook ingress

**Auth:** None required (public).

Public inbound webhook endpoint for Telegram updates. Telegram POSTs updates here for the channel adapter instance identified by `{channel_id}`.

---

### Developer / OpenAPI

#### `GET /api/v1/openapi.json` — OpenAPI specification

**Auth:** None required (public).

Returns the OpenAPI 3.1 specification document describing the REST API.

#### `GET /api/v1/docs` — Interactive API docs

**Auth:** None required (public).

Serves the [Scalar](https://scalar.com/) interactive API documentation UI, rendered from the OpenAPI spec.

---

## Full Route Index

Generated from `crates/agentos-api/openapi.json` — **135 paths, 177 operations**. That file (served at `GET /api/v1/openapi.json`, browsable at `GET /api/v1/docs`) is the authoritative contract: request and response schemas live there, and CI fails when it drifts from the handlers. The detailed sections above cover the most-used endpoints; everything else is listed here.

### `agent-chats`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/agent-chats` | List multi-agent conversations (most-recent first). |
| `POST` | `/api/v1/agent-chats` | Create a conversation and start its orchestration |
| `GET` | `/api/v1/agent-chats/{id}` | Get a conversation with its turn timeline. |
| `POST` | `/api/v1/agent-chats/{id}/continue` | Resume a finished conversation in |
| `POST` | `/api/v1/agent-chats/{id}/messages` | Post an operator message. A running |
| `POST` | `/api/v1/agent-chats/{id}/stop` | Stop a running conversation after its |

### `agents`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/agents` | List all connected agents. |
| `POST` | `/api/v1/agents` | Connect a new agent. |
| `GET` | `/api/v1/agents/{id}/inbox` | Agent-to-agent message history for an agent |
| `GET` | `/api/v1/agents/{name}` | Get detailed info for a single agent. |
| `DELETE` | `/api/v1/agents/{name}` | Disconnect an agent by name, or remove it |
| `GET` | `/api/v1/agents/{name}/identity` | Agent cryptographic identity. |
| `POST` | `/api/v1/agents/{name}/permissions` | Grant a permission to an agent. |
| `POST` | `/api/v1/agents/{name}/permissions/revoke` | Revoke a permission. |
| `POST` | `/api/v1/agents/{name}/settings` | Update editable settings for an agent. |
| `GET` | `/api/v1/providers` | List built-in and catalog LLM providers. |

### `approval-policies`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/approval-policies` | List active standing grants. |
| `POST` | `/api/v1/approval-policies` | Add a standing grant. |
| `DELETE` | `/api/v1/approval-policies/{id}` | Revoke a standing grant. |

### `audit`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/audit/logs` | Query audit log entries. |
| `GET` | `/api/v1/audit/logs/{trace_id}` | Get a specific audit entry by trace ID. |
| `GET` | `/api/v1/audit/verify` | Verify audit log integrity. |

### `auth`

| Method | Path | Summary |
|---|---|---|
| `POST` | `/api/v1/auth/login` | Exchange the operator credential for a scoped, |
| `GET` | `/api/v1/auth/me` | Identity and scopes of the presented key. |
| `POST` | `/api/v1/auth/refresh` | Rotate the presented key: mint a fresh key with |
| `POST` | `/api/v1/ws/ticket` | Mint a short-lived, single-use WebSocket auth |

### `channels`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/channels` | List connected channels. |
| `POST` | `/api/v1/channels` | Connect a channel (mirrors `agentos channel connect`). |
| `GET` | `/api/v1/channels/pairings` | DM pairing allowlist (approved + pending). |
| `POST` | `/api/v1/channels/pairings/{code}/approve` | Approve a pairing code. |
| `GET` | `/api/v1/channels/{id}` | Channel detail. |
| `PUT` | `/api/v1/channels/{id}` | Edit a connected channel and rebuild its adapter. |
| `PUT` | `/api/v1/channels/{id}/agent` | Set/clear the default chat agent. |
| `POST` | `/api/v1/channels/{id}/disconnect` | Deregister a channel. |
| `DELETE` | `/api/v1/channels/{id}/pairings/{sender_id}` | Revoke an approved sender. |
| `POST` | `/api/v1/channels/{id}/pairings/{sender_id}/approve` | Approve a |
| `POST` | `/api/v1/channels/{id}/test` | Deliver a test notification. |

### `chat`

| Method | Path | Summary |
|---|---|---|
| `POST` | `/api/v1/chat/completions` | OpenAI-compatible chat completion. |

### `chat-sessions`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/chat/sessions` | List chat sessions (most-recent first). |
| `POST` | `/api/v1/chat/sessions` | Create a new chat session. |
| `GET` | `/api/v1/chat/sessions/{id}` | Get a session with its message timeline. |
| `PATCH` | `/api/v1/chat/sessions/{id}` | Rename a session (or clear the title). |
| `DELETE` | `/api/v1/chat/sessions/{id}` | Delete a session and its messages. |
| `GET` | `/api/v1/chat/sessions/{id}/export` | Export a session as JSON or markdown. |
| `POST` | `/api/v1/chat/sessions/{id}/fork` | Fork a session into a new copy. |
| `GET` | `/api/v1/chat/sessions/{id}/messages` | List a session's messages. |
| `POST` | `/api/v1/chat/sessions/{id}/messages` | send a user message and get the |
| `POST` | `/api/v1/chat/sessions/{id}/messages/stream` | send a user message and |

### `config`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/config` | Full config tree with secret-bearing leaves redacted. |
| `GET` | `/api/v1/config/{key}` | Resolve a dotted config key from the live file. |
| `PUT` | `/api/v1/config/{key}` | Write a dotted config key (gated by |

### `connectors`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/connectors` | List registered connectors and connection status. |
| `POST` | `/api/v1/connectors` | Register a connector from a manifest TOML. |
| `GET` | `/api/v1/connectors/{id}` | Connector detail. |
| `PUT` | `/api/v1/connectors/{id}` | Replace a connector manifest in place. |
| `DELETE` | `/api/v1/connectors/{id}` | Remove manifest + credential. |
| `POST` | `/api/v1/connectors/{id}/credential` | Store an OAuth token by hand. |
| `POST` | `/api/v1/connectors/{id}/disconnect` | Revoke OAuth credential + deregister. |
| `GET` | `/api/v1/connectors/{id}/oauth/callback` | Provider redirect target (public). |
| `POST` | `/api/v1/connectors/{id}/oauth/start` | Begin OAuth; returns the URL to open. |

### `costs`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/costs/agents/{name}` | Get cost summary for a specific agent. |
| `GET` | `/api/v1/costs/summary` | Get cost summary across all agents. |

### `escalations`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/escalations` | List escalations (pending by default, or all). |
| `GET` | `/api/v1/escalations/{id}` | Get a single escalation by numeric ID. |
| `POST` | `/api/v1/escalations/{id}/resolve` | Resolve an escalation with a decision. |

### `events`

| Method | Path | Summary |
|---|---|---|
| `POST` | `/api/v1/events/emit` | Emit an event into the kernel event bus. |
| `GET` | `/api/v1/events/stream` | SSE stream of realtime events. |
| `GET` | `/api/v1/events/subscriptions` | List all event subscriptions. |
| `POST` | `/api/v1/events/subscriptions` | Create an event subscription. |
| `DELETE` | `/api/v1/events/subscriptions/{id}` | Remove a subscription. |
| `POST` | `/api/v1/events/subscriptions/{id}/disable` | Pause a subscription. |
| `POST` | `/api/v1/events/subscriptions/{id}/enable` | Activate a subscription. |

### `files`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/files` | List files for the authenticated principal. |
| `POST` | `/api/v1/files` | Upload a file via `multipart/form-data`. |
| `GET` | `/api/v1/files/{id}` | Get a single file's metadata. |
| `DELETE` | `/api/v1/files/{id}` | Remove a file record and its bytes from disk. |
| `GET` | `/api/v1/files/{id}/download` | Stream the raw file bytes. |

### `keys`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/keys` | List all API keys with management metadata (never the |
| `POST` | `/api/v1/keys` | Mint a new scoped API key. The raw key is returned once. |
| `DELETE` | `/api/v1/keys/{id}` | Revoke a key by its public id. Idempotent-ish: |

### `marketplace`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/marketplace` | Search the registry (empty list on failure). |
| `GET` | `/api/v1/marketplace/{name}` | Fetch a single registry item. |
| `POST` | `/api/v1/marketplace/{name}/reviews` | Submit a review to the registry. |

### `mcp`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/mcp` | List MCP servers (live + persisted attachments). |
| `POST` | `/api/v1/mcp` | Attach an MCP server at runtime (stdio or http). |
| `GET` | `/api/v1/mcp/catalog` | Browse the curated MCP catalog. |
| `GET` | `/api/v1/mcp/catalog/{id}` | Full catalog entry. |
| `POST` | `/api/v1/mcp/catalog/{id}/install` | Install a catalog entry (attaches under its id). |
| `PUT` | `/api/v1/mcp/{name}` | Re-attach a server with a new configuration. |
| `POST` | `/api/v1/mcp/{name}/detach` | Stop and remove an MCP server. |

### `memory`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/agents/{id}/memory/{tier}` | Browse or search an agent's memory. |

### `notifications`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/notifications` | List notifications with optional filtering. |
| `DELETE` | `/api/v1/notifications` | Clear every notification (live questions survive). |
| `DELETE` | `/api/v1/notifications/read` | Clear all read notifications. |
| `POST` | `/api/v1/notifications/read-all` | Mark every notification as read. |
| `GET` | `/api/v1/notifications/unread` | Get count of unread notifications. |
| `GET` | `/api/v1/notifications/{id}` | Get a single notification. |
| `DELETE` | `/api/v1/notifications/{id}` | Dismiss a single notification. |
| `POST` | `/api/v1/notifications/{id}/read` | Mark a single notification read. |
| `POST` | `/api/v1/notifications/{id}/respond` | Respond to a notification. |

### `pipelines`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/pipelines` | List all saved pipelines. |
| `POST` | `/api/v1/pipelines` | Save (create or update) a pipeline. |
| `POST` | `/api/v1/pipelines/import` | Install a pipeline from raw YAML. |
| `GET` | `/api/v1/pipelines/runs/{run_id}/events` | Snapshot of a pipeline run. |
| `GET` | `/api/v1/pipelines/{name}` | Full pipeline definition as JSON. |
| `DELETE` | `/api/v1/pipelines/{name}` | Delete a pipeline. |
| `GET` | `/api/v1/pipelines/{name}/export` | Export a pipeline as raw YAML. |
| `POST` | `/api/v1/pipelines/{name}/run` | Execute a pipeline. |

### `plugins`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/plugins` | List discovered plugins. |
| `POST` | `/api/v1/plugins` | Install a plugin from a pasted `plugin.toml`. |
| `POST` | `/api/v1/plugins/discover` | Re-scan plugin directories. |
| `GET` | `/api/v1/plugins/{id}` | Plugin detail. |
| `PUT` | `/api/v1/plugins/{id}` | Replace a user plugin's manifest in place. |
| `DELETE` | `/api/v1/plugins/{id}` | Remove a user-installed plugin (core plugins refuse). |
| `POST` | `/api/v1/plugins/{id}/disable` | Deactivate a plugin. |
| `POST` | `/api/v1/plugins/{id}/enable` | Activate a plugin. |

### `prefs`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/prefs/proposals` | List user-preference proposals by status. |
| `POST` | `/api/v1/prefs/proposals/{id}/accept` | Accept a proposal. |
| `POST` | `/api/v1/prefs/proposals/{id}/reject` | Reject a proposal. |
| `GET` | `/api/v1/prefs/stats` | Aggregate proposal counts. |

### `roles`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/roles` | List all roles. |
| `POST` | `/api/v1/roles` | Create a new role with permissions. |
| `GET` | `/api/v1/roles/{name}` | Get a single role by name. |
| `DELETE` | `/api/v1/roles/{name}` | Delete a role by name. |

### `schedules`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/schedules` | List all scheduled entries: recurring cron |
| `POST` | `/api/v1/schedules` | Create a new cron schedule. |
| `POST` | `/api/v1/schedules/preview` | Compute upcoming fire times for a cron |
| `DELETE` | `/api/v1/schedules/{id}` | Delete a schedule. |
| `POST` | `/api/v1/schedules/{id}/pause` | Pause a schedule. |
| `POST` | `/api/v1/schedules/{id}/resume` | Resume a paused schedule. |
| `GET` | `/api/v1/schedules/{id}/runs` | List recorded fires of a schedule. |

### `scratchpad`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/agents/{name}/scratchpad` | List an agent's scratchpad pages. |
| `GET` | `/api/v1/agents/{name}/scratchpad/{page}` | Read an agent's page. |
| `PUT` | `/api/v1/agents/{name}/scratchpad/{page}` | Create or overwrite a page. |
| `DELETE` | `/api/v1/agents/{name}/scratchpad/{page}` | Delete a page. |
| `GET` | `/api/v1/scratchpad` | List pages in the global scratchpad. |
| `GET` | `/api/v1/scratchpad/{page}` | Read a page from the global scratchpad. |
| `PUT` | `/api/v1/scratchpad/{page}` | Create or overwrite a global page. |
| `DELETE` | `/api/v1/scratchpad/{page}` | Delete a global page. |

### `secrets`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/secrets` | List all secrets (metadata only, no values). |
| `POST` | `/api/v1/secrets` | Set or update a secret. |
| `DELETE` | `/api/v1/secrets/{name}` | Revoke (delete) a secret. |

### `skills`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/skills` | List installed skills. |
| `GET` | `/api/v1/skills/{name}` | Get one skill's full detail. |

### `system`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/dashboard` | Composite dashboard summary (agents, task counts, |
| `GET` | `/api/v1/doctor` | Run all diagnostic checks (read-only). |
| `POST` | `/api/v1/doctor/fix` | Attempt to auto-repair failing checks, then |
| `GET` | `/api/v1/hal` | Hardware abstraction layer device inventory + snapshot. |
| `GET` | `/api/v1/health` | Public health check (no auth required). |
| `GET` | `/api/v1/logs` | Query the audit log with optional level/since filters. |
| `GET` | `/api/v1/resources` | Host memory/disk snapshot plus live resource locks. |
| `GET` | `/api/v1/status` | System status with agent/task/tool counts. |

### `tasks`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/tasks` | List tasks with optional filtering. |
| `POST` | `/api/v1/tasks/run` | Submit a new task for execution. |
| `GET` | `/api/v1/tasks/{id}` | Get a single task by ID. |
| `POST` | `/api/v1/tasks/{id}/cancel` | Cancel a running task. |
| `GET` | `/api/v1/tasks/{id}/checkpoints` | List checkpoints for a task (0 or 1). |
| `POST` | `/api/v1/tasks/{id}/resume` | Resume a task from its latest checkpoint. |
| `GET` | `/api/v1/tasks/{id}/trace` | Get execution trace for a task. |

### `tools`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/tools` | List all registered tools. |
| `POST` | `/api/v1/tools` | Install a tool from a manifest path. |
| `GET` | `/api/v1/tools/{name}` | Get a specific tool by name. |
| `DELETE` | `/api/v1/tools/{name}` | Remove a tool by name. |

### `webhooks`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/webhooks` | List webhook endpoints. |
| `POST` | `/api/v1/webhooks` | Create a webhook endpoint (returns secret once). |
| `POST` | `/api/v1/webhooks/incoming/{endpoint_id}` | Provider webhook ingress. |
| `POST` | `/api/v1/webhooks/telegram/{channel_id}` | `POST /api/v1/webhooks/telegram/{channel_id}` |
| `DELETE` | `/api/v1/webhooks/{id}` | Delete a webhook endpoint. |
| `POST` | `/api/v1/webhooks/{id}/rotate` | Rotate the endpoint secret (returns once). |

### `workflows`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/workflows` | List all saved workflows. |
| `POST` | `/api/v1/workflows` | Create a new workflow (server assigns the id). |
| `GET` | `/api/v1/workflows/{id}` | Fetch a single workflow's full definition. |
| `PUT` | `/api/v1/workflows/{id}` | Update an existing workflow in place. |
| `DELETE` | `/api/v1/workflows/{id}` | Delete a workflow. |

### `workspace-grants`

| Method | Path | Summary |
|---|---|---|
| `GET` | `/api/v1/workspace-grants` | List active folder-access grants. |
| `POST` | `/api/v1/workspace-grants` | Grant a host directory to one agent, or to |
| `DELETE` | `/api/v1/workspace-grants` | Revoke a grant. |

---

## Security Headers

All API responses include:

| Header | Value |
|--------|-------|
| `X-Content-Type-Options` | `nosniff` |
| `X-Frame-Options` | `DENY` |
| `Cache-Control` | `no-store` |

---

## Middleware Stack

Requests pass through this middleware chain (outermost to innermost):

1. **Rate limiting** — 120 burst / 2 per second per IP
2. **CORS** — allow origin from the configured `host:port`
3. **Tracing** — structured HTTP trace spans
4. **Compression** — response body compression
5. **Security headers** — see above
6. **Bearer auth** — on protected routes only

---

## Related

- [[25-API Authentication and Keys]] — API key lifecycle and best practices
- [[24-WebSocket Guide]] — Real-time event subscriptions and chat
- [[08-Security Model]] — Capability tokens, permission scopes, and API auth layer
- [[16-Configuration Reference]] — `[api]` config section
