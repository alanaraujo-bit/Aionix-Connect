//! Device presence (ADR-0010).
//!
//! The signaling service is the only writer. Each authenticated device link owns the
//! device's presence record through its connection id:
//!
//! * link authenticated → [`PresenceStore::claim`] with state `Connecting`
//!   (a newer link always takes over; the caller tells the old one to close);
//! * `Presence` message received → `refresh` with state `Online`;
//! * every message/heartbeat → `refresh` (extends the TTL; refused once superseded);
//! * orderly `Goodbye` → `release(None)` → offline at once;
//! * link lost unexpectedly / server shutting down → `release(Some(grace))` →
//!   `Connecting` until the grace period ends, then offline;
//! * signaling process killed → nobody refreshes → the record expires after the
//!   heartbeat timeout → offline.
//!
//! Records live in Redis with a TTL, so presence can never outlive the evidence for
//! it. After a Redis restart the next heartbeat of each live link recreates its record.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use aionix_control_api::{rest::PresenceState, signal::EndpointAddrDto};
use async_trait::async_trait;
use redis::{Script, aio::ConnectionManager};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::redis_conn::Keys;

#[derive(Debug, Clone, thiserror::Error)]
#[error("ephemeral store unavailable: {0}")]
pub struct StoreError(pub String);

impl From<redis::RedisError> for StoreError {
    fn from(e: redis::RedisError) -> Self {
        Self(e.to_string())
    }
}

impl From<StoreError> for crate::ApiFailure {
    fn from(e: StoreError) -> Self {
        tracing::error!(error = %e, "ephemeral store error");
        crate::ApiFailure::unavailable("presence store unavailable")
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceRecord {
    pub conn_id: Uuid,
    /// `Online` or `Connecting` (absence of a record means `Offline`).
    pub state: PresenceState,
    #[serde(default)]
    pub addr: EndpointAddrDto,
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub unattended_enabled: bool,
    /// Unix seconds when the link was authenticated.
    pub since: i64,
    /// Signaling instance holding the link (diagnostics).
    #[serde(default)]
    pub instance: String,
}

impl PresenceRecord {
    pub fn connecting(conn_id: Uuid, instance: &str) -> Self {
        Self {
            conn_id,
            state: PresenceState::Connecting,
            addr: EndpointAddrDto::default(),
            busy: false,
            unattended_enabled: false,
            since: time::OffsetDateTime::now_utc().unix_timestamp(),
            instance: instance.to_owned(),
        }
    }
}

/// Presence of a device as reported to technicians.
pub fn state_of(record: Option<&PresenceRecord>) -> PresenceState {
    record.map(|r| r.state).unwrap_or(PresenceState::Offline)
}

#[async_trait]
pub trait PresenceStore: Send + Sync {
    /// A new authenticated link takes ownership. Returns the connection id it replaced.
    async fn claim(
        &self,
        device: Uuid,
        record: &PresenceRecord,
        ttl: Duration,
    ) -> StoreResult<Option<Uuid>>;

    /// Updates/extends the record while `record.conn_id` still owns it (or recreates it
    /// when it vanished, e.g. after a Redis restart). `false` = superseded.
    async fn refresh(&self, device: Uuid, record: &PresenceRecord, ttl: Duration)
    -> StoreResult<bool>;

    /// Gives up ownership. `lost = Some((record, grace))` keeps the device
    /// `Connecting` for `grace`; `None` makes it offline immediately.
    async fn release(
        &self,
        device: Uuid,
        conn_id: Uuid,
        lost: Option<(PresenceRecord, Duration)>,
    ) -> StoreResult<()>;

    async fn get(&self, device: Uuid) -> StoreResult<Option<PresenceRecord>>;

    async fn get_many(&self, devices: &[Uuid]) -> StoreResult<HashMap<Uuid, PresenceRecord>>;

    /// Devices currently `Online`.
    async fn online_count(&self) -> StoreResult<u64>;
}

// ---------------------------------------------------------------------------
// Redis
// ---------------------------------------------------------------------------

const CLAIM: &str = r#"
local prev = redis.call('GET', KEYS[1])
local prev_conn = false
if prev then
  local ok, d = pcall(cjson.decode, prev)
  if ok and type(d) == 'table' and d.conn_id ~= ARGV[6] then prev_conn = d.conn_id end
end
redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
if ARGV[5] == 'online' then
  redis.call('ZADD', KEYS[2], ARGV[4], ARGV[3])
else
  redis.call('ZREM', KEYS[2], ARGV[3])
end
return prev_conn
"#;

const REFRESH: &str = r#"
local cur = redis.call('GET', KEYS[1])
if cur then
  local ok, d = pcall(cjson.decode, cur)
  if ok and type(d) == 'table' and d.conn_id ~= ARGV[6] then return 0 end
end
redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
if ARGV[5] == 'online' then
  redis.call('ZADD', KEYS[2], ARGV[4], ARGV[3])
else
  redis.call('ZREM', KEYS[2], ARGV[3])
end
return 1
"#;

const RELEASE: &str = r#"
local cur = redis.call('GET', KEYS[1])
if not cur then
  redis.call('ZREM', KEYS[2], ARGV[2])
  return 0
end
local ok, d = pcall(cjson.decode, cur)
if ok and type(d) == 'table' and d.conn_id ~= ARGV[1] then return 0 end
redis.call('ZREM', KEYS[2], ARGV[2])
if tonumber(ARGV[3]) > 0 then
  redis.call('SET', KEYS[1], ARGV[4], 'PX', ARGV[3])
else
  redis.call('DEL', KEYS[1])
end
return 1
"#;

pub struct RedisPresence {
    conn: ConnectionManager,
    keys: Keys,
    claim: Script,
    refresh: Script,
    release: Script,
}

impl RedisPresence {
    pub fn new(conn: ConnectionManager, keys: Keys) -> Self {
        Self {
            conn,
            keys,
            claim: Script::new(CLAIM),
            refresh: Script::new(REFRESH),
            release: Script::new(RELEASE),
        }
    }

    async fn write(
        &self,
        script: &Script,
        device: Uuid,
        record: &PresenceRecord,
        ttl: Duration,
    ) -> StoreResult<redis::Value> {
        let json = serde_json::to_string(record).map_err(|e| StoreError(e.to_string()))?;
        let ttl_ms = ttl.as_millis().max(1) as u64;
        let deadline = now_ms() + ttl_ms;
        let mut c = self.conn.clone();
        Ok(script
            .key(self.keys.presence(device))
            .key(self.keys.presence_online())
            .arg(json)
            .arg(ttl_ms)
            .arg(device.to_string())
            .arg(deadline)
            .arg(record.state.as_str())
            .arg(record.conn_id.to_string())
            .invoke_async(&mut c)
            .await?)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[async_trait]
impl PresenceStore for RedisPresence {
    async fn claim(
        &self,
        device: Uuid,
        record: &PresenceRecord,
        ttl: Duration,
    ) -> StoreResult<Option<Uuid>> {
        let v = self.write(&self.claim, device, record, ttl).await?;
        Ok(match v {
            redis::Value::BulkString(b) => String::from_utf8_lossy(&b).parse().ok(),
            redis::Value::SimpleString(s) => s.parse().ok(),
            _ => None,
        })
    }

    async fn refresh(
        &self,
        device: Uuid,
        record: &PresenceRecord,
        ttl: Duration,
    ) -> StoreResult<bool> {
        let v = self.write(&self.refresh, device, record, ttl).await?;
        Ok(matches!(v, redis::Value::Int(1)))
    }

    async fn release(
        &self,
        device: Uuid,
        conn_id: Uuid,
        lost: Option<(PresenceRecord, Duration)>,
    ) -> StoreResult<()> {
        let (json, grace_ms) = match lost {
            Some((mut r, grace)) => {
                r.state = PresenceState::Connecting;
                (
                    serde_json::to_string(&r).map_err(|e| StoreError(e.to_string()))?,
                    grace.as_millis().max(1) as u64,
                )
            }
            None => (String::new(), 0),
        };
        let mut c = self.conn.clone();
        let _: i64 = self
            .release
            .key(self.keys.presence(device))
            .key(self.keys.presence_online())
            .arg(conn_id.to_string())
            .arg(device.to_string())
            .arg(grace_ms)
            .arg(json)
            .invoke_async(&mut c)
            .await?;
        Ok(())
    }

    async fn get(&self, device: Uuid) -> StoreResult<Option<PresenceRecord>> {
        let mut c = self.conn.clone();
        let v: Option<String> = redis::cmd("GET")
            .arg(self.keys.presence(device))
            .query_async(&mut c)
            .await?;
        Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
    }

    async fn get_many(&self, devices: &[Uuid]) -> StoreResult<HashMap<Uuid, PresenceRecord>> {
        if devices.is_empty() {
            return Ok(HashMap::new());
        }
        let mut c = self.conn.clone();
        let mut cmd = redis::cmd("MGET");
        for d in devices {
            cmd.arg(self.keys.presence(*d));
        }
        let values: Vec<Option<String>> = cmd.query_async(&mut c).await?;
        Ok(devices
            .iter()
            .zip(values)
            .filter_map(|(d, v)| {
                v.and_then(|s| serde_json::from_str(&s).ok())
                    .map(|r| (*d, r))
            })
            .collect())
    }

    async fn online_count(&self) -> StoreResult<u64> {
        let mut c = self.conn.clone();
        let key = self.keys.presence_online();
        let _: i64 = redis::cmd("ZREMRANGEBYSCORE")
            .arg(&key)
            .arg("-inf")
            .arg(now_ms())
            .query_async(&mut c)
            .await?;
        Ok(redis::cmd("ZCARD").arg(&key).query_async(&mut c).await?)
    }
}

// ---------------------------------------------------------------------------
// In-memory (single process; tests). Uses tokio's clock so tests can pause time.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct MemoryPresence {
    records: Mutex<HashMap<Uuid, (PresenceRecord, tokio::time::Instant)>>,
}

impl MemoryPresence {
    pub fn new() -> Self {
        Self::default()
    }

    fn live(&self, device: Uuid) -> Option<PresenceRecord> {
        let mut map = self.records.lock().unwrap();
        match map.get(&device) {
            Some((r, until)) if *until > tokio::time::Instant::now() => Some(r.clone()),
            Some(_) => {
                map.remove(&device);
                None
            }
            None => None,
        }
    }
}

#[async_trait]
impl PresenceStore for MemoryPresence {
    async fn claim(
        &self,
        device: Uuid,
        record: &PresenceRecord,
        ttl: Duration,
    ) -> StoreResult<Option<Uuid>> {
        let prev = self.live(device).map(|r| r.conn_id);
        self.records.lock().unwrap().insert(
            device,
            (record.clone(), tokio::time::Instant::now() + ttl),
        );
        Ok(prev.filter(|p| *p != record.conn_id))
    }

    async fn refresh(
        &self,
        device: Uuid,
        record: &PresenceRecord,
        ttl: Duration,
    ) -> StoreResult<bool> {
        if let Some(cur) = self.live(device)
            && cur.conn_id != record.conn_id
        {
            return Ok(false);
        }
        self.records.lock().unwrap().insert(
            device,
            (record.clone(), tokio::time::Instant::now() + ttl),
        );
        Ok(true)
    }

    async fn release(
        &self,
        device: Uuid,
        conn_id: Uuid,
        lost: Option<(PresenceRecord, Duration)>,
    ) -> StoreResult<()> {
        match self.live(device) {
            Some(cur) if cur.conn_id == conn_id => {}
            _ => return Ok(()),
        }
        let mut map = self.records.lock().unwrap();
        match lost {
            Some((mut r, grace)) => {
                r.state = PresenceState::Connecting;
                map.insert(device, (r, tokio::time::Instant::now() + grace));
            }
            None => {
                map.remove(&device);
            }
        }
        Ok(())
    }

    async fn get(&self, device: Uuid) -> StoreResult<Option<PresenceRecord>> {
        Ok(self.live(device))
    }

    async fn get_many(&self, devices: &[Uuid]) -> StoreResult<HashMap<Uuid, PresenceRecord>> {
        Ok(devices
            .iter()
            .filter_map(|d| self.live(*d).map(|r| (*d, r)))
            .collect())
    }

    async fn online_count(&self) -> StoreResult<u64> {
        let now = tokio::time::Instant::now();
        Ok(self
            .records
            .lock()
            .unwrap()
            .values()
            .filter(|(r, until)| *until > now && r.state == PresenceState::Online)
            .count() as u64)
    }
}

/// Behaviour shared by every implementation; the Redis integration tests run the same
/// scenarios against a real server.
pub mod scenarios {
    use super::*;

    fn online(conn: Uuid) -> PresenceRecord {
        PresenceRecord {
            state: PresenceState::Online,
            addr: EndpointAddrDto {
                relay_urls: vec!["https://relay.example.test/".into()],
                direct_addrs: vec![],
            },
            ..PresenceRecord::connecting(conn, "test")
        }
    }

    /// Connecting → online → heartbeat timeout → offline.
    pub async fn timeout(store: &dyn PresenceStore, ttl: Duration, wait: impl AsyncFn(Duration)) {
        let dev = Uuid::new_v4();
        let conn = Uuid::new_v4();
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Offline);
        store
            .claim(dev, &PresenceRecord::connecting(conn, "test"), ttl)
            .await
            .unwrap();
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Connecting);
        assert!(store.refresh(dev, &online(conn), ttl).await.unwrap());
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Online);
        assert!(store.online_count().await.unwrap() >= 1);
        // No heartbeat: the evidence expires.
        wait(ttl + Duration::from_millis(300)).await;
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Offline);
    }

    /// Unexpected loss → connecting for the grace period → reconnect → online; the old
    /// link can no longer touch the record; goodbye → offline at once.
    pub async fn reconnect(store: &dyn PresenceStore, ttl: Duration, wait: impl AsyncFn(Duration)) {
        let dev = Uuid::new_v4();
        let first = Uuid::new_v4();
        store.claim(dev, &online(first), ttl).await.unwrap();
        store.refresh(dev, &online(first), ttl).await.unwrap();
        store
            .release(dev, first, Some((online(first), ttl)))
            .await
            .unwrap();
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Connecting);

        let second = Uuid::new_v4();
        store
            .claim(dev, &PresenceRecord::connecting(second, "test"), ttl)
            .await
            .unwrap();
        assert!(store.refresh(dev, &online(second), ttl).await.unwrap());
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Online);

        // A late heartbeat / release of the superseded link is refused / ignored.
        assert!(!store.refresh(dev, &online(first), ttl).await.unwrap());
        store.release(dev, first, None).await.unwrap();
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Online);

        // A third link takes over and reports who it replaced.
        let third = Uuid::new_v4();
        let replaced = store
            .claim(dev, &PresenceRecord::connecting(third, "test"), ttl)
            .await
            .unwrap();
        assert_eq!(replaced, Some(second));

        // Orderly goodbye: offline immediately.
        store.release(dev, third, None).await.unwrap();
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Offline);

        // Loss with grace, then nobody comes back: offline after the grace period.
        let fourth = Uuid::new_v4();
        store.claim(dev, &online(fourth), ttl).await.unwrap();
        store
            .release(dev, fourth, Some((online(fourth), ttl)))
            .await
            .unwrap();
        wait(ttl + Duration::from_millis(300)).await;
        assert_eq!(state_of(store.get(dev).await.unwrap().as_ref()), PresenceState::Offline);
        let many = store.get_many(&[dev, Uuid::new_v4()]).await.unwrap();
        assert!(many.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn advance(d: Duration) {
        tokio::time::advance(d).await;
    }

    #[tokio::test(start_paused = true)]
    async fn memory_presence_times_out() {
        scenarios::timeout(&MemoryPresence::new(), Duration::from_secs(45), advance).await;
    }

    #[tokio::test(start_paused = true)]
    async fn memory_presence_reconnects() {
        scenarios::reconnect(&MemoryPresence::new(), Duration::from_secs(30), advance).await;
    }
}
