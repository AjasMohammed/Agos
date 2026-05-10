use agentos_types::AgentOSError;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing::{debug, warn};

/// Default per-provider in-flight request cap. Set to 8 — enough for
/// reasonable parallelism (batch tasks, multiple chat sessions) while
/// preventing one runaway loop from saturating an upstream that's
/// already rate-limiting (observed in 2026-05-08 logs: a single
/// `provider="custom"` endpoint returned 5 distinct 429 storms within
/// 30 minutes once two chat sessions ran concurrently).
pub const DEFAULT_PROVIDER_CONCURRENCY: usize = 8;

/// Max time a caller will block waiting for a permit before giving up
/// with a typed error. Bounded so a long `Retry-After: 60s` storm
/// cannot stall an entire chat session indefinitely — the queued
/// caller surfaces a clear "provider saturated" message after this
/// window and the user/loop can fall through to a different provider.
pub const CONCURRENCY_ACQUIRE_TIMEOUT_SECS: u64 = 30;

/// Construct a fresh per-provider concurrency limiter. Adapters should
/// store one of these per instance and pass `&self.concurrency` to
/// every [`send_with_retry`] call so retries hold the permit and other
/// callers wait their turn instead of stacking up additional 429s.
pub fn default_concurrency_limiter() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(DEFAULT_PROVIDER_CONCURRENCY))
}

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

/// Jitter derived by hashing thread ID + clock nanos to decorrelate concurrent callers.
fn rand_jitter(max_ms: f64) -> f64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;

    let mut hasher = DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    nanos.hash(&mut hasher);
    let hash = hasher.finish();

    (hash % 1000) as f64 / 1000.0 * max_ms
}

/// Whether an HTTP status code is retryable.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 529)
}

/// Parse `Retry-After` header to a Duration. Accepts the two spec
/// forms (RFC 9110 §10.2.3): an integer "delta-seconds" *or* an
/// HTTP-date. Anything we cannot interpret returns `None`, which lets
/// the caller fall back to the policy's exponential schedule rather
/// than misinterpreting a malformed header as zero delay.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Form 1: delta-seconds (integer).
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // Form 2: HTTP-date. chrono parses RFC 1123 / RFC 850 / asctime.
    type DateParser = fn(&str) -> chrono::ParseResult<chrono::DateTime<chrono::Utc>>;
    let parsers: &[DateParser] = &[
        |s| chrono::DateTime::parse_from_rfc2822(s).map(|dt| dt.with_timezone(&chrono::Utc)),
        |s| {
            chrono::NaiveDateTime::parse_from_str(s, "%A, %d-%b-%y %H:%M:%S GMT").map(|naive| {
                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive, chrono::Utc)
            })
        },
        |s| {
            chrono::NaiveDateTime::parse_from_str(s, "%a %b %e %H:%M:%S %Y").map(|naive| {
                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive, chrono::Utc)
            })
        },
    ];
    for parse in parsers {
        if let Ok(target) = parse(trimmed) {
            let now = chrono::Utc::now();
            if target >= now {
                // `to_std()` only fails on negative spans, which the
                // `>=` already guards against; falls back to 0 instead
                // of returning `None` so the caller doesn't reset to
                // the exponential schedule on the rare exact-equal case.
                return Some(
                    target
                        .signed_duration_since(now)
                        .to_std()
                        .unwrap_or(Duration::from_secs(0)),
                );
            }
            // Past timestamp ⇒ retry immediately.
            return Some(Duration::from_secs(0));
        }
    }
    None
}

/// Simple circuit breaker that tracks consecutive failures.
pub struct CircuitBreaker {
    consecutive_failures: AtomicU32,
    is_open: AtomicBool,
    last_failure: std::sync::Mutex<Option<Instant>>,
    /// Number of consecutive failures before tripping.
    pub failure_threshold: u32,
    /// Cooldown before a half-open probe attempt is allowed.
    pub cooldown: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
            is_open: AtomicBool::new(false),
            last_failure: std::sync::Mutex::new(None),
            failure_threshold,
            cooldown,
        }
    }

    /// Check if the circuit allows a request through.
    pub fn can_attempt(&self) -> bool {
        if !self.is_open.load(Ordering::Acquire) {
            return true;
        }
        // Half-open: allow if cooldown has elapsed since last failure.
        let guard = self.last_failure.lock().unwrap_or_else(|e| e.into_inner());
        guard.map(|t| t.elapsed() >= self.cooldown).unwrap_or(true)
    }

    /// Record a successful response. Resets the breaker.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.is_open.store(false, Ordering::Release);
    }

    /// Record a failure. May trip the breaker.
    pub fn record_failure(&self) {
        let count = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut guard) = self.last_failure.lock() {
            *guard = Some(Instant::now());
        }
        if count >= self.failure_threshold {
            self.is_open.store(true, Ordering::Release);
            warn!(
                failures = count,
                "Circuit breaker tripped after {} consecutive failures", count
            );
        }
    }

    /// Whether the breaker is currently open (tripped).
    pub fn is_open(&self) -> bool {
        self.is_open.load(Ordering::Acquire)
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(5, Duration::from_secs(30))
    }
}

/// Send an HTTP request with retry and circuit breaker logic.
///
/// The `build_request` closure is called for each attempt (since `reqwest::RequestBuilder`
/// is not cloneable). Returns the successful `reqwest::Response` or the last error.
///
/// Without a `concurrency` limiter, parallel callers all race against
/// the same upstream and any rate-limit response is multiplied by the
/// number of in-flight requests. Pass an `Arc<Semaphore>` shared across
/// the adapter instance so retries inherit the permit and other callers
/// queue rather than pile on. See [`default_concurrency_limiter`].
pub async fn send_with_retry(
    provider: &str,
    policy: &RetryPolicy,
    breaker: &CircuitBreaker,
    concurrency: Option<&Arc<Semaphore>>,
    build_request: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, AgentOSError> {
    // Acquire an in-flight slot for this provider before checking the
    // breaker — if we are over the concurrency cap we'd rather queue
    // than race ahead and trip the breaker on a 429. The permit is
    // held across all retries, so per-call backoff is honoured but no
    // additional caller can stomp the same upstream window.
    let _permit = if let Some(sem) = concurrency {
        match tokio::time::timeout(
            Duration::from_secs(CONCURRENCY_ACQUIRE_TIMEOUT_SECS),
            Arc::clone(sem).acquire_owned(),
        )
        .await
        {
            Ok(Ok(p)) => Some(p),
            Ok(Err(e)) => {
                return Err(AgentOSError::LLMError {
                    provider: provider.to_string(),
                    reason: format!("concurrency semaphore closed for provider {provider}: {e}"),
                });
            }
            Err(_) => {
                // Bounded queue prevents a slow upstream from stalling
                // every caller indefinitely. Surface a typed error so
                // the caller can log it and (eventually) fall through
                // to a different provider in the fallback chain.
                return Err(AgentOSError::LLMError {
                    provider: provider.to_string(),
                    reason: format!(
                        "provider {provider} concurrency saturated — \
                         no permit available within {CONCURRENCY_ACQUIRE_TIMEOUT_SECS}s"
                    ),
                });
            }
        }
    } else {
        None
    };

    if !breaker.can_attempt() {
        return Err(AgentOSError::LLMError {
            provider: provider.to_string(),
            reason: "Circuit breaker is open — provider temporarily unavailable".to_string(),
        });
    }

    let mut last_error = None;
    for attempt in 0..=policy.max_retries {
        let res = build_request().send().await;
        match res {
            Ok(response) if response.status().is_success() => {
                breaker.record_success();
                return Ok(response);
            }
            Ok(response) if is_retryable_status(response.status().as_u16()) => {
                let status = response.status().as_u16();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after);
                let body = response.text().await.unwrap_or_default();

                breaker.record_failure();
                last_error = Some(format!("HTTP {}: {}", status, body));

                if attempt < policy.max_retries {
                    let delay = policy.delay_for_attempt(attempt, retry_after);
                    debug!(
                        provider,
                        status,
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        "Retryable error, backing off"
                    );
                    sleep(delay).await;
                } else {
                    warn!(
                        provider,
                        status,
                        "All retries exhausted after {} attempts",
                        policy.max_retries + 1
                    );
                }
            }
            Ok(response) => {
                // Non-retryable HTTP error (400, 401, 403, 404, etc.)
                // Don't record as circuit breaker failure — these are client errors.
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(AgentOSError::LLMError {
                    provider: provider.to_string(),
                    reason: format!("API error {}: {}", status, body),
                });
            }
            Err(e) => {
                // Network / connection error — retryable.
                breaker.record_failure();
                // Chain error sources so we see the root cause (e.g., serde_json
                // errors hidden behind reqwest "builder error").
                let mut full_reason = format!("Network error: {}", e);
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    full_reason += &format!(" -> {}", s);
                    src = std::error::Error::source(s);
                }
                // Also classify the reqwest error kind for easier diagnosis.
                let kind = if e.is_builder() {
                    "builder"
                } else if e.is_connect() {
                    "connect"
                } else if e.is_timeout() {
                    "timeout"
                } else if e.is_request() {
                    "request"
                } else if e.is_body() {
                    "body"
                } else if e.is_decode() {
                    "decode"
                } else {
                    "other"
                };
                full_reason += &format!(" [kind={}]", kind);
                last_error = Some(full_reason);

                if attempt < policy.max_retries {
                    let delay = policy.delay_for_attempt(attempt, None);
                    warn!(
                        provider,
                        attempt,
                        kind,
                        error = %e,
                        delay_ms = delay.as_millis() as u64,
                        "Network error, retrying"
                    );
                    sleep(delay).await;
                } else {
                    warn!(
                        provider,
                        kind,
                        error = %e,
                        "All retries exhausted after {} attempts (network errors)",
                        policy.max_retries + 1
                    );
                }
            }
        }
    }

    Err(AgentOSError::LLMError {
        provider: provider.to_string(),
        reason: format!(
            "All {} retries exhausted. Last error: {}",
            policy.max_retries,
            last_error.unwrap_or_default()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_policy_delay_increases() {
        let policy = RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
            backoff_factor: 2.0,
        };
        let d0 = policy.delay_for_attempt(0, None);
        let d1 = policy.delay_for_attempt(1, None);
        let d2 = policy.delay_for_attempt(2, None);
        // Each delay should be roughly double the previous (plus jitter).
        assert!(d1 > d0, "d1={:?} should be > d0={:?}", d1, d0);
        assert!(d2 > d1, "d2={:?} should be > d1={:?}", d2, d1);
    }

    #[test]
    fn test_retry_policy_respects_max_delay() {
        let policy = RetryPolicy {
            max_retries: 10,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(5),
            backoff_factor: 10.0,
        };
        let d = policy.delay_for_attempt(5, None);
        assert!(d <= Duration::from_secs(5));
    }

    #[test]
    fn test_retry_policy_uses_retry_after() {
        let policy = RetryPolicy::default();
        let d = policy.delay_for_attempt(0, Some(Duration::from_secs(10)));
        assert_eq!(d, Duration::from_secs(10));
    }

    #[test]
    fn test_retry_after_caps_at_max_delay() {
        let policy = RetryPolicy {
            max_delay: Duration::from_secs(5),
            ..RetryPolicy::default()
        };
        let d = policy.delay_for_attempt(0, Some(Duration::from_secs(120)));
        assert_eq!(d, Duration::from_secs(5));
    }

    #[test]
    fn test_is_retryable_status() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(529));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(200));
    }

    #[test]
    fn test_parse_retry_after() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after(" 5 "), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("abc"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    #[test]
    fn test_parse_retry_after_http_date() {
        // RFC 9110 §10.2.3 form 2: HTTP-date.
        let future = chrono::Utc::now() + chrono::Duration::seconds(45);
        let s = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let parsed = parse_retry_after(&s).expect("HTTP-date should parse");
        // Allow 5s clock drift / parser rounding.
        assert!(
            parsed.as_secs() >= 40 && parsed.as_secs() <= 50,
            "got {:?}",
            parsed
        );
    }

    #[test]
    fn test_parse_retry_after_past_http_date_returns_zero() {
        // Past timestamp ⇒ retry immediately.
        let past = chrono::Utc::now() - chrono::Duration::seconds(60);
        let s = past.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert_eq!(parse_retry_after(&s), Some(Duration::from_secs(0)));
    }

    #[test]
    fn test_circuit_breaker_trips_after_threshold() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));
        assert!(cb.can_attempt());
        cb.record_failure();
        assert!(cb.can_attempt());
        cb.record_failure();
        assert!(cb.can_attempt());
        cb.record_failure(); // 3rd failure trips the breaker.
        assert!(cb.is_open());
        // can_attempt returns false because cooldown hasn't elapsed.
        assert!(!cb.can_attempt());
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let cb = CircuitBreaker::new(2, Duration::from_secs(60));
        cb.record_failure();
        cb.record_failure();
        assert!(cb.is_open());
        cb.record_success();
        assert!(!cb.is_open());
        assert!(cb.can_attempt());
    }

    #[test]
    fn test_circuit_breaker_default() {
        let cb = CircuitBreaker::default();
        assert_eq!(cb.failure_threshold, 5);
        assert!(cb.can_attempt());
        assert!(!cb.is_open());
    }

    #[test]
    fn test_circuit_breaker_half_open_after_cooldown() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(1));
        cb.record_failure(); // trips immediately at threshold=1
        assert!(cb.is_open());
        // Immediately after tripping, cooldown hasn't elapsed.
        assert!(!cb.can_attempt());
        // Wait for cooldown to expire.
        std::thread::sleep(Duration::from_millis(5));
        // Half-open: probe attempt should be allowed.
        assert!(cb.can_attempt());
    }

    #[test]
    fn test_is_retryable_status_includes_408_504() {
        assert!(is_retryable_status(408));
        assert!(is_retryable_status(504));
    }

    /// Regression test for the per-provider concurrency cap. Two
    /// `acquire_owned` calls on a 1-permit semaphore must serialise:
    /// the second waits until the first releases. Proves
    /// `send_with_retry` (which acquires the same way) will queue
    /// excess callers instead of stacking up additional 429s on the
    /// upstream during a rate-limit storm.
    #[tokio::test]
    async fn concurrency_limiter_serialises_callers() {
        let sem = Arc::new(Semaphore::new(1));
        let p1 = Arc::clone(&sem).acquire_owned().await.unwrap();
        // Second acquisition must not complete while p1 is held.
        let sem2 = Arc::clone(&sem);
        let race = tokio::spawn(async move { sem2.acquire_owned().await.unwrap() });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!race.is_finished(), "second acquire jumped the queue");
        drop(p1);
        // After release, the queued acquire should resolve quickly.
        let _p2 = tokio::time::timeout(Duration::from_millis(200), race)
            .await
            .expect("p2 acquired after p1 released")
            .expect("join ok");
    }
}
