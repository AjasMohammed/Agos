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

#[cfg(test)]
mod tests {
    use super::*;

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
