use std::collections::VecDeque;
use std::time::Instant;

/// A sliding window rate limiter that tracks calls over the last minute.
#[derive(Debug, Clone)]
pub struct SlidingWindowRateLimiter {
    /// Max calls allowed in a 60-second window.
    max_calls: u32,
    /// Timestamps of recent calls (kept only if within the window).
    call_times: VecDeque<Instant>,
}

impl SlidingWindowRateLimiter {
    /// Create a new rate limiter with the given max calls per minute.
    pub fn new(max_calls_per_minute: u32) -> Self {
        Self {
            max_calls: max_calls_per_minute,
            call_times: VecDeque::new(),
        }
    }

    /// Check if a call is allowed under the rate limit.
    /// If allowed, records the call time. Returns true if allowed, false if rate limit exceeded.
    /// A `max_calls` of 0 means unlimited (always allowed).
    pub fn check_and_record(&mut self) -> bool {
        // 0 = unlimited
        if self.max_calls == 0 {
            return true;
        }

        let now = Instant::now();
        let window_start = now - std::time::Duration::from_secs(60);

        // Remove calls outside the window.
        while let Some(&oldest) = self.call_times.front() {
            if oldest < window_start {
                self.call_times.pop_front();
            } else {
                break;
            }
        }

        if self.call_times.len() < self.max_calls as usize {
            self.call_times.push_back(now);
            true
        } else {
            false
        }
    }

    /// Get the current call count within the window.
    pub fn current_count(&self) -> u32 {
        self.call_times.len() as u32
    }

    /// Get the max allowed calls per minute.
    pub fn max_calls_per_minute(&self) -> u32 {
        self.max_calls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_limiter_is_empty() {
        let limiter = SlidingWindowRateLimiter::new(10);
        assert_eq!(limiter.current_count(), 0);
    }

    #[test]
    fn check_and_record_allows_calls_under_limit() {
        let mut limiter = SlidingWindowRateLimiter::new(3);
        assert!(limiter.check_and_record());
        assert!(limiter.check_and_record());
        assert!(limiter.check_and_record());
        assert_eq!(limiter.current_count(), 3);
    }

    #[test]
    fn check_and_record_blocks_over_limit() {
        let mut limiter = SlidingWindowRateLimiter::new(2);
        assert!(limiter.check_and_record());
        assert!(limiter.check_and_record());
        assert!(!limiter.check_and_record());
        assert_eq!(limiter.current_count(), 2);
    }

    #[test]
    fn check_and_record_allows_after_window_expires() {
        let mut limiter = SlidingWindowRateLimiter::new(1);
        assert!(limiter.check_and_record());
        assert!(!limiter.check_and_record());
    }
}
