//! Redis connection and key naming.
//!
//! Every key and channel starts with a configurable prefix (`AIONIX_REDIS_PREFIX`,
//! default `aionix`) so several environments — and parallel integration tests — can
//! share one Redis without interfering.

use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use tracing::warn;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Keys {
    prefix: String,
}

impl Keys {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn presence(&self, device: Uuid) -> String {
        format!("{}:presence:{device}", self.prefix)
    }

    /// Sorted set of online devices scored by heartbeat deadline (unix ms).
    pub fn presence_online(&self) -> String {
        format!("{}:presence:online", self.prefix)
    }

    pub fn device_channel(&self, device: Uuid) -> String {
        format!("{}:dev:{device}", self.prefix)
    }

    pub fn device_channel_pattern(&self) -> String {
        format!("{}:dev:*", self.prefix)
    }

    pub fn offer_channel(&self, session: Uuid) -> String {
        format!("{}:offer:{session}", self.prefix)
    }

    pub fn offer_channel_pattern(&self) -> String {
        format!("{}:offer:*", self.prefix)
    }

    /// Marker read by the relay to refuse revoked devices before their relay grant
    /// expires. Same layout as `aionix_relay::REVOKED_KEY_FORMAT`.
    pub fn revoked_device(&self, device: Uuid) -> String {
        format!("{}:revoked:{device}", self.prefix)
    }

    pub fn rate_limit(&self, bucket: &str, key: &str) -> String {
        format!("{}:rl:{bucket}:{key}", self.prefix)
    }

    /// Last part of a channel name (`…:dev:<uuid>` → uuid).
    pub fn id_from_channel(channel: &str) -> Option<Uuid> {
        channel.rsplit(':').next().and_then(|s| s.parse().ok())
    }
}

/// Connects a self-healing multiplexed connection (reconnects with backoff).
pub async fn connect(
    url: &str,
    attempts: u32,
) -> anyhow::Result<(redis::Client, ConnectionManager)> {
    let client = redis::Client::open(url)?;
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(Some(Duration::from_secs(3)))
        .set_response_timeout(Some(Duration::from_secs(3)))
        .set_number_of_retries(3)
        .set_max_delay(Duration::from_secs(2));
    let mut delay = Duration::from_millis(500);
    let mut last = None;
    for attempt in 1..=attempts.max(1) {
        match client
            .get_connection_manager_with_config(config.clone())
            .await
        {
            Ok(m) => return Ok((client, m)),
            Err(e) => {
                warn!(attempt, error = %e, "redis not reachable yet");
                last = Some(e);
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }
    }
    Err(anyhow::anyhow!(
        "cannot connect to Redis: {}",
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

pub async fn ping(conn: &ConnectionManager) -> Result<(), String> {
    let mut c = conn.clone();
    redis::cmd("PING")
        .query_async::<String>(&mut c)
        .await
        .map(|_| ())
        .map_err(|e| format!("redis unavailable: {e}"))
}
