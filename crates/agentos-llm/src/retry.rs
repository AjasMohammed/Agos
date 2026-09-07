use agentos_types::AgentOSError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::sleep;
use tracing::{debug, warn};

/// Default per-*endpoint* in-flight request cap. Set to 8 — enough for
/// reasonable parallelism (batch tasks, multiple chat sessions) while
/// preventing one runaway loop from saturating an upstream that's
/// already rate-limiting (observed in 2026-05-08 logs: a single
/// `provider="custom"` endpoint returned 5 distinct 429 storms within
/// 30 minutes once two chat sessions ran concurrently).
///
/// The cap is per upstream endpoint, not per adapter instance: on
/// 2026-08-31 five agents pointed at the same local Ollama each built
/// their own limiter through `OllamaCore::new` (one call per agent
/// connect), so 5 × 8 = 40 requests went in flight against a server
/// sized for 8 and produced a sustained 429 storm. See
/// [`concurrency_limiter_for`].
pub const DEFAULT_PROVIDER_CONCURRENCY: usize = 8;

/// Process-wide registry of concurrency limiters, one per upstream
/// endpoint. Never pruned — an entry is a `String` key plus an 8-permit
/// semaphore, and the set of distinct endpoints a process talks to is
/// bounded by its configured providers.
// ponytail: `std::sync::Mutex` — the critical section is one map lookup
// with no await inside; a `tokio::sync::Mutex` would only add a yield.
static ENDPOINT_LIMITERS: OnceLock<Mutex<HashMap<String, Arc<Semaphore>>>> = OnceLock::new();

/// Normalise an upstream base URL into a registry key.
///
/// Trims surrounding whitespace, drops trailing `/` (so `.../v1` and
/// `.../v1/` are one endpoint), and lowercases the scheme + authority,
/// which are case-insensitive per RFC 3986 §3.1/§3.2.2. The path is
/// left alone because it is case-*sensitive*; folding it could merge
/// two genuinely distinct endpoints. A scheme-less `host:port/path`
/// splits at the same place, so its path is preserved too.
fn endpoint_key(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let authority_start = trimmed.find("://").map_or(0, |i| i + 3);
    let authority_end = trimmed[authority_start..]
        .find('/')
        .map_or(trimmed.len(), |j| authority_start + j);
    let (authority, path) = trimmed.split_at(authority_end);
    format!("{}{}", authority.to_ascii_lowercase(), path)
}

/// Get the shared concurrency limiter for `base_url`, creating it on
/// first use. Adapters must call this in their constructor and pass
/// `&self.concurrency` to every [`send_with_retry`] call, so retries
/// hold the permit and *every* adapter instance aimed at the same
/// upstream queues behind the same 8 permits instead of stacking up
/// additional 429s (the 2026-08-31 five-agents-one-Ollama storm).
pub fn concurrency_limiter_for(base_url: &str) -> Arc<Semaphore> {
    let registry = ENDPOINT_LIMITERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = registry.lock().unwrap_or_else(|e| e.into_inner());
    let limiter = guard
        .entry(endpoint_key(base_url))
        .or_insert_with(|| Arc::new(Semaphore::new(DEFAULT_PROVIDER_CONCURRENCY)));
    Arc::clone(limiter)
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

/// Turn a non-retryable HTTP failure into an actionable message.
///
/// Groq (and other OpenAI-compatible gateways) answer a request whose token
/// count exceeds the *account's* per-minute budget with `413 Payload Too
/// Large` + `code: rate_limit_exceeded`. That is not a malformed request and
/// not a transient 429: when the single request is larger than the whole TPM
/// allowance, no backoff window will ever admit it, so 413 stays out of
/// `is_retryable_status` and the operator needs to be told to raise the tier
/// or move the agent, not to wait. The raw body is kept verbatim on the end
/// so the provider's own detail is never lost.
fn friendly_reason(status: u16, body: &str) -> String {
    let raw = format!("API error {}: {}", status, body);
    if status == 413 && body.contains("rate_limit_exceeded") {
        return format!(
            "Provider token-per-minute limit is smaller than a single AgentOS request \
             (a turn carries the system prompt plus tool schemas). Raise the account tier \
             or point this agent at a provider with a larger TPM budget. (raw: {raw})"
        );
    }
    raw
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
/// is not cloneable). Returns the successful `reqwest::Response` **and the
/// concurrency permit it was fetched under**, or the last error.
///
/// Without a `concurrency` limiter, parallel callers all race against
/// the same upstream and any rate-limit response is multiplied by the
/// number of in-flight requests. Pass the `Arc<Semaphore>` shared by
/// every adapter aimed at that endpoint so retries inherit the permit
/// and other callers queue rather than pile on. See
/// [`concurrency_limiter_for`].
///
/// # Holding the permit
///
/// The permit is handed back rather than dropped on return because for a
/// `stream: true` request the response headers arrive at the *first
/// token* — dropping here would release the slot while the expensive part,
/// token generation, is still running, and the cap would bound only
/// non-streaming calls. Non-streaming callers bind it to `_permit` and let
/// it drop at the end of their scope; streaming callers must keep that
/// binding alive until the body stream is fully consumed.
pub async fn send_with_retry(
    provider: &str,
    policy: &RetryPolicy,
    breaker: &CircuitBreaker,
    concurrency: Option<&Arc<Semaphore>>,
    build_request: impl Fn() -> reqwest::RequestBuilder,
) -> Result<(reqwest::Response, Option<OwnedSemaphorePermit>), AgentOSError> {
    // Acquire an in-flight slot for this provider before checking the
    // breaker — if we are over the concurrency cap we'd rather queue
    // than race ahead and trip the breaker on a 429. The permit is
    // held across all retries, so per-call backoff is honoured but no
    // additional caller can stomp the same upstream window.
    //
    // The wait is deliberately unbounded. The permits are shared by every
    // adapter instance aimed at this endpoint, so on a local model — where
    // a non-streaming generation holds its permit for the whole 20-90s
    // generation — the 9th concurrent caller legitimately queues for
    // minutes. A timeout here would turn that benign wait into a hard
    // error, and the kernel's task executor treats an `LLMError` as fatal:
    // it fails the task outright (a provider fallback chain exists but is
    // opt-in and empty by default). Permits are released by the adapter's
    // client request timeout on the non-streaming path, and the executor
    // bounds the whole inference with its own hard timeout plus a watchdog.
    //
    // The one hole: on the STREAMING path the permit is held while the
    // adapter awaits the consumer channel, which reqwest's timeout does not
    // cover (a parked send means the body is never polled, so its deadline
    // never fires). The kernel's chat consumer therefore bounds its own send
    // (`STREAM_CONSUMER_SEND_TIMEOUT`) and treats a stalled reader as a
    // dropped client — without that, a handful of stalled browsers would pin
    // every permit on the endpoint. Any new consumer of a streaming adapter
    // must do the same.
    let permit = if let Some(sem) = concurrency {
        if sem.available_permits() == 0 {
            // Queued callers are invisible otherwise: the executor's watchdog
            // cannot tell "waiting for a permit" from "slow inference" and
            // escalates a healthy queued task as a runaway one.
            debug!(
                provider = provider,
                "provider concurrency saturated — queueing for a permit"
            );
        }
        match Arc::clone(sem).acquire_owned().await {
            Ok(p) => Some(p),
            Err(e) => {
                return Err(AgentOSError::LLMError {
                    provider: provider.to_string(),
                    reason: format!("concurrency semaphore closed for provider {provider}: {e}"),
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
                return Ok((response, permit));
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
                    reason: friendly_reason(status.as_u16(), &body),
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

                // Read/idle timeouts (connection established, server stopped
                // responding) take the full per-attempt timeout to surface —
                // ~60s for the streaming clients — and rarely recover on retry.
                // Retrying them `max_retries` times turned a single ~60s failure
                // into a ~4min wait for interactive chat (observed in kernel logs
                // against a flapping NVIDIA gateway). Fail fast on these. The
                // failure is already recorded against the breaker above; connect
                // -level timeouts (`is_connect`) are cheap (~10s) and stay
                // retryable below.
                if e.is_timeout() && !e.is_connect() {
                    warn!(
                        provider,
                        kind,
                        error = %e,
                        "Read/idle timeout — not retrying (fail fast)"
                    );
                    return Err(AgentOSError::LLMError {
                        provider: provider.to_string(),
                        reason: full_reason,
                    });
                }

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
        assert!(!is_retryable_status(413));
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

    /// A read/idle timeout (connection accepted, server never responds) must
    /// NOT be retried — retrying a 60s-per-attempt timeout turned a single
    /// failure into a ~4min wait for interactive chat. We assert by counting
    /// accepted connections: fail-fast means exactly one, a retry loop would
    /// produce `max_retries + 1`.
    #[tokio::test]
    async fn read_timeout_is_not_retried() {
        use std::sync::atomic::AtomicU32;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepts = Arc::new(AtomicU32::new(0));
        let accepts_cl = Arc::clone(&accepts);
        tokio::spawn(async move {
            // Accept connections and hold them open without ever responding,
            // forcing the client's request to hit its read/overall timeout.
            let mut held = Vec::new();
            loop {
                if let Ok((stream, _)) = listener.accept().await {
                    accepts_cl.fetch_add(1, Ordering::SeqCst);
                    held.push(stream);
                }
            }
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(300))
            .build()
            .unwrap();
        let url = format!("http://{addr}/");
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(50),
            backoff_factor: 2.0,
        };
        let breaker = CircuitBreaker::default();

        let start = Instant::now();
        let res = send_with_retry("test", &policy, &breaker, None, || client.get(&url)).await;
        let elapsed = start.elapsed();

        assert!(
            res.is_err(),
            "hanging server should produce a timeout error"
        );
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "read/idle timeout must not be retried (expected exactly one connection)"
        );
        // And it should fail fast — far under 4 × the per-attempt timeout.
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout fail-fast took too long: {elapsed:?}"
        );
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

    // The registry is process-global and tests share a process, so every
    // test below uses its own `.invalid` host to stay independent.

    /// Regression test for the 2026-08-31 storm: five `OllamaCore::new`
    /// calls against one host must hand back one limiter, not five.
    #[test]
    fn endpoint_limiter_is_shared_per_endpoint() {
        let a = concurrency_limiter_for("http://shared.invalid:11434");
        let b = concurrency_limiter_for("http://shared.invalid:11434");
        assert!(Arc::ptr_eq(&a, &b), "same endpoint must share one limiter");

        let other_port = concurrency_limiter_for("http://shared.invalid:11435");
        let other_host = concurrency_limiter_for("http://elsewhere.invalid:11434");
        assert!(!Arc::ptr_eq(&a, &other_port), "port must key separately");
        assert!(!Arc::ptr_eq(&a, &other_host), "host must key separately");
    }

    #[test]
    fn endpoint_key_normalises_slash_whitespace_and_scheme_case() {
        let canonical = endpoint_key("http://localhost:11434");
        assert_eq!(endpoint_key("http://localhost:11434/"), canonical);
        assert_eq!(endpoint_key("  http://localhost:11434  "), canonical);
        assert_eq!(endpoint_key("HTTP://LocalHost:11434"), canonical);
        // Distinct upstreams stay distinct.
        assert_ne!(endpoint_key("http://localhost:11435"), canonical);
        assert_ne!(endpoint_key("http://127.0.0.1:11434"), canonical);
        // Paths are case-sensitive per RFC 3986 and must not be folded —
        // including when there is no scheme to anchor the authority on.
        assert_ne!(endpoint_key("http://h/V1"), endpoint_key("http://h/v1"));
        assert_ne!(
            endpoint_key("localhost:11434/V1"),
            endpoint_key("localhost:11434/v1")
        );
        // The scheme-less authority is still folded.
        assert_eq!(
            endpoint_key("LocalHost:11434/v1"),
            endpoint_key("localhost:11434/v1")
        );

        // And the normalisation actually collapses to one limiter.
        let plain = concurrency_limiter_for("http://norm.invalid:11434");
        let slash = concurrency_limiter_for("http://norm.invalid:11434/");
        let padded = concurrency_limiter_for("  http://norm.invalid:11434  ");
        assert!(Arc::ptr_eq(&plain, &slash), "trailing slash must collapse");
        assert!(Arc::ptr_eq(&plain, &padded), "whitespace must collapse");
    }

    /// Sharing the `Arc` is not enough — the permits themselves must be
    /// the same pool, which is what the buggy per-instance limiter broke.
    #[test]
    fn endpoint_limiter_shares_permits_across_handles() {
        let url = "http://permits.invalid:11434";
        let first = concurrency_limiter_for(url);
        let held: Vec<_> = (0..DEFAULT_PROVIDER_CONCURRENCY)
            .map(|_| Arc::clone(&first).try_acquire_owned().expect("within cap"))
            .collect();

        let second = concurrency_limiter_for(url);
        assert!(
            Arc::clone(&second).try_acquire_owned().is_err(),
            "a second handle must see the first handle's permits as taken"
        );
        drop(held);
        assert!(
            Arc::clone(&second).try_acquire_owned().is_ok(),
            "permit must be available again once released"
        );
    }

    /// The cap must bound *streaming* generations, not just non-streaming
    /// ones. A `stream: true` response's headers land at the first token, so
    /// a `send_with_retry` that dropped its permit on return would leave the
    /// whole of token generation — the expensive part, and the load the cap
    /// exists for — outside the limit. Serve headers, keep the chunked body
    /// open, and assert the permit survives the return.
    #[tokio::test]
    async fn streaming_response_keeps_permit_until_caller_drops_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // Status line, headers and one chunk — a complete 200 as far as the
        // client is concerned — with the chunked body deliberately unfinished.
        const HEAD_AND_CHUNK: &[u8] = b"HTTP/1.1 200 OK\r\n\
            Content-Type: text/event-stream\r\n\
            Transfer-Encoding: chunked\r\n\
            \r\n\
            5\r\nhello\r\n";

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Hold every accepted socket open so the body keeps streaming.
            let mut held = Vec::new();
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(HEAD_AND_CHUNK).await;
                let _ = sock.flush().await;
                held.push(sock);
            }
        });

        let sem = Arc::new(Semaphore::new(1));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let url = format!("http://{addr}/");
        let (response, permit) = send_with_retry(
            "test",
            &RetryPolicy::default(),
            &CircuitBreaker::default(),
            Some(&sem),
            || client.get(&url),
        )
        .await
        .expect("headers arrive before the body completes");

        assert_eq!(
            sem.available_permits(),
            0,
            "permit must still be held while the body stream is unconsumed"
        );
        drop(response);
        assert_eq!(
            sem.available_permits(),
            0,
            "dropping the response must not release the caller's permit"
        );
        drop(permit);
        assert_eq!(
            sem.available_permits(),
            1,
            "permit must be released once the streaming caller drops it"
        );
    }

    /// A saturated endpoint must make the caller *queue*, never fail it. With
    /// permits shared per endpoint, a non-streaming local generation holds one
    /// for its whole 20-90s run, and the kernel's task executor treats an
    /// `LLMError` as fatal — the acquire timeout this used to carry turned
    /// that benign wait into a dead task under exactly the load the cap exists
    /// for. Real time, not a paused clock: this crate's tokio has no
    /// `test-util` feature, so the assertion is "still queued after a beat"
    /// rather than "still queued after the old 30s bound".
    #[tokio::test]
    async fn saturated_endpoint_queues_the_caller_instead_of_erroring() {
        let sem = Arc::new(Semaphore::new(1));
        let held = Arc::clone(&sem).acquire_owned().await.unwrap();

        let sem_cl = Arc::clone(&sem);
        let queued = tokio::spawn(async move {
            send_with_retry(
                "test",
                &RetryPolicy::default(),
                &CircuitBreaker::default(),
                Some(&sem_cl),
                // Never reached while the permit is held; an unroutable
                // address keeps this honest if it somehow is.
                || reqwest::Client::new().get("http://127.0.0.1:1/"),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !queued.is_finished(),
            "a saturated endpoint must queue the caller, not fail it"
        );

        drop(held);
        queued.abort();
    }
}

#[cfg(test)]
mod friendly_reason_tests {
    use super::friendly_reason;

    #[test]
    fn tpm_413_gets_actionable_hint_and_keeps_raw() {
        let body = r#"{"error":{"message":"Request too large ... tokens per minute (TPM): Limit 8000, Requested 15601","type":"tokens","code":"rate_limit_exceeded"}}"#;
        let out = friendly_reason(413, body);
        assert!(out.contains("token-per-minute limit"), "{out}");
        assert!(
            out.contains("Requested 15601"),
            "raw body must survive: {out}"
        );
    }

    #[test]
    fn other_errors_pass_through_unchanged() {
        // A 413 that is a genuine oversized body, and an unrelated 400, must
        // keep the exact `API error {status}: {body}` shape that
        // `friendly_ollama_reason` and the log greps anchor on.
        assert_eq!(
            friendly_reason(413, "request entity too large"),
            "API error 413: request entity too large"
        );
        assert_eq!(
            friendly_reason(400, "bad model"),
            "API error 400: bad model"
        );
    }
}
