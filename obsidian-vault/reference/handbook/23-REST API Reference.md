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
- [[24-WebSocket Guide]] — Real-time event subscriptions and chat streaming
- [[08-Security Model]] — Capability tokens, permission scopes, and API auth layer
- [[16-Configuration Reference]] — `[api]` config section
