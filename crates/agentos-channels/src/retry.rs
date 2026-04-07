use agentos_types::AgentOSError;
use std::future::Future;
use std::time::Duration;
use tracing::warn;

/// Configuration for exponential backoff retry.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of attempts (including the initial attempt).
    pub max_attempts: u32,
    /// Base delay between retries (doubled each attempt).
    pub base_delay: Duration,
    /// Maximum delay cap.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

/// Returns `true` if the error looks like a transient/retryable failure.
fn is_retryable(e: &AgentOSError) -> bool {
    match e {
        AgentOSError::ToolExecutionFailed { reason, .. } => {
            let r = reason.to_lowercase();
            // Network errors, timeouts, rate limits, server errors
            r.contains("timeout")
                || r.contains("connection")
                || r.contains("429")
                || r.contains("500")
                || r.contains("502")
                || r.contains("503")
                || r.contains("504")
                || r.contains("timed out")
                || r.contains("reset by peer")
                || r.contains("broken pipe")
        }
        _ => false,
    }
}

/// Execute an async operation with exponential backoff retry.
///
/// Only retries on transient errors (network failures, rate limits, server errors).
/// Non-retryable errors (auth failures, bad requests) are returned immediately.
pub async fn with_retry<F, Fut, T>(
    policy: &RetryPolicy,
    channel_name: &str,
    mut operation: F,
) -> Result<T, AgentOSError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AgentOSError>>,
{
    let mut delay = policy.base_delay;

    for attempt in 1..=policy.max_attempts {
        match operation().await {
            Ok(val) => return Ok(val),
            Err(e) if is_retryable(&e) && attempt < policy.max_attempts => {
                warn!(
                    channel = channel_name,
                    attempt,
                    max_attempts = policy.max_attempts,
                    delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "Retryable error, backing off"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(policy.max_delay);
            }
            Err(e) => return Err(e),
        }
    }

    unreachable!("loop exits via return")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_succeeds_immediately() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let result = with_retry(&policy, "test", || async { Ok::<_, AgentOSError>(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_retries_on_transient_error() {
        let counter = Arc::new(AtomicU32::new(0));
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let counter_clone = counter.clone();
        let result = with_retry(&policy, "test", || {
            let c = counter_clone.clone();
            async move {
                let attempt = c.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt < 3 {
                    Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "test".into(),
                        reason: "connection timeout".into(),
                    })
                } else {
                    Ok(99)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 99);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_no_retry_on_non_transient() {
        let counter = Arc::new(AtomicU32::new(0));
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let counter_clone = counter.clone();
        let result: Result<i32, _> = with_retry(&policy, "test", || {
            let c = counter_clone.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "test".into(),
                    reason: "auth error: invalid token".into(),
                })
            }
        })
        .await;
        assert!(result.is_err());
        // Should not retry non-transient errors
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_exhausts_retries() {
        let counter = Arc::new(AtomicU32::new(0));
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let counter_clone = counter.clone();
        let result: Result<i32, _> = with_retry(&policy, "test", || {
            let c = counter_clone.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "test".into(),
                    reason: "502 Bad Gateway".into(),
                })
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }
}
