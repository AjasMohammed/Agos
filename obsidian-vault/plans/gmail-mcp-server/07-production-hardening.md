---
title: Phase 7 — Production Hardening
tags:
  - rate-limiting
  - retries
  - audit
  - errors
  - phase-7
date: 2026-04-18
status: planned
effort: 1.5d
priority: high
---

# Phase 7 — Production Hardening

> Rate-limiter, retry policy, structured error taxonomy, and audit logging — the features that separate a demo from a tool you can leave running unattended against a real inbox.

---

## Why this phase

Phases 4–6 shipped functionality. This phase makes it reliable under load, transparent when it goes wrong, and safe when Google pushes back. Hardening lands before v1 ships.

---

## Deliverables

- Token-bucket rate limiter with per-account buckets and Gmail-quota-aware costs.
- Retry policy: exponential backoff + jitter for 429 / 5xx; no retry for 4xx client errors; absolute cap on total time.
- Audit log: newline-delimited JSON to stderr by default, or file/syslog via `--audit-sink`.
- Typed error taxonomy with deterministic JSON-RPC error codes.
- `prometheus` metrics endpoint (optional, via `--metrics-addr`).
- `--log-level`, `--log-format json|text`, `--audit-sink` flags.

---

## Rate limiter

Per-account token bucket at 200 units/sec (Gmail's per-user default is 250/s; leave 50 units headroom for token refreshes and internal probes):

```rust
// crates/gmail-mcp-core/src/ratelimit.rs
pub struct RateLimiter {
    capacity: u32,                    // max tokens in bucket
    refill_per_sec: u32,
    tokens: tokio::sync::Mutex<BucketState>,
}

struct BucketState { tokens: f64, last: Instant }

impl RateLimiter {
    pub fn new(rate: u32) -> Self {
        Self {
            capacity: rate,
            refill_per_sec: rate,
            tokens: tokio::sync::Mutex::new(BucketState { tokens: rate as f64, last: Instant::now() }),
        }
    }
    pub async fn acquire(&self, cost: u32) -> Result<(), RateLimitError> {
        loop {
            let wait = {
                let mut state = self.tokens.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(state.last).as_secs_f64();
                state.tokens = (state.tokens + elapsed * self.refill_per_sec as f64).min(self.capacity as f64);
                state.last = now;
                if state.tokens >= cost as f64 {
                    state.tokens -= cost as f64;
                    return Ok(());
                }
                let deficit = cost as f64 - state.tokens;
                Duration::from_secs_f64(deficit / self.refill_per_sec as f64)
            };
            if wait > Duration::from_secs(30) {
                return Err(RateLimitError::WouldBlockTooLong(wait));
            }
            tokio::time::sleep(wait).await;
        }
    }
}
```

---

## Retry policy

```rust
// crates/gmail-mcp-core/src/retry.rs
pub async fn with_retry<F, Fut, T>(op: F, policy: RetryPolicy) -> Result<T, GmailError>
where
    F: Fn(u32) -> Fut,   // attempt number -> future
    Fut: Future<Output = Result<T, GmailError>>,
{
    let mut attempt = 0u32;
    let mut elapsed = Duration::ZERO;
    loop {
        let start = Instant::now();
        match op(attempt).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let kind = classify(&e);
                if !kind.retryable() || attempt >= policy.max_attempts {
                    return Err(e);
                }
                let backoff = backoff(attempt, &policy);
                elapsed += start.elapsed() + backoff;
                if elapsed > policy.total_cap {
                    return Err(GmailError::Transport(format!("retry budget exhausted after {attempt} attempts")));
                }
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
        }
    }
}

pub struct RetryPolicy {
    pub max_attempts: u32,        // default 5
    pub base: Duration,           // default 1s
    pub max_backoff: Duration,    // default 30s
    pub total_cap: Duration,      // default 120s
}

fn backoff(attempt: u32, p: &RetryPolicy) -> Duration {
    let exp = p.base * 2u32.pow(attempt);
    let clamped = exp.min(p.max_backoff);
    let jitter = rand::random::<u32>() % 250;   // up to 250ms jitter
    clamped + Duration::from_millis(jitter as u64)
}

enum RetryKind { NoRetry, RetryTransient, RetryAuth }

fn classify(e: &GmailError) -> RetryKind {
    match e {
        GmailError::RateLimited
        | GmailError::Transport(_) => RetryKind::RetryTransient,
        GmailError::AuthExpired => RetryKind::RetryAuth,    // refresh + retry once
        _ => RetryKind::NoRetry,
    }
}

impl RetryKind {
    fn retryable(&self) -> bool { !matches!(self, Self::NoRetry) }
}
```

`AuthExpired` gets special handling — the `Client` catches it, calls `TokenManager::refresh`, and retries once before giving up.

---

## Error taxonomy (final)

```rust
// crates/gmail-mcp-core/src/errors.rs
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("authentication expired")]
    AuthExpired,
    #[error("authentication revoked")]
    AuthRevoked,
    #[error("missing scopes: {required:?}")]
    ScopeMissing { required: Vec<String> },
    #[error("rate limited; retry after {retry_after:?}")]
    RateLimited { retry_after: Option<Duration> },
    #[error("quota exhausted")]
    QuotaExhausted,
    #[error("message {id} not found")]
    MessageNotFound { id: String },
    #[error("thread {id} not found")]
    ThreadNotFound { id: String },
    #[error("label '{name}' not found")]
    LabelNotFound { name: String },
    #[error("filter {id} not found")]
    FilterNotFound { id: String },
    #[error("invalid query: {reason}")]
    InvalidQuery { reason: String },
    #[error("attachment too large: {size} bytes (limit {limit})")]
    AttachmentTooLarge { size: u64, limit: u64 },
    #[error("recipient limit exceeded: {count} (limit {limit})")]
    RecipientLimitExceeded { count: usize, limit: usize },
    #[error("batch size exceeded: {count} (limit {limit})")]
    BatchSizeExceeded { count: usize, limit: usize },
    #[error("Gmail API: {0}")]
    Gmail(#[from] GmailError),
    #[error("internal error: {0}")]
    Internal(String),
}
```

JSON-RPC error code mapping:

| Variant | Code | Notes |
|---------|------|-------|
| `AuthExpired`, `AuthRevoked` | -32001 | Client should prompt re-auth |
| `ScopeMissing` | -32002 | Client shows which scopes; remediation known |
| `RateLimited` | -32003 | `data.retry_after_seconds` populated |
| `QuotaExhausted` | -32004 | No retry possible soon |
| `*NotFound` | -32005 | Benign; agent handles |
| `Invalid*`, `*TooLarge`, `*Exceeded` | -32602 | Client error |
| `Gmail(_)`, `Internal` | -32000 | Server error; audit-log |

---

## Audit log

Every tool call emits a single structured event:

```json
{
  "ts": "2026-04-18T14:32:11.123Z",
  "event": "tool_call",
  "account": "default",
  "account_email_hash": "sha256:7b2c…",        // privacy — never the plain email
  "tool": "gmail_send",
  "args_hash": "sha256:abc…",                  // for dedup / replay detection
  "scopes_used": ["gmail.send"],
  "result": "ok",                              // ok | error
  "error_code": null,
  "error_kind": null,
  "duration_ms": 234,
  "gmail_cost_units": 100,
  "message_ids": ["18a2…"],                   // effect IDs where meaningful
  "trace_id": "01HYX…"                         // ULID
}
```

Emitter in Phase 2's dispatch, enriched with data from the tool layer:

```rust
// crates/gmail-mcp-core/src/audit.rs
pub struct AuditSink { inner: Arc<dyn AuditEmit> }

#[async_trait]
pub trait AuditEmit: Send + Sync {
    async fn emit(&self, event: AuditEvent);
}

pub struct StderrJsonEmitter;

#[async_trait]
impl AuditEmit for StderrJsonEmitter {
    async fn emit(&self, event: AuditEvent) {
        if let Ok(line) = serde_json::to_string(&event) {
            // stderr — stdout is MCP protocol.
            eprintln!("{line}");
        }
    }
}
```

CLI flag: `--audit-sink stderr|file:/path/to/audit.jsonl|syslog`.

**Privacy rules:**
- Email addresses are hashed with SHA-256 before logging. A sibling `--audit-privacy off` flag disables hashing for users who want plaintext addresses in their own logs.
- Message subjects and bodies are never logged.
- Attachment filenames and sizes may be logged; content never.
- Input `args` are hashed for fingerprinting, not stored.

---

## Additional events

- `auth.login` — on successful OAuth completion.
- `auth.logout` — on `gmail-mcp logout`.
- `auth.refresh` — periodic, on token refresh.
- `auth.revoked` — when refresh returns `invalid_grant`.
- `server.startup` / `server.shutdown`.
- `rate_limit.hit` — whenever the local limiter blocks >1s.
- `api.retry` — every retry attempt with the reason.

---

## Metrics (stretch within this phase)

Optional `--metrics-addr 127.0.0.1:9464` exposes a Prometheus endpoint:

```
gmail_mcp_tool_calls_total{tool="gmail_search",result="ok"} 47
gmail_mcp_tool_duration_seconds_bucket{tool="gmail_send",le="1.0"} 12
gmail_mcp_gmail_retries_total{reason="429"} 3
gmail_mcp_auth_refreshes_total 2
gmail_mcp_rate_limit_blocked_seconds_sum 0.84
```

Implementation: `metrics` + `metrics-exporter-prometheus` crates. Minor — ~80 lines of code, gated behind a feature flag so users who don't want the HTTP listener don't pay for it.

---

## Test plan

1. Rate limiter: 201 quick acquires of cost 1 → the 201st sleeps ~5ms (depending on timer granularity).
2. Rate limiter: cost 500 against capacity 200 → `WouldBlockTooLong` immediately.
3. Retry: 429 then 200 → success, 1 retry, backoff ≥1s.
4. Retry: 3 × 500 then 200 → success, 3 retries with increasing backoff.
5. Retry: 404 → no retry, immediate error.
6. Retry budget exhaustion: 30s of transient errors → error after total cap.
7. Audit: successful tool call → emitted event contains expected fields; no subject/body leakage.
8. Audit: auth refresh → `auth.refresh` event with no token leakage.
9. Privacy: by default, `account_email_hash` starts with `sha256:` and is 71 chars.
10. Privacy opt-out: `--audit-privacy off` emits plain email.
11. Metrics endpoint returns HTTP 200 + Prometheus-formatted body when enabled.

---

## Verification

```bash
cargo test -p gmail-mcp-core ratelimit
cargo test -p gmail-mcp-core retry
cargo test -p gmail-mcp-core audit

# Manual:
./target/debug/gmail-mcp serve --audit-sink stderr 2>audit.jsonl &
# ... exercise tools from an MCP client ...
jq -s 'group_by(.event) | map({event: .[0].event, count: length})' audit.jsonl
# → summary of events emitted
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/gmail-mcp-core/src/ratelimit.rs` | New |
| `crates/gmail-mcp-core/src/retry.rs` | New |
| `crates/gmail-mcp-core/src/audit.rs` | New |
| `crates/gmail-mcp-core/src/errors.rs` | Expand `ToolError` + JSON-RPC mapping |
| `crates/gmail-mcp-core/src/gmail/client.rs` | Wrap `request` in `with_retry` |
| `crates/gmail-mcp-core/src/mcp/server.rs` | Emit audit events; threaded sink |
| `crates/gmail-mcp/src/serve.rs` | Wire `--audit-sink`, `--metrics-addr`, etc. |
| `crates/gmail-mcp-core/Cargo.toml` | `metrics`, `metrics-exporter-prometheus` (feature-gated) |

---

## Dependencies

- **Requires:** Phases 4, 5 (both needed for the full event + retry surface).
- **Blocks:** Phase 9 (releases must be built with hardening in place).

---

## Related

- [[Gmail MCP Server Plan]]
- [[Gmail MCP Server Data Flow]]
- [[09-distribution-and-releases]]
