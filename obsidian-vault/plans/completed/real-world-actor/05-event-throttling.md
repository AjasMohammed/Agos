---
title: "Phase 5: Event Throttling & Agent Wake-up"
tags:
  - plan
  - real-world
  - events
  - throttling
  - phase-5
date: 2026-04-08
status: complete
effort: 1.5d
priority: high
---

# Phase 5: Event Throttling & Agent Wake-up

> Add debouncing, batching, and rate limiting to webhook events so a noisy external service doesn't bankrupt the user in LLM API costs, then wire events to create agent tasks.

---

## Why This Phase

Phase 4 adds the ingress endpoint — webhooks arrive and emit `WebhookReceived` events. But without throttling, a GitHub repo receiving 500 pushes/minute would spawn 500 LLM inference calls. At ~$0.03/call, that's $15/minute from a single noisy repo.

This phase adds three layers of protection:
1. **Token-bucket rate limiter** — hard cap per endpoint (e.g., 10 events/min)
2. **Debouncer** — waits N seconds after the first event, batches all events in that window
3. **Agent wake-up** — creates an `AgentTask` with the batched payload as context

---

## Current State

- Phase 4 provides `WebhookRegistry`, ingress handler, and `WebhookReceived` events
- `WebhookEndpoint` has `debounce_seconds` field
- Event bus exists in kernel (`EventBus` / `EventSubscription` system)
- `AgentTask` creation is well-established in the kernel
- No throttling or batching logic exists

## Target State

- `EventThrottle` with per-endpoint token bucket + debounce window
- `WebhookBatcher` that aggregates events within the debounce window
- `WebhookWakeUp` service that creates `AgentTask` from batched webhook events
- Configurable per-endpoint: rate limit, debounce window, max batch size
- Cost cap: optional max USD spend per endpoint per hour

---

## Detailed Subtasks

### 1. Token-bucket rate limiter

**File:** `crates/agentos-kernel/src/webhook_throttle.rs` (new)

```rust
use std::collections::HashMap;
use std::sync::RwLock;
use tokio::time::Instant;

pub struct TokenBucket {
    pub capacity: u32,          // max burst
    pub refill_rate: f64,       // tokens per second
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32, per_minute: u32) -> Self;

    /// Try to consume one token. Returns true if allowed, false if rate-limited.
    pub fn try_consume(&mut self) -> bool;
}

pub struct WebhookThrottle {
    buckets: RwLock<HashMap<WebhookEndpointID, TokenBucket>>,
    default_capacity: u32,       // default: 60
    default_per_minute: u32,     // default: 30
}

impl WebhookThrottle {
    pub fn new(default_capacity: u32, default_per_minute: u32) -> Self;

    /// Check if an event from this endpoint is allowed through
    pub fn allow(&self, endpoint_id: &WebhookEndpointID) -> bool;

    /// Configure a specific endpoint's rate limit
    pub fn configure(&self, endpoint_id: WebhookEndpointID, capacity: u32, per_minute: u32);

    /// Remove an endpoint's bucket (on deletion)
    pub fn remove(&self, endpoint_id: &WebhookEndpointID);
}
```

### 2. Debounce batcher

**File:** `crates/agentos-kernel/src/webhook_batcher.rs` (new)

```rust
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

pub struct PendingBatch {
    pub endpoint_id: WebhookEndpointID,
    pub agent_id: AgentID,
    pub events: Vec<WebhookEvent>,
    pub first_event_at: DateTime<Utc>,
    pub debounce_until: DateTime<Utc>,
}

pub struct WebhookBatcher {
    pending: RwLock<HashMap<WebhookEndpointID, PendingBatch>>,
    wake_tx: mpsc::Sender<BatchReady>,
    max_batch_size: usize,        // default: 50 events
}

impl WebhookBatcher {
    pub fn new(wake_tx: mpsc::Sender<BatchReady>, max_batch_size: usize) -> Self;

    /// Add an event. If this is the first event for this endpoint,
    /// start the debounce timer. If batch hits max_batch_size, flush immediately.
    pub async fn add_event(&self, event: WebhookEvent, endpoint: &WebhookEndpoint);

    /// Flush a batch for an endpoint (called when debounce timer expires)
    pub async fn flush(&self, endpoint_id: &WebhookEndpointID);

    /// Background loop that checks for expired debounce windows every second
    pub async fn run_flush_loop(&self, cancel: CancellationToken);
}

pub struct BatchReady {
    pub endpoint_id: WebhookEndpointID,
    pub agent_id: AgentID,
    pub events: Vec<WebhookEvent>,
    pub provider: WebhookProvider,
}
```

### 3. Agent wake-up service

**File:** `crates/agentos-kernel/src/webhook_wakeup.rs` (new)

```rust
pub struct WebhookWakeUp {
    kernel: Arc<Kernel>,
    rx: mpsc::Receiver<BatchReady>,
}

impl WebhookWakeUp {
    pub fn new(kernel: Arc<Kernel>, rx: mpsc::Receiver<BatchReady>) -> Self;

    /// Run the wake-up loop. For each BatchReady:
    /// 1. Format a system prompt with the batched payloads
    /// 2. Create an AgentTask assigned to the endpoint's agent
    /// 3. Inject the formatted context
    /// 4. Submit the task to the kernel scheduler
    pub async fn run(mut self, cancel: CancellationToken);

    /// Format the webhook batch as a context message for the agent
    fn format_webhook_context(batch: &BatchReady) -> String {
        // "You are receiving this task because your webhook endpoint received
        //  {n} events from {provider} between {first} and {last}.
        //  Analyze the payloads and take appropriate action.
        //
        //  Events:
        //  [JSON array of payloads, truncated to 32KB total]"
    }
}
```

### 4. Wire into kernel boot

**File:** `crates/agentos-kernel/src/kernel.rs`

During kernel initialization:
```rust
// Create channel for batcher → wake-up communication
let (wake_tx, wake_rx) = mpsc::channel(256);

let webhook_throttle = Arc::new(WebhookThrottle::new(60, 30));
let webhook_batcher = Arc::new(WebhookBatcher::new(wake_tx, 50));
let webhook_wakeup = WebhookWakeUp::new(kernel.clone(), wake_rx);

// Spawn background tasks
tokio::spawn(webhook_batcher.clone().run_flush_loop(cancel.clone()));
tokio::spawn(webhook_wakeup.run(cancel.clone()));
```

### 5. Connect ingress handler to throttle + batcher

**File:** `crates/agentos-web/src/handlers/webhooks.rs`

Update `incoming_webhook` handler (from Phase 4):
```rust
// After signature verification:
if !state.webhook_throttle.allow(&endpoint_id) {
    return Ok(StatusCode::TOO_MANY_REQUESTS);  // 429
}
state.webhook_batcher.add_event(webhook_event, &endpoint).await;
Ok(StatusCode::OK)
```

### 6. Configuration

**File:** `config/default.toml`

Add section:
```toml
[webhooks]
default_rate_limit_per_minute = 30
default_rate_limit_burst = 60
default_debounce_seconds = 60
max_batch_size = 50
max_payload_bytes = 65536       # 64KB per event
context_max_bytes = 32768       # 32KB total injected into agent context
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/webhook_throttle.rs` | **New** — Token bucket rate limiter |
| `crates/agentos-kernel/src/webhook_batcher.rs` | **New** — Debounce + batch aggregation |
| `crates/agentos-kernel/src/webhook_wakeup.rs` | **New** — Agent task creation from batches |
| `crates/agentos-kernel/src/kernel.rs` | Add throttle, batcher, wakeup fields; spawn background tasks |
| `crates/agentos-web/src/handlers/webhooks.rs` | Wire throttle + batcher into ingress handler |
| `crates/agentos-web/src/state.rs` | Add `webhook_throttle` and `webhook_batcher` to `AppState` |
| `config/default.toml` | Add `[webhooks]` configuration section |

---

## Dependencies

- **Requires:** Phase 4 (Webhook Ingress)
- **Blocks:** None (end of Subsystem B)

---

## Test Plan

1. **Unit: token bucket** — Verify bucket allows `capacity` events, then rejects; verify refill after time passes
2. **Unit: debounce batching** — Add 5 events within debounce window, verify they're flushed as one batch after the window
3. **Unit: max batch flush** — Add `max_batch_size` events, verify immediate flush before debounce expires
4. **Unit: context formatting** — Verify `format_webhook_context` produces valid markdown with truncated payloads
5. **Integration: throttle → 429** — Send events past rate limit, verify 429 responses
6. **Integration: full pipeline** — Send 3 webhooks within debounce window, verify single `AgentTask` created with all 3 payloads
7. **Performance: batching under load** — Send 100 events/second, verify no more than `per_minute` tasks created

---

## Verification

```bash
cargo test -p agentos-kernel
cargo test -p agentos-web
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
