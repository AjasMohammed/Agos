use agentos_types::WebhookEndpointID;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tokio::time::Instant;

/// Per-endpoint token bucket for webhook rate limiting.
///
/// Each webhook endpoint gets its own bucket. When a webhook arrives, a token
/// is consumed. If the bucket is empty, the webhook is rejected with 429.
/// Tokens refill at a steady rate (e.g., 0.5/sec = 30/min).
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: u32, per_minute: u32) -> Self {
        Self {
            capacity: capacity as f64,
            tokens: capacity as f64,
            refill_rate: per_minute as f64 / 60.0,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume one token. Returns true if allowed, false if rate-limited.
    fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = now;
    }
}

/// Manages per-endpoint rate limiting for webhook ingress.
pub struct WebhookThrottle {
    buckets: RwLock<HashMap<WebhookEndpointID, TokenBucket>>,
    default_capacity: u32,
    default_per_minute: u32,
}

impl WebhookThrottle {
    pub fn new(default_capacity: u32, default_per_minute: u32) -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
            default_capacity,
            default_per_minute,
        }
    }

    /// Check if an event from this endpoint is allowed through.
    /// Creates a default bucket if one doesn't exist for this endpoint.
    pub async fn allow(&self, endpoint_id: &WebhookEndpointID) -> bool {
        let mut buckets = self.buckets.write().await;
        let bucket = buckets
            .entry(*endpoint_id)
            .or_insert_with(|| TokenBucket::new(self.default_capacity, self.default_per_minute));
        bucket.try_consume()
    }

    /// Configure a specific endpoint's rate limit (replaces any existing bucket).
    pub async fn configure(&self, endpoint_id: WebhookEndpointID, capacity: u32, per_minute: u32) {
        let mut buckets = self.buckets.write().await;
        buckets.insert(endpoint_id, TokenBucket::new(capacity, per_minute));
    }

    /// Remove an endpoint's bucket (called when endpoint is deleted).
    pub async fn remove(&self, endpoint_id: &WebhookEndpointID) {
        self.buckets.write().await.remove(endpoint_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_allow_within_capacity() {
        let throttle = WebhookThrottle::new(5, 60);
        let id = WebhookEndpointID::new();

        // Should allow 5 events (capacity)
        for _ in 0..5 {
            assert!(throttle.allow(&id).await);
        }
    }

    #[tokio::test]
    async fn test_reject_over_capacity() {
        let throttle = WebhookThrottle::new(3, 60);
        let id = WebhookEndpointID::new();

        // Exhaust capacity
        for _ in 0..3 {
            assert!(throttle.allow(&id).await);
        }

        // 4th should be rejected
        assert!(!throttle.allow(&id).await);
    }

    #[tokio::test]
    async fn test_refill_over_time() {
        let throttle = WebhookThrottle::new(1, 6000); // 100/sec for fast test
        let id = WebhookEndpointID::new();

        // Consume the one token
        assert!(throttle.allow(&id).await);
        assert!(!throttle.allow(&id).await);

        // Wait for refill (100/sec = 10ms per token)
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Should have refilled at least 1 token
        assert!(throttle.allow(&id).await);
    }

    #[tokio::test]
    async fn test_separate_buckets_per_endpoint() {
        let throttle = WebhookThrottle::new(1, 60);
        let id_a = WebhookEndpointID::new();
        let id_b = WebhookEndpointID::new();

        // Exhaust A
        assert!(throttle.allow(&id_a).await);
        assert!(!throttle.allow(&id_a).await);

        // B should still work
        assert!(throttle.allow(&id_b).await);
    }

    #[tokio::test]
    async fn test_custom_configuration() {
        let throttle = WebhookThrottle::new(10, 60);
        let id = WebhookEndpointID::new();

        // Override to capacity=2
        throttle.configure(id, 2, 60).await;

        assert!(throttle.allow(&id).await);
        assert!(throttle.allow(&id).await);
        assert!(!throttle.allow(&id).await);
    }

    #[tokio::test]
    async fn test_remove_bucket() {
        let throttle = WebhookThrottle::new(1, 60);
        let id = WebhookEndpointID::new();

        // Exhaust
        assert!(throttle.allow(&id).await);
        assert!(!throttle.allow(&id).await);

        // Remove and re-create (fresh bucket)
        throttle.remove(&id).await;
        assert!(throttle.allow(&id).await); // new bucket, full capacity
    }
}
