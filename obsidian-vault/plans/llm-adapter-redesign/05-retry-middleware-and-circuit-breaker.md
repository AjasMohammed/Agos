---
title: "Phase 5: Retry Middleware and Circuit Breaker"
tags:
  - llm
  - v3
  - plan
date: 2026-03-24
status: planned
effort: 1.5d
priority: high
---

# Phase 5: Retry Middleware and Circuit Breaker

> Add automatic retry with exponential backoff for transient LLM API errors, and a circuit breaker that marks providers as unhealthy after consecutive failures.

---

## Why This Phase

A single 429 (rate limit) or 503 (overloaded) response from a provider currently kills the entire agent task with an `AgentOSError::LLMError`. In production agentic workloads:

- Rate limits are expected and normal, especially under parallel agent load.
- Provider outages are transient -- a 30-second blip should not abort a 10-minute task.
- The kernel has no visibility into whether an error is retryable or permanent.

After this phase, retryable errors are transparently retried with exponential backoff and jitter. Persistent failures trip a circuit breaker that prevents wasting time on a dead provider.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Retry logic | None -- first error is fatal | Exponential backoff with jitter, max 3 attempts |
| Rate limit handling | Error returned to kernel | Read `retry-after` header, sleep, retry |
| Circuit breaker | None | Track consecutive failures per adapter; trip after 5 in 60s |
| `HealthStatus` integration | Only used by explicit `health_check()` | Updated by circuit breaker state |
| Error categorization | All errors are `LLMError` | New `is_retryable()` helper on error responses |

---

## What to Do

### Step 1: Create `crates/agentos-llm/src/retry.rs`

New module with retry policy and circuit breaker:

```rust
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use tokio::time::sleep;
use tracing::{warn, debug};

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (excluding the initial attempt).
    pub max_retries: u32,
    /// Base delay before first retry.
    pub base_delay: Duration,
    /// Maximum delay between retries.
    pub max_delay: Duration,
    /// Multiplier for exponential backoff.
    pub backoff_factor: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_factor: 2.0,
        }
    }
}

impl RetryPolicy {
    /// Calculate delay for attempt N (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(ra) = retry_after {
            return ra.min(self.max_delay);
        }
        let base_ms = self.base_delay.as_millis() as f64;
        let delay_ms = base_ms * self.backoff_factor.powi(attempt as i32);
        let jitter_ms = rand_jitter(delay_ms * 0.1);
        let total = Duration::from_millis((delay_ms + jitter_ms) as u64);
        total.min(self.max_delay)
    }
}

fn rand_jitter(max_ms: f64) -> f64 {
    // Simple deterministic jitter from system time nanos.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as f64;
    (nanos % 1000.0) / 1000.0 * max_ms
}

/// Whether an HTTP status code is retryable.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 529)
}

/// Parse `retry-after` header value to a Duration.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    // Try seconds first.
    if let Ok(secs) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // Could also be an HTTP-date, but providers typically use seconds.
    None
}

/// Simple circuit breaker.
pub struct CircuitBreaker {
    consecutive_failures: AtomicU32,
    is_open: AtomicBool,
    last_failure: std::sync::Mutex<Option<Instant>>,
    /// Number of consecutive failures before tripping.
    pub failure_threshold: u32,
    /// Time window for counting failures.
    pub failure_window: Duration,
    /// Cooldown before half-open attempt.
    pub cooldown: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, failure_window: Duration, cooldown: Duration) -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
            is_open: AtomicBool::new(false),
            last_failure: std::sync::Mutex::new(None),
            failure_threshold,
            failure_window,
            cooldown,
        }
    }

    /// Check if the circuit allows a request through.
    pub fn can_attempt(&self) -> bool {
        if !self.is_open.load(Ordering::Relaxed) {
            return true;
        }
        // Half-open: allow if cooldown has elapsed.
        let guard = self.last_failure.lock().unwrap();
        guard.map(|t| t.elapsed() >= self.cooldown).unwrap_or(true)
    }

    /// Record a successful response. Resets the breaker.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.is_open.store(false, Ordering::Relaxed);
    }

    /// Record a failure. May trip the breaker.
    pub fn record_failure(&self) {
        let count = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        *self.last_failure.lock().unwrap() = Some(Instant::now());
        if count >= self.failure_threshold {
            self.is_open.store(true, Ordering::Relaxed);
            warn!(
                failures = count,
                "Circuit breaker tripped after {} consecutive failures",
                count
            );
        }
    }

    pub fn is_open(&self) -> bool {
        self.is_open.load(Ordering::Relaxed)
    }
}
```

### Step 2: Integrate retry into each adapter's `infer_with_tools`

Add a `retry_policy: RetryPolicy` and `circuit_breaker: CircuitBreaker` field to each adapter struct. Wrap the HTTP send call:

```rust
async fn send_with_retry(
    &self,
    request_builder: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, AgentOSError> {
    if !self.circuit_breaker.can_attempt() {
        return Err(AgentOSError::LLMError {
            provider: self.provider_name().to_string(),
            reason: "Circuit breaker is open — provider is temporarily unavailable".to_string(),
        });
    }

    let mut last_error = None;
    for attempt in 0..=self.retry_policy.max_retries {
        let res = request_builder().send().await;
        match res {
            Ok(response) if response.status().is_success() => {
                self.circuit_breaker.record_success();
                return Ok(response);
            }
            Ok(response) if is_retryable_status(response.status().as_u16()) => {
                let status = response.status().as_u16();
                let retry_after = response.headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after);
                let body = response.text().await.unwrap_or_default();
                warn!(
                    provider = self.provider_name(),
                    status, attempt, "Retryable error, backing off"
                );
                self.circuit_breaker.record_failure();
                last_error = Some(format!("HTTP {}: {}", status, body));

                if attempt < self.retry_policy.max_retries {
                    let delay = self.retry_policy.delay_for_attempt(attempt, retry_after);
                    sleep(delay).await;
                }
            }
            Ok(response) => {
                // Non-retryable error (400, 401, 403, etc.)
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(AgentOSError::LLMError {
                    provider: self.provider_name().to_string(),
                    reason: format!("API error {}: {}", status, body),
                });
            }
            Err(e) => {
                // Network error -- retryable
                warn!(provider = self.provider_name(), attempt, error = %e, "Network error, retrying");
                self.circuit_breaker.record_failure();
                last_error = Some(format!("Network error: {}", e));

                if attempt < self.retry_policy.max_retries {
                    let delay = self.retry_policy.delay_for_attempt(attempt, None);
                    sleep(delay).await;
                }
            }
        }
    }

    Err(AgentOSError::LLMError {
        provider: self.provider_name().to_string(),
        reason: format!(
            "All {} retries exhausted. Last error: {}",
            self.retry_policy.max_retries,
            last_error.unwrap_or_default()
        ),
    })
}
```

### Step 3: Update `health_check` to reflect circuit breaker state

If the circuit breaker is open, `health_check` should return `HealthStatus::Unhealthy` without making a network call.

### Step 4: Register module in `lib.rs`

Add `pub mod retry;` and re-export `RetryPolicy`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/retry.rs` | New file: `RetryPolicy`, `CircuitBreaker`, `is_retryable_status`, `parse_retry_after` |
| `crates/agentos-llm/src/lib.rs` | Add `pub mod retry;`, re-export `RetryPolicy` |
| `crates/agentos-llm/src/openai.rs` | Add `retry_policy` + `circuit_breaker` fields, wrap HTTP calls with `send_with_retry` |
| `crates/agentos-llm/src/anthropic.rs` | Same retry integration |
| `crates/agentos-llm/src/gemini.rs` | Same retry integration |
| `crates/agentos-llm/src/ollama.rs` | Same retry integration (Ollama can also have transient errors) |
| `crates/agentos-llm/src/custom.rs` | Same retry integration |

---

## Prerequisites

[[01-core-types-and-trait-redesign]] must be complete.

---

## Test Plan

- `cargo build -p agentos-llm` must pass
- Add test `test_retry_policy_delay_increases` -- verify exponential growth with cap
- Add test `test_is_retryable_status` -- 429, 500, 502, 503 retryable; 400, 401, 403 not
- Add test `test_circuit_breaker_trips_after_threshold` -- 5 failures trips the breaker, `can_attempt()` returns false
- Add test `test_circuit_breaker_resets_on_success` -- success after failures resets counter
- Add test `test_parse_retry_after` -- "30" parses to 30s, "abc" returns None
- Integration test: local TCP server returns 503 twice then 200 -- adapter retries and succeeds
- Integration test: local TCP server returns 503 forever -- adapter exhausts retries and returns error

---

## Verification

```bash
cargo build -p agentos-llm
cargo test -p agentos-llm -- --nocapture
cargo build --workspace
cargo test --workspace
cargo clippy -p agentos-llm -- -D warnings
cargo fmt --all -- --check
```
