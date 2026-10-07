//! In-process fixed-window rate limiter.
//!
//! Used where limits only need to hold per process (relay connection attempts,
//! signaling messages per connection) and as the fallback of the Redis-backed limiter
//! in the Control Plane.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub max: u32,
    pub window: Duration,
}

impl Limit {
    pub const fn per_minute(max: u32) -> Self {
        Self {
            max,
            window: Duration::from_secs(60),
        }
    }

    pub const fn per_hour(max: u32) -> Self {
        Self {
            max,
            window: Duration::from_secs(3600),
        }
    }

    pub const fn per_second(max: u32) -> Self {
        Self {
            max,
            window: Duration::from_secs(1),
        }
    }
}

/// Outcome of a rate limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Refused; retry after this many seconds.
    Limited {
        retry_after: u64,
    },
}

impl Decision {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

#[derive(Debug, Default)]
pub struct MemoryRateLimiter {
    buckets: Mutex<HashMap<String, (Instant, u32)>>,
}

impl MemoryRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&self, key: &str, limit: Limit) -> Decision {
        self.check_at(key, limit, Instant::now())
    }

    pub fn check_at(&self, key: &str, limit: Limit, now: Instant) -> Decision {
        let mut buckets = self.buckets.lock().unwrap();
        if buckets.len() > 100_000 {
            buckets.retain(|_, (start, _)| now.duration_since(*start) < limit.window);
        }
        let entry = buckets.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= limit.window {
            *entry = (now, 0);
        }
        if entry.1 >= limit.max {
            let left = limit.window.saturating_sub(now.duration_since(entry.0));
            return Decision::Limited {
                retry_after: left.as_secs().max(1),
            };
        }
        entry.1 += 1;
        Decision::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_window() {
        let rl = MemoryRateLimiter::new();
        let limit = Limit {
            max: 2,
            window: Duration::from_secs(10),
        };
        let t0 = Instant::now();
        assert!(rl.check_at("a", limit, t0).is_allowed());
        assert!(rl.check_at("a", limit, t0).is_allowed());
        assert!(matches!(
            rl.check_at("a", limit, t0),
            Decision::Limited { retry_after: 10 }
        ));
        assert!(rl.check_at("b", limit, t0).is_allowed());
        assert!(
            rl.check_at("a", limit, t0 + Duration::from_secs(11))
                .is_allowed()
        );
    }
}
