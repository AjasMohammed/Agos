use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A simple sliding-window rate limiter for a single connection/agent.
pub struct RateLimiter {
    window_start: Instant,
    count: u32,
    max_per_window: u32,
    window_duration: Duration,
}

impl RateLimiter {
    pub fn new(max_per_second: u32) -> Self {
        Self {
            window_start: Instant::now(),
            count: 0,
            max_per_window: max_per_second,
            window_duration: Duration::from_secs(1),
        }
    }

    /// Returns Ok(()) if allowed, Err(wait_duration) if rate-limited.
    pub fn check(&mut self) -> Result<(), Duration> {
        let now = Instant::now();

        if now.duration_since(self.window_start) > self.window_duration {
            // New window
            self.window_start = now;
            self.count = 1;
            Ok(())
        } else if self.count < self.max_per_window {
            self.count += 1;
            Ok(())
        } else {
            let wait = self.window_duration - now.duration_since(self.window_start);
            Err(wait)
        }
    }
}

/// Per-agent rate limiter that tracks an independent sliding window for each agent key.
///
/// Keyed by agent name string to enable checking before the agent ID is resolved.
/// Prevents multi-connection bypass: a single agent opening N connections still
/// gets at most `max_per_second` total requests across all those connections.
pub struct PerAgentRateLimiter {
    limiters: HashMap<String, RateLimiter>,
    max_per_second: u32,
}

impl PerAgentRateLimiter {
    /// Create a new per-agent rate limiter. Every agent shares the same `max_per_second` limit.
    /// A `max_per_second` of 0 means unlimited.
    pub fn new(max_per_second: u32) -> Self {
        Self {
            limiters: HashMap::new(),
            max_per_second,
        }
    }

    /// Check whether the agent identified by `key` is within its rate limit.
    ///
    /// Returns `Ok(())` if the request is allowed, or `Err(wait_duration)` if the agent
    /// has exceeded its window limit. Returns `Ok(())` immediately if `max_per_second == 0`.
    pub fn check(&mut self, key: &str) -> Result<(), Duration> {
        if self.max_per_second == 0 {
            return Ok(());
        }
        let max = self.max_per_second;
        self.limiters
            .entry(key.to_string())
            .or_insert_with(|| RateLimiter::new(max))
            .check()
    }

    /// Remove the rate-limit state for an agent (e.g., on disconnect).
    pub fn remove(&mut self, key: &str) {
        self.limiters.remove(key);
    }

    /// Number of agents currently being tracked.
    pub fn tracked_count(&self) -> usize {
        self.limiters.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── PerAgentRateLimiter tests ─────────────────────────────────────────────

    #[test]
    fn per_agent_zero_limit_is_unlimited() {
        let mut rl = PerAgentRateLimiter::new(0);
        for _ in 0..1000 {
            assert!(rl.check("agent-a").is_ok(), "0 = unlimited, should always allow");
        }
    }

    #[test]
    fn per_agent_independent_windows_per_key() {
        let mut rl = PerAgentRateLimiter::new(2);
        // agent-a can send 2
        assert!(rl.check("agent-a").is_ok());
        assert!(rl.check("agent-a").is_ok());
        assert!(rl.check("agent-a").is_err(), "agent-a should be rate limited");

        // agent-b has its own independent window — not affected by agent-a
        assert!(rl.check("agent-b").is_ok(), "agent-b has its own independent window");
        assert!(rl.check("agent-b").is_ok());
        assert!(rl.check("agent-b").is_err());
    }

    #[test]
    fn per_agent_remove_clears_state() {
        let mut rl = PerAgentRateLimiter::new(1);
        assert!(rl.check("agent-x").is_ok());
        assert!(rl.check("agent-x").is_err()); // rate limited
        assert_eq!(rl.tracked_count(), 1);

        rl.remove("agent-x");
        assert_eq!(rl.tracked_count(), 0);
        // After removal, agent-x gets a fresh window
        assert!(rl.check("agent-x").is_ok());
    }

    #[test]
    fn per_agent_tracked_count_is_accurate() {
        let mut rl = PerAgentRateLimiter::new(100);
        assert_eq!(rl.tracked_count(), 0);
        rl.check("a").unwrap();
        assert_eq!(rl.tracked_count(), 1);
        rl.check("b").unwrap();
        assert_eq!(rl.tracked_count(), 2);
        rl.remove("a");
        assert_eq!(rl.tracked_count(), 1);
    }

    // ─── RateLimiter tests ─────────────────────────────────────────────────────

    #[test]
    fn test_allows_within_limit() {
        let mut rl = RateLimiter::new(5);
        for _ in 0..5 {
            assert!(rl.check().is_ok());
        }
    }

    #[test]
    fn test_rejects_over_limit() {
        let mut rl = RateLimiter::new(3);
        assert!(rl.check().is_ok());
        assert!(rl.check().is_ok());
        assert!(rl.check().is_ok());
        assert!(rl.check().is_err());
    }

    #[test]
    fn test_resets_after_window() {
        let mut rl = RateLimiter::new(2);
        assert!(rl.check().is_ok());
        assert!(rl.check().is_ok());
        assert!(rl.check().is_err());

        // Simulate window expiry
        rl.window_start = Instant::now() - Duration::from_secs(2);
        assert!(rl.check().is_ok());
    }
}
