//! Rate limits shared by every API instance (Redis fixed windows), falling back to a
//! per-process limiter while Redis is unreachable so limits never silently disappear.

use std::time::Duration;

use aionix_server_common::ratelimit::{Decision, Limit, MemoryRateLimiter};
use redis::{Script, aio::ConnectionManager};
use tracing::warn;

use crate::redis_conn::Keys;

/// Named limits applied by the API.
pub mod limits {
    use super::Limit;
    /// Every request, per client IP.
    pub const GLOBAL_PER_IP: Limit = Limit::per_minute(600);
    pub const LOGIN_PER_IP: Limit = Limit::per_minute(20);
    pub const LOGIN_PER_ACCOUNT: Limit = Limit::per_minute(10);
    pub const REFRESH_PER_IP: Limit = Limit::per_minute(60);
    pub const CHALLENGE_PER_IP: Limit = Limit::per_minute(60);
    pub const DEVICE_LOGIN_PER_IP: Limit = Limit::per_minute(60);
    pub const REGISTER_PER_IP: Limit = Limit::per_hour(30);
    pub const QUICK_SUPPORT_REGISTER_PER_IP: Limit = Limit::per_hour(10);
    pub const SESSION_REQUEST_PER_USER: Limit = Limit::per_minute(30);
    pub const LOOKUP_PER_USER: Limit = Limit::per_minute(60);
}

const INCR: &str = r#"
local c = redis.call('INCR', KEYS[1])
if c == 1 then redis.call('PEXPIRE', KEYS[1], ARGV[1]) end
local ttl = redis.call('PTTL', KEYS[1])
if ttl < 0 then redis.call('PEXPIRE', KEYS[1], ARGV[1]); ttl = tonumber(ARGV[1]) end
return {c, ttl}
"#;

pub struct RateLimiter {
    redis: Option<(ConnectionManager, Keys)>,
    script: Script,
    fallback: MemoryRateLimiter,
}

impl RateLimiter {
    pub fn redis(conn: ConnectionManager, keys: Keys) -> Self {
        Self {
            redis: Some((conn, keys)),
            script: Script::new(INCR),
            fallback: MemoryRateLimiter::new(),
        }
    }

    pub fn memory() -> Self {
        Self {
            redis: None,
            script: Script::new(INCR),
            fallback: MemoryRateLimiter::new(),
        }
    }

    pub async fn check(&self, bucket: &str, key: &str, limit: Limit) -> Decision {
        let local_key = format!("{bucket}:{key}");
        let Some((conn, keys)) = &self.redis else {
            return self.fallback.check(&local_key, limit);
        };
        let mut c = conn.clone();
        let window_ms = limit.window.as_millis() as u64;
        let res: Result<(i64, i64), _> = self
            .script
            .key(keys.rate_limit(bucket, key))
            .arg(window_ms)
            .invoke_async(&mut c)
            .await;
        match res {
            Ok((count, ttl_ms)) if count > i64::from(limit.max) => Decision::Limited {
                retry_after: Duration::from_millis(ttl_ms.max(0) as u64).as_secs().max(1),
            },
            Ok(_) => Decision::Allowed,
            Err(e) => {
                warn!(error = %e, bucket, "rate limiter falling back to in-process counters");
                self.fallback.check(&local_key, limit)
            }
        }
    }
}
