//! Bounded reconnection.
//!
//! The technician drives resumption: after an unexpected transport loss it makes at most
//! `max_attempts` attempts, separated by exponential backoff with jitter, each bounded by
//! `attempt_timeout`. The host keeps the session for `resume_grace` and accepts exactly
//! one resume token (rotated on every successful resume). Either side gives up on its
//! own limit and moves the session to `Failed`, so no zombie session outlives the window.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub attempt_timeout: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(4),
            attempt_timeout: Duration::from_secs(8),
        }
    }
}

impl ReconnectPolicy {
    /// Nominal delay before attempt `attempt` (1-based), without jitter.
    pub fn base_delay(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(16);
        self.initial_delay
            .saturating_mul(1u32 << shift)
            .min(self.max_delay)
    }

    /// Delay with ±20 % jitter so many viewers do not reconnect in lockstep.
    pub fn delay(&self, attempt: u32) -> Duration {
        self.base_delay(attempt)
            .mul_f64(rand::random_range(0.8..1.2))
    }

    /// Upper bound of the whole reconnection episode.
    pub fn worst_case(&self) -> Duration {
        (1..=self.max_attempts)
            .map(|a| self.base_delay(a).mul_f64(1.2) + self.attempt_timeout)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_and_capped() {
        let p = ReconnectPolicy::default();
        assert_eq!(p.base_delay(1), Duration::from_millis(250));
        assert_eq!(p.base_delay(2), Duration::from_millis(500));
        assert_eq!(p.base_delay(4), Duration::from_secs(2));
        assert_eq!(p.base_delay(10), Duration::from_secs(4));
        assert_eq!(p.base_delay(u32::MAX), Duration::from_secs(4));
        for a in 1..=8 {
            let d = p.delay(a);
            assert!(d >= p.base_delay(a).mul_f64(0.8) && d <= p.base_delay(a).mul_f64(1.2));
        }
        assert!(p.worst_case() < Duration::from_secs(80));
    }
}
