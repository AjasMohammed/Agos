use agentos_types::AgentOSError;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, warn};

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

/// Parse `retry-after` header value to a Duration.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
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
pub async fn send_with_retry(
    provider: &str,
    policy: &RetryPolicy,
    breaker: &CircuitBreaker,
    build_request: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, AgentOSError> {
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
                last_error = Some(format!("Network error: {}", e));

                if attempt < policy.max_retries {
                    let delay = policy.delay_for_attempt(attempt, None);
                    debug!(
                        provider,
                        attempt,
                        error = %e,
                        delay_ms = delay.as_millis() as u64,
                        "Network error, retrying"
                    );
                    sleep(delay).await;
                } else {
                    warn!(
                        provider,
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
}
