---
title: "Phase 4: Webhook Ingress"
tags:
  - plan
  - real-world
  - events
  - webhooks
  - phase-4
date: 2026-04-08
status: complete
effort: 2d
priority: high
---

# Phase 4: Webhook Ingress

> Add inbound webhook endpoints to `agentos-web` so external services (GitHub, Stripe, PagerDuty) can push events into AgentOS, triggering agent tasks.

---

## Why This Phase

AgentOS currently operates reactively — agents wake up when a user sends a message or a cron schedule fires. In the real world, events happen asynchronously: a PR is opened, a payment fails, a server alert fires. Polling wastes compute and adds latency.

The existing `agentos-channels` WebhookAdapter is **outbound-only** — it sends messages to external webhook URLs. This phase adds the **inbound** side: AgentOS receives webhooks from external services.

The `EventCategory::ExternalEvents` and `EventType::WebhookReceived` already exist in `agentos-types` but have no ingress path.

---

## Current State

- `agentos-web` has Axum router with rate limiting (60 req/min) and auth middleware
- `EventCategory::ExternalEvents` and `EventType::WebhookReceived` exist in types
- `agentos-channels::WebhookAdapter` is outbound only (HMAC-SHA256 signed sends)
- No inbound webhook endpoints exist
- No webhook endpoint registry exists

## Target State

- `POST /api/v1/webhooks/incoming/:endpoint_id` — receives external webhooks
- `WebhookEndpoint` registry (SQLite-backed) mapping endpoint UUIDs to agents and config
- Signature verification per provider (GitHub X-Hub-Signature-256, Stripe Stripe-Signature, generic HMAC)
- Received webhooks emit `WebhookReceived` events into the event bus
- KernelCommands for endpoint CRUD
- Agent tool: `webhook.create_endpoint(provider, debounce_seconds)` returns the URL

---

## Detailed Subtasks

### 1. Define `WebhookEndpoint` types

**File:** `crates/agentos-types/src/webhook.rs` (new)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::{AgentID, define_id};

define_id!(WebhookEndpointID);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    pub id: WebhookEndpointID,
    pub agent_id: AgentID,             // which agent owns this endpoint
    pub provider: WebhookProvider,
    pub secret: String,                // HMAC secret for signature verification
    pub debounce_seconds: u64,         // 0 = no debounce
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub last_received_at: Option<DateTime<Utc>>,
    pub total_received: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebhookProvider {
    GitHub,
    Stripe,
    Slack,
    PagerDuty,
    Generic,                           // HMAC-SHA256 with X-Signature header
    Custom { signature_header: String, algorithm: SignatureAlgorithm },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignatureAlgorithm {
    HmacSha256,
    HmacSha1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEvent {
    pub endpoint_id: WebhookEndpointID,
    pub provider: WebhookProvider,
    pub headers: HashMap<String, String>,  // selected safe headers only
    pub payload: serde_json::Value,
    pub received_at: DateTime<Utc>,
    pub signature_valid: bool,
}
```

### 2. Webhook endpoint registry

**File:** `crates/agentos-kernel/src/webhook_registry.rs` (new)

```rust
pub struct WebhookRegistry {
    db_path: PathBuf,
    endpoints: RwLock<HashMap<WebhookEndpointID, WebhookEndpoint>>,
}

impl WebhookRegistry {
    pub fn new(db_path: PathBuf) -> Result<Self, AgentOSError>;

    /// Create a new endpoint, returns the endpoint ID (used in the URL)
    pub fn create_endpoint(
        &self,
        agent_id: AgentID,
        provider: WebhookProvider,
        debounce_seconds: u64,
    ) -> Result<WebhookEndpoint, AgentOSError>;

    /// Look up an endpoint by ID (for incoming request handling)
    pub fn get_endpoint(&self, id: &WebhookEndpointID) -> Option<WebhookEndpoint>;

    /// List all endpoints (optionally filtered by agent)
    pub fn list_endpoints(&self, agent_id: Option<&AgentID>) -> Vec<WebhookEndpoint>;

    /// Delete an endpoint
    pub fn delete_endpoint(&self, id: &WebhookEndpointID) -> Result<(), AgentOSError>;

    /// Update last_received_at and increment total_received
    pub fn record_receipt(&self, id: &WebhookEndpointID) -> Result<(), AgentOSError>;
}
```

SQLite schema:
```sql
CREATE TABLE IF NOT EXISTS webhook_endpoints (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    encrypted_secret BLOB NOT NULL,
    debounce_seconds INTEGER NOT NULL DEFAULT 0,
    active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    last_received_at TEXT,
    total_received INTEGER NOT NULL DEFAULT 0
);
```

### 3. Signature verification

**File:** `crates/agentos-kernel/src/webhook_verify.rs` (new)

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Verify a webhook signature based on provider conventions
pub fn verify_webhook_signature(
    provider: &WebhookProvider,
    secret: &str,
    body: &[u8],
    headers: &HeaderMap,
) -> Result<bool, AgentOSError> {
    match provider {
        WebhookProvider::GitHub => {
            // X-Hub-Signature-256: sha256=<hex>
            let sig = headers.get("x-hub-signature-256")...;
            verify_hmac_sha256(secret, body, &sig)
        }
        WebhookProvider::Stripe => {
            // Stripe-Signature: t=<timestamp>,v1=<hex>
            let sig = headers.get("stripe-signature")...;
            verify_stripe_signature(secret, body, &sig)
        }
        WebhookProvider::Generic => {
            // X-Signature: <hex>
            let sig = headers.get("x-signature")...;
            verify_hmac_sha256(secret, body, &sig)
        }
        // ... other providers
    }
}

fn verify_hmac_sha256(secret: &str, body: &[u8], expected_hex: &str) -> Result<bool, AgentOSError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
    mac.update(body);
    // Constant-time comparison
    Ok(mac.verify_slice(&hex::decode(expected_hex)?).is_ok())
}
```

### 4. Ingress HTTP handler

**File:** `crates/agentos-web/src/handlers/webhooks.rs` (new)

```rust
/// POST /api/v1/webhooks/incoming/:endpoint_id
/// This endpoint is UNAUTHENTICATED (external services can't carry our auth token)
/// Security is via the webhook secret + signature verification
pub async fn incoming_webhook(
    State(state): State<Arc<AppState>>,
    Path(endpoint_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    // 1. Parse endpoint_id as WebhookEndpointID
    // 2. Look up in WebhookRegistry
    // 3. If not found or not active → 404
    // 4. Verify signature using provider-specific logic
    // 5. If invalid → 401 + audit log
    // 6. Parse body as JSON (or store raw if not JSON)
    // 7. Emit WebhookReceived event via event bus
    // 8. Record receipt in registry
    // 9. Return 200 OK immediately (processing is async)
}
```

**Critical:** This endpoint must:
- Return `200 OK` within 5 seconds (external services timeout and retry)
- Never block on LLM inference or task execution
- Buffer the payload for async processing
- Rate limit per endpoint_id (separate from global rate limit)

### 5. Register routes

**File:** `crates/agentos-web/src/router.rs`

```rust
// Webhook ingress — OUTSIDE auth middleware (external callers)
.route("/api/v1/webhooks/incoming/{endpoint_id}", post(webhooks::incoming_webhook))
```

### 6. KernelCommand + CLI wiring

**File:** `crates/agentos-bus/src/message.rs`

```rust
CreateWebhookEndpoint { agent_id: AgentID, provider: String, debounce_seconds: u64 },
ListWebhookEndpoints { agent_id: Option<AgentID> },
DeleteWebhookEndpoint { endpoint_id: String },
```

**File:** `crates/agentos-kernel/src/commands/webhook.rs` (new)

**File:** `crates/agentos-cli/src/commands/webhook.rs` (new) — `agentos webhook create/list/delete`

### 7. Agent tool

**File:** `tools/core/webhook-create.toml` (new)

```toml
[manifest]
name = "webhook-create"
version = "1.0.0"
description = "Create a webhook endpoint that external services can POST to"
author = "agentos-core"
trust_tier = "core"

[capabilities_required]
permissions = ["webhook.create:x"]

[intent_schema]
type = "execute"
target_tool = "webhook-create"

[sandbox]
network = false
fs_write = false
max_memory_mb = 32
max_cpu_ms = 5000
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/webhook.rs` | **New** — `WebhookEndpoint`, `WebhookProvider`, `WebhookEvent` types |
| `crates/agentos-types/src/lib.rs` | Re-export webhook module |
| `crates/agentos-kernel/src/webhook_registry.rs` | **New** — SQLite-backed endpoint registry |
| `crates/agentos-kernel/src/webhook_verify.rs` | **New** — Provider-specific signature verification |
| `crates/agentos-kernel/src/kernel.rs` | Add `webhook_registry` field |
| `crates/agentos-kernel/src/commands/webhook.rs` | **New** — Command handlers |
| `crates/agentos-kernel/src/run_loop.rs` | Add dispatch arms |
| `crates/agentos-web/src/handlers/webhooks.rs` | **New** — Ingress HTTP handler |
| `crates/agentos-web/src/router.rs` | Register webhook ingress route (unauthenticated) |
| `crates/agentos-bus/src/message.rs` | Add webhook command variants |
| `crates/agentos-cli/src/commands/webhook.rs` | **New** — CLI subcommands |
| `tools/core/webhook-create.toml` | **New** — Agent tool manifest |

---

## Dependencies

- **Requires:** None (independent track)
- **Blocks:** Phase 5 (Event Throttling & Wake-up)

---

## Test Plan

1. **Unit: endpoint CRUD** — Create, list, get, delete endpoints; verify SQLite persistence
2. **Unit: GitHub signature verification** — Known payload + secret + expected signature → valid
3. **Unit: Stripe signature verification** — Known payload with timestamp + signature → valid
4. **Unit: invalid signature** — Wrong secret → rejected (constant-time)
5. **Integration: ingress handler** — POST to `/api/v1/webhooks/incoming/<id>` with valid signature, verify 200 + event emitted
6. **Integration: unknown endpoint** — POST to non-existent endpoint_id → 404
7. **Integration: disabled endpoint** — POST to inactive endpoint → 404
8. **Security: no auth bypass** — Verify the ingress route doesn't grant access to any other authenticated endpoints
9. **Performance: fast response** — Verify handler returns within 100ms (no blocking on task creation)

---

## Verification

```bash
cargo test -p agentos-types
cargo test -p agentos-kernel
cargo test -p agentos-web
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
