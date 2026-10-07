//! Cross-service messaging between `aionix-api` and `aionix-signaling` (Redis pub/sub).
//!
//! * `<prefix>:dev:<device>` — messages for a device's signaling link (session offers,
//!   session revocations, device revocation, "close older links"). Every signaling
//!   instance pattern-subscribes and forwards to the links it holds.
//! * `<prefix>:offer:<session>` — the host's answer to a session offer, consumed by the
//!   API request waiting for it.
//!
//! Pub/sub is fire-and-forget: nothing here is durable, and nothing needs to be — a lost
//! offer surfaces as a timeout to the technician, and presence is refreshed by
//! heartbeats. Listener tasks reconnect on their own after a Redis outage.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use aionix_control_api::signal::{EndpointAddrDto, OfferRejectReason, ServerMessage};
use async_trait::async_trait;
use futures::StreamExt;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    presence::{StoreError, StoreResult},
    redis_conn::Keys,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DeviceBusMessage {
    /// Forward this message on the device's link.
    Deliver { message: ServerMessage },
    /// Close every link of the device except `keep` (a newer link took over).
    Supersede { keep: Uuid },
    /// The identity was revoked: send `DeviceRevoked` and close.
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferAnswer {
    pub accept: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<OfferRejectReason>,
    #[serde(default)]
    pub addr: EndpointAddrDto,
}

type WaiterMap = Arc<Mutex<HashMap<Uuid, oneshot::Sender<OfferAnswer>>>>;

/// Waits for one session's offer answer; unregisters itself when dropped.
pub struct OfferWaiter {
    session: Uuid,
    rx: Option<oneshot::Receiver<OfferAnswer>>,
    map: WaiterMap,
}

impl OfferWaiter {
    /// `None` on timeout.
    pub async fn wait(mut self, timeout: Duration) -> Option<OfferAnswer> {
        let rx = self.rx.take()?;
        tokio::time::timeout(timeout, rx).await.ok()?.ok()
    }
}

impl Drop for OfferWaiter {
    fn drop(&mut self) {
        self.map.lock().unwrap().remove(&self.session);
    }
}

#[derive(Default, Clone)]
struct Waiters(WaiterMap);

impl Waiters {
    fn register(&self, session: Uuid) -> OfferWaiter {
        let (tx, rx) = oneshot::channel();
        self.0.lock().unwrap().insert(session, tx);
        OfferWaiter {
            session,
            rx: Some(rx),
            map: self.0.clone(),
        }
    }

    fn complete(&self, session: Uuid, answer: OfferAnswer) {
        if let Some(tx) = self.0.lock().unwrap().remove(&session) {
            let _ = tx.send(answer);
        }
    }
}

#[async_trait]
pub trait SignalBus: Send + Sync {
    async fn publish_device(&self, device: Uuid, msg: &DeviceBusMessage) -> StoreResult<()>;

    /// Every device message published on the bus (signaling instances).
    async fn device_messages(&self) -> StoreResult<mpsc::Receiver<(Uuid, DeviceBusMessage)>>;

    async fn publish_offer_answer(&self, session: Uuid, answer: &OfferAnswer) -> StoreResult<()>;

    /// Registers interest in `session`'s answer. Call before publishing the offer.
    fn await_offer_answer(&self, session: Uuid) -> OfferWaiter;

    /// Whether the background subscriptions are currently active (readiness).
    fn listening(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Redis
// ---------------------------------------------------------------------------

pub struct RedisBus {
    client: redis::Client,
    conn: ConnectionManager,
    keys: Keys,
    waiters: Waiters,
    offer_listener: Arc<AtomicBool>,
    device_listener: Arc<AtomicBool>,
    wants_offers: AtomicBool,
}

impl RedisBus {
    pub fn new(client: redis::Client, conn: ConnectionManager, keys: Keys) -> Self {
        Self {
            client,
            conn,
            keys,
            waiters: Waiters::default(),
            offer_listener: Arc::new(AtomicBool::new(false)),
            device_listener: Arc::new(AtomicBool::new(false)),
            wants_offers: AtomicBool::new(false),
        }
    }

    /// Starts the offer-answer subscription (API processes).
    pub fn start_offer_listener(&self) {
        self.wants_offers.store(true, Ordering::SeqCst);
        let client = self.client.clone();
        let pattern = self.keys.offer_channel_pattern();
        let waiters = self.waiters.clone();
        let flag = self.offer_listener.clone();
        tokio::spawn(subscribe_loop(
            client,
            pattern,
            flag,
            move |channel, payload| {
                if let (Some(id), Ok(answer)) = (
                    Keys::id_from_channel(channel),
                    serde_json::from_str::<OfferAnswer>(payload),
                ) {
                    waiters.complete(id, answer);
                }
                true
            },
        ));
    }

    async fn publish(&self, channel: String, payload: String) -> StoreResult<()> {
        let mut c = self.conn.clone();
        let _: i64 = redis::cmd("PUBLISH")
            .arg(channel)
            .arg(payload)
            .query_async(&mut c)
            .await?;
        Ok(())
    }
}

/// Keeps a pattern subscription alive across Redis restarts. `on_message` returns
/// `false` to stop.
async fn subscribe_loop(
    client: redis::Client,
    pattern: String,
    ready: Arc<AtomicBool>,
    on_message: impl Fn(&str, &str) -> bool + Send + 'static,
) {
    let mut delay = Duration::from_millis(250);
    loop {
        match client.get_async_pubsub().await {
            Ok(mut pubsub) => match pubsub.psubscribe(&pattern).await {
                Ok(()) => {
                    info!(pattern = %pattern, "bus subscription active");
                    ready.store(true, Ordering::SeqCst);
                    delay = Duration::from_millis(250);
                    let mut stream = pubsub.on_message();
                    while let Some(msg) = stream.next().await {
                        let payload: String = match msg.get_payload() {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        if !on_message(msg.get_channel_name(), &payload) {
                            ready.store(false, Ordering::SeqCst);
                            return;
                        }
                    }
                    warn!(pattern = %pattern, "bus subscription lost; reconnecting");
                }
                Err(e) => warn!(error = %e, "bus psubscribe failed"),
            },
            Err(e) => debug!(error = %e, "bus connection failed"),
        }
        ready.store(false, Ordering::SeqCst);
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

#[async_trait]
impl SignalBus for RedisBus {
    async fn publish_device(&self, device: Uuid, msg: &DeviceBusMessage) -> StoreResult<()> {
        let payload = serde_json::to_string(msg).map_err(|e| StoreError(e.to_string()))?;
        self.publish(self.keys.device_channel(device), payload)
            .await
    }

    async fn device_messages(&self) -> StoreResult<mpsc::Receiver<(Uuid, DeviceBusMessage)>> {
        let (tx, rx) = mpsc::channel(1024);
        let flag = self.device_listener.clone();
        tokio::spawn(subscribe_loop(
            self.client.clone(),
            self.keys.device_channel_pattern(),
            flag,
            move |channel, payload| {
                if let (Some(id), Ok(m)) = (
                    Keys::id_from_channel(channel),
                    serde_json::from_str::<DeviceBusMessage>(payload),
                ) && tx.try_send((id, m)).is_err()
                {
                    if tx.is_closed() {
                        return false;
                    }
                    warn!("device message queue full; message dropped");
                }
                true
            },
        ));
        Ok(rx)
    }

    async fn publish_offer_answer(&self, session: Uuid, answer: &OfferAnswer) -> StoreResult<()> {
        let payload = serde_json::to_string(answer).map_err(|e| StoreError(e.to_string()))?;
        self.publish(self.keys.offer_channel(session), payload)
            .await
    }

    fn await_offer_answer(&self, session: Uuid) -> OfferWaiter {
        self.waiters.register(session)
    }

    fn listening(&self) -> bool {
        !self.wants_offers.load(Ordering::SeqCst) || self.offer_listener.load(Ordering::SeqCst)
    }
}

impl RedisBus {
    /// Readiness for signaling instances.
    pub fn device_listener_active(&self) -> bool {
        self.device_listener.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// In-memory (single process; tests)
// ---------------------------------------------------------------------------

pub struct MemoryBus {
    devices: broadcast::Sender<(Uuid, DeviceBusMessage)>,
    waiters: Waiters,
}

impl Default for MemoryBus {
    fn default() -> Self {
        Self {
            devices: broadcast::channel(1024).0,
            waiters: Waiters::default(),
        }
    }
}

impl MemoryBus {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SignalBus for MemoryBus {
    async fn publish_device(&self, device: Uuid, msg: &DeviceBusMessage) -> StoreResult<()> {
        let _ = self.devices.send((device, msg.clone()));
        Ok(())
    }

    async fn device_messages(&self) -> StoreResult<mpsc::Receiver<(Uuid, DeviceBusMessage)>> {
        let mut sub = self.devices.subscribe();
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            while let Ok(m) = sub.recv().await {
                if tx.send(m).await.is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }

    async fn publish_offer_answer(&self, session: Uuid, answer: &OfferAnswer) -> StoreResult<()> {
        self.waiters.complete(session, answer.clone());
        Ok(())
    }

    fn await_offer_answer(&self, session: Uuid) -> OfferWaiter {
        self.waiters.register(session)
    }

    fn listening(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_bus_delivers_device_messages_and_offer_answers() {
        let bus = MemoryBus::new();
        let mut rx = bus.device_messages().await.unwrap();
        let dev = Uuid::new_v4();
        bus.publish_device(dev, &DeviceBusMessage::Revoked)
            .await
            .unwrap();
        assert_eq!(rx.recv().await.unwrap(), (dev, DeviceBusMessage::Revoked));

        let sid = Uuid::new_v4();
        let waiter = bus.await_offer_answer(sid);
        let answer = OfferAnswer {
            accept: true,
            reason: None,
            addr: EndpointAddrDto::default(),
        };
        bus.publish_offer_answer(sid, &answer).await.unwrap();
        assert_eq!(waiter.wait(Duration::from_secs(1)).await, Some(answer));

        // Unanswered offers time out and unregister.
        let w = bus.await_offer_answer(Uuid::new_v4());
        assert_eq!(w.wait(Duration::from_millis(20)).await, None);
        assert!(bus.waiters.0.lock().unwrap().is_empty());
    }

    #[test]
    fn bus_message_json_is_stable() {
        let m = DeviceBusMessage::Supersede { keep: Uuid::nil() };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(
            s,
            r#"{"t":"supersede","keep":"00000000-0000-0000-0000-000000000000"}"#
        );
        let d = DeviceBusMessage::Deliver {
            message: ServerMessage::DeviceRevoked,
        };
        let back: DeviceBusMessage =
            serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }
}
