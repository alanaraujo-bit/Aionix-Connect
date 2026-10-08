//! `aionix-signaling` — standalone signaling service (ADR-0010).
//!
//! Devices keep one WebSocket open to `GET /v1/signal` (bearer device token). The
//! link carries presence, session offers and their answers, host-side session events
//! and revocations — JSON control messages only, never screen content or input.
//!
//! ```text
//! connect ─▶ verify device token (Control Plane key) + device not revoked
//!         ─▶ claim presence (Connecting) ─▶ Ready
//! Presence ─▶ Online          every message ─▶ heartbeat (TTL refresh)
//! Ping every 15 s; silence > 45 s ─▶ close 4000 ─▶ Connecting (grace) ─▶ Offline
//! Goodbye ─▶ Offline at once   token expiry ─▶ close 4001 (log in, reconnect)
//! newer link ─▶ old one closed 4002   revoked ─▶ DeviceRevoked + close 4003
//! SIGTERM ─▶ close 1012 to every link ─▶ Connecting (grace) ─▶ clients reconnect
//! ```
//!
//! Several instances can run behind the reverse proxy: presence lives in Redis and
//! messages for a device reach whichever instance holds its link through the bus.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use aionix_control_api::{
    claims::DeviceClaims,
    rest::PresenceState,
    signal::{ClientMessage, ServerMessage, close_code},
};
use aionix_control_plane::{
    audit::{AuditRecord, Outcome, action},
    bus::{DeviceBusMessage, OfferAnswer, RedisBus, SignalBus},
    config::{store_config, trusted_proxies},
    db, devices,
    presence::{PresenceRecord, PresenceStore, RedisPresence},
    redis_conn::{self, Keys},
    server::store_checks,
    sessions::{self, Reporter},
};
use aionix_crypto::{PublicKey, token::TokenVerifier};
use aionix_server_common::{
    config::{ConfigError, Env, VarSource},
    health::{FnCheck, Health},
    http::{TrustedProxies, with_request_tracing},
    keys::{parse_public_keys, verifier_for},
    ratelimit::{Limit, MemoryRateLimiter},
    shutdown::Shutdown,
};
use axum::{
    Router,
    extract::{
        ConnectInfo, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tracing::{Instrument, debug, info, info_span, warn};
use uuid::Uuid;

/// Largest accepted client message.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const CONNECT_LIMIT: Limit = Limit::per_minute(30);
const MESSAGE_LIMIT: Limit = Limit::per_second(20);

#[derive(Debug, Clone)]
pub struct Timing {
    /// Server `Ping` cadence (also announced in `Ready`).
    pub ping_interval: Duration,
    /// Silence after which the link is considered dead.
    pub heartbeat_timeout: Duration,
    /// How long a device stays `Connecting` after an unexpected loss.
    pub reconnect_grace: Duration,
}

impl Timing {
    /// TTL of a link's presence record. A silent link is only detected on the ping tick
    /// after `heartbeat_timeout`, up to one `ping_interval` later; the record must outlive
    /// that moment (plus one more interval of slack for store latency) so the link can
    /// still hand it over to the `Connecting` grace period instead of having it vanish
    /// first. If the signaling process dies, the record expires on its own after this.
    pub fn presence_ttl(&self) -> Duration {
        self.heartbeat_timeout + self.ping_interval * 2
    }
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(15),
            heartbeat_timeout: Duration::from_secs(45),
            reconnect_grace: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SignalingConfig {
    pub bind: SocketAddr,
    pub database_url: String,
    pub db_max_connections: u32,
    pub redis_url: String,
    pub redis_prefix: String,
    pub token_keys: Vec<PublicKey>,
    pub timing: Timing,
    pub trusted_proxies: TrustedProxies,
}

impl SignalingConfig {
    pub fn from_env(src: &dyn VarSource) -> Result<Self, ConfigError> {
        let mut env = Env::new(src);
        let store = store_config(&mut env);
        let bind = env.parse_or("AIONIX_SIGNALING_BIND", "0.0.0.0:8081".parse().unwrap());
        let keys_raw = env.list("AIONIX_TOKEN_PUBLIC_KEYS");
        if keys_raw.is_empty() {
            env.problem(
                "AIONIX_TOKEN_PUBLIC_KEYS",
                "required (output of `aionix-admin public-key`)",
            );
        }
        let token_keys = parse_public_keys(&keys_raw).unwrap_or_else(|e| {
            env.problem("AIONIX_TOKEN_PUBLIC_KEYS", e.to_string());
            Vec::new()
        });
        let timing = Timing {
            ping_interval: env.secs_or("AIONIX_SIGNAL_PING_SECS", 15),
            heartbeat_timeout: env.secs_or("AIONIX_SIGNAL_HEARTBEAT_TIMEOUT_SECS", 45),
            reconnect_grace: env.secs_or("AIONIX_SIGNAL_RECONNECT_GRACE_SECS", 30),
        };
        if timing.heartbeat_timeout <= timing.ping_interval {
            env.problem(
                "AIONIX_SIGNAL_HEARTBEAT_TIMEOUT_SECS",
                "must be larger than AIONIX_SIGNAL_PING_SECS",
            );
        }
        let trusted_proxies = trusted_proxies(&mut env);
        env.finish()?;
        Ok(Self {
            bind,
            database_url: store.database_url,
            db_max_connections: store.db_max_connections,
            redis_url: store.redis_url,
            redis_prefix: store.redis_prefix,
            token_keys,
            timing,
            trusted_proxies,
        })
    }
}

enum Outgoing {
    Msg(ServerMessage),
    Close(u16, &'static str),
}

struct LinkHandle {
    conn_id: Uuid,
    tx: mpsc::Sender<Outgoing>,
}

/// One signaling instance.
pub struct Signaling {
    db: PgPool,
    presence: Arc<dyn PresenceStore>,
    bus: Arc<dyn SignalBus>,
    verifier: TokenVerifier,
    timing: Timing,
    proxies: TrustedProxies,
    links: Mutex<HashMap<Uuid, Vec<LinkHandle>>>,
    link_count: AtomicU64,
    limiter: MemoryRateLimiter,
    shutdown: Shutdown,
    instance: String,
}

impl Signaling {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: PgPool,
        presence: Arc<dyn PresenceStore>,
        bus: Arc<dyn SignalBus>,
        verifier: TokenVerifier,
        timing: Timing,
        proxies: TrustedProxies,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            presence,
            bus,
            verifier,
            timing,
            proxies,
            links: Mutex::new(HashMap::new()),
            link_count: AtomicU64::new(0),
            limiter: MemoryRateLimiter::new(),
            shutdown: Shutdown::new(),
            instance: format!(
                "{}-{}",
                std::env::var("HOSTNAME").unwrap_or_else(|_| "signaling".into()),
                &Uuid::new_v4().simple().to_string()[..8]
            ),
        })
    }

    /// Links currently held by this instance.
    pub fn link_count(&self) -> u64 {
        self.link_count.load(Ordering::SeqCst)
    }

    pub fn shutdown_handle(&self) -> Shutdown {
        self.shutdown.clone()
    }

    /// Starts forwarding bus messages to local links.
    pub async fn start_dispatcher(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut rx = self.bus.device_messages().await?;
        let me = self.clone();
        tokio::spawn(async move {
            while let Some((device, msg)) = rx.recv().await {
                me.dispatch(device, msg).await;
            }
        });
        Ok(())
    }

    async fn dispatch(&self, device: Uuid, msg: DeviceBusMessage) {
        let targets: Vec<(Uuid, mpsc::Sender<Outgoing>)> = self
            .links
            .lock()
            .unwrap()
            .get(&device)
            .map(|v| v.iter().map(|l| (l.conn_id, l.tx.clone())).collect())
            .unwrap_or_default();
        for (conn_id, tx) in targets {
            match &msg {
                DeviceBusMessage::Deliver { message } => {
                    let _ = tx.send(Outgoing::Msg(message.clone())).await;
                }
                DeviceBusMessage::Supersede { keep } if *keep != conn_id => {
                    let _ = tx
                        .send(Outgoing::Close(close_code::SUPERSEDED, "superseded"))
                        .await;
                }
                DeviceBusMessage::Supersede { .. } => {}
                DeviceBusMessage::Revoked => {
                    let _ = tx.send(Outgoing::Msg(ServerMessage::DeviceRevoked)).await;
                    let _ = tx
                        .send(Outgoing::Close(
                            close_code::DEVICE_REVOKED,
                            "device revoked",
                        ))
                        .await;
                }
            }
        }
    }

    /// Closes every link with 1012 and waits (bounded) until they are released.
    pub async fn drain(&self, timeout: Duration) {
        self.shutdown.trigger();
        let deadline = tokio::time::Instant::now() + timeout;
        while self.link_count() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        info!(remaining = self.link_count(), "signaling drained");
    }
}

/// Connects the stores and builds an instance plus its readiness checks.
pub async fn build(config: &SignalingConfig) -> anyhow::Result<(Arc<Signaling>, Health)> {
    let pool = db::connect(&config.database_url, config.db_max_connections, 30).await?;
    let (client, conn) = redis_conn::connect(&config.redis_url, 30).await?;
    let keys = Keys::new(config.redis_prefix.clone());
    from_parts(
        pool,
        client,
        conn,
        keys,
        verifier_for(&config.token_keys),
        config.timing.clone(),
        config.trusted_proxies.clone(),
    )
    .await
}

/// Builds an instance on existing connections (tests, embedding) and starts its
/// dispatcher.
pub async fn from_parts(
    pool: PgPool,
    client: redis::Client,
    conn: redis::aio::ConnectionManager,
    keys: Keys,
    verifier: TokenVerifier,
    timing: Timing,
    proxies: TrustedProxies,
) -> anyhow::Result<(Arc<Signaling>, Health)> {
    let bus = Arc::new(RedisBus::new(client, conn.clone(), keys.clone()));
    let presence = Arc::new(RedisPresence::new(conn.clone(), keys));
    let sig = Signaling::new(
        pool.clone(),
        presence,
        bus.clone(),
        verifier,
        timing,
        proxies,
    );
    sig.start_dispatcher().await?;
    let mut checks = store_checks(pool, conn);
    checks.push(Box::new(FnCheck::new("bus", move || {
        let ok = bus.device_listener_active();
        Box::pin(async move {
            if ok {
                Ok(())
            } else {
                Err("device subscription not active".to_owned())
            }
        })
    })));
    let shutdown = sig.shutdown_handle();
    checks.push(Box::new(FnCheck::new("accepting", move || {
        let stopping = shutdown.is_triggered();
        Box::pin(async move {
            if stopping {
                Err("shutting down".to_owned())
            } else {
                Ok(())
            }
        })
    })));
    Ok((sig, Health::new("aionix-signaling", checks)))
}

pub fn router(sig: Arc<Signaling>, health: Health) -> Router {
    let api = Router::new()
        .route("/v1/signal", get(upgrade))
        .with_state(sig);
    with_request_tracing(api.merge(health.router()))
}

fn refuse(status: StatusCode, detail: &'static str) -> Response {
    (
        status,
        axum::Json(json!({"code": "unauthorized", "detail": detail})),
    )
        .into_response()
}

async fn upgrade(
    State(sig): State<Arc<Signaling>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let ip = sig.proxies.client_ip(peer, &headers).to_string();
    if sig.shutdown.is_triggered() {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "restarting");
    }
    if !sig
        .limiter
        .check(&format!("connect:{ip}"), CONNECT_LIMIT)
        .is_allowed()
    {
        metrics::counter!("aionix_signaling_refused_total", "reason" => "rate_limited")
            .increment(1);
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "too many connection attempts",
        );
    }
    let Some(token) = aionix_control_plane::api::bearer(&headers) else {
        metrics::counter!("aionix_signaling_refused_total", "reason" => "no_token").increment(1);
        return refuse(StatusCode::UNAUTHORIZED, "missing device token");
    };
    let verified = match sig.verifier.verify::<DeviceClaims>(token) {
        Ok(v) => v,
        Err(e) => {
            debug!(error = %e, %ip, "signaling token refused");
            metrics::counter!("aionix_signaling_refused_total", "reason" => "bad_token")
                .increment(1);
            return refuse(StatusCode::UNAUTHORIZED, "invalid device token");
        }
    };
    let claims = verified.claims;
    let device = match devices::by_id(&sig.db, claims.sub).await {
        Ok(Some(d)) => d,
        Ok(None) => return refuse(StatusCode::UNAUTHORIZED, "unknown device"),
        Err(e) => {
            warn!(error = %e, "database unavailable");
            return refuse(StatusCode::SERVICE_UNAVAILABLE, "database unavailable");
        }
    };
    if device.key() != Some(claims.key) {
        return refuse(StatusCode::UNAUTHORIZED, "key mismatch");
    }
    if device.is_revoked() {
        let _ = AuditRecord::security(action::SIGNALING_REFUSED, Outcome::Denied)
            .device(device.id)
            .org(device.org_id)
            .client(
                Some(&ip),
                aionix_server_common::http::request_id(&headers).as_deref(),
            )
            .details(json!({"reason": "device_revoked"}))
            .write(&sig.db)
            .await;
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({"code": "device_revoked", "detail": "device identity revoked"})),
        )
            .into_response();
    }
    let span = info_span!("signal_link", device_id = %device.id, org_id = ?device.org_id);
    let expires_at = verified.expires_at;
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            run_link(sig, socket, device.id, device.friendly(), expires_at).instrument(span)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// Orderly goodbye or revocation: offline at once.
    Offline,
    /// Unexpected loss, timeout, token expiry, restart: connecting for the grace.
    Lost,
    /// A newer link owns the presence: leave it alone.
    Superseded,
}

async fn run_link(
    sig: Arc<Signaling>,
    socket: WebSocket,
    device: Uuid,
    friendly: aionix_shared::FriendlyId,
    token_expires: time::OffsetDateTime,
) {
    let conn_id = Uuid::new_v4();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Outgoing>(64);
    let mut record = PresenceRecord::connecting(conn_id, &sig.instance);
    let ttl = sig.timing.presence_ttl();

    match sig.presence.claim(device, &record, ttl).await {
        Ok(Some(_previous)) => {
            let _ = sig
                .bus
                .publish_device(device, &DeviceBusMessage::Supersede { keep: conn_id })
                .await;
        }
        Ok(None) => {}
        Err(e) => {
            warn!(error = %e, "presence store unavailable; refusing link");
            let _ = sink
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::SERVICE_RESTART,
                    reason: "presence unavailable".into(),
                })))
                .await;
            return;
        }
    }
    sig.links
        .lock()
        .unwrap()
        .entry(device)
        .or_default()
        .push(LinkHandle {
            conn_id,
            tx: tx.clone(),
        });
    sig.link_count.fetch_add(1, Ordering::SeqCst);
    metrics::gauge!("aionix_signaling_links").set(sig.link_count() as f64);
    let _ = devices::mark_seen(&sig.db, device).await;
    info!(%conn_id, "device link open");

    let ready = ServerMessage::Ready {
        device_id: device,
        friendly_id: friendly,
        ping_interval: sig.timing.ping_interval.as_secs(),
    };
    let mut end = End::Lost;
    let mut close: Option<(u16, &'static str)> = None;
    if send(&mut sink, &ready).await.is_err() {
        close = None;
    } else {
        let mut ping = tokio::time::interval(sig.timing.ping_interval);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping.tick().await;
        let mut ping_id = 0u64;
        let mut last_heard = tokio::time::Instant::now();
        let mut last_seen_write = tokio::time::Instant::now();
        let token_left = (token_expires - time::OffsetDateTime::now_utc())
            .try_into()
            .unwrap_or(Duration::ZERO);
        let token_deadline = tokio::time::sleep(token_left);
        tokio::pin!(token_deadline);
        let shutdown = sig.shutdown.clone();
        loop {
            tokio::select! {
                incoming = stream.next() => {
                    let msg = match incoming {
                        Some(Ok(m)) => m,
                        _ => break,
                    };
                    last_heard = tokio::time::Instant::now();
                    let text = match msg {
                        Message::Text(t) => t,
                        Message::Ping(_) | Message::Pong(_) => {
                            if !heartbeat(&sig, device, &record).await { end = End::Superseded; break; }
                            continue;
                        }
                        Message::Close(_) => break,
                        Message::Binary(_) => { close = Some((close_code::POLICY, "binary frames not accepted")); break; }
                    };
                    if !sig.limiter.check(&format!("msg:{conn_id}"), MESSAGE_LIMIT).is_allowed() {
                        close = Some((close_code::POLICY, "message rate exceeded"));
                        break;
                    }
                    let parsed: ClientMessage = match serde_json::from_str(text.as_str()) {
                        Ok(m) => m,
                        Err(_) => { close = Some((close_code::POLICY, "invalid message")); break; }
                    };
                    match parsed {
                        ClientMessage::Presence { addr, unattended_enabled, busy } => {
                            record.state = PresenceState::Online;
                            record.addr = addr;
                            record.busy = busy;
                            record.unattended_enabled = unattended_enabled;
                            if !heartbeat(&sig, device, &record).await { end = End::Superseded; break; }
                        }
                        ClientMessage::Pong { .. } => {
                            if !heartbeat(&sig, device, &record).await { end = End::Superseded; break; }
                        }
                        ClientMessage::SessionOfferResponse { session_id, accept, reason, addr } => {
                            if !heartbeat(&sig, device, &record).await { end = End::Superseded; break; }
                            forward_offer_answer(&sig, device, session_id, OfferAnswer { accept, reason, addr }).await;
                        }
                        ClientMessage::SessionEvent { session_id, kind, transport, detail } => {
                            if !heartbeat(&sig, device, &record).await { end = End::Superseded; break; }
                            if let Err(e) = sessions::record_event(
                                &sig.db, session_id, Reporter::Host { device }, kind, transport, &detail, (None, None),
                            ).await {
                                warn!(error = %e, %session_id, "could not record session event");
                            }
                        }
                        ClientMessage::Goodbye => { end = End::Offline; break; }
                    }
                    if last_seen_write.elapsed() > Duration::from_secs(60) {
                        let _ = devices::mark_seen(&sig.db, device).await;
                        last_seen_write = tokio::time::Instant::now();
                    }
                }
                out = rx.recv() => {
                    match out {
                        Some(Outgoing::Msg(m)) => {
                            let revoked = matches!(m, ServerMessage::DeviceRevoked);
                            if send(&mut sink, &m).await.is_err() { break; }
                            if revoked { end = End::Offline; }
                        }
                        Some(Outgoing::Close(code, reason)) => {
                            if code == close_code::SUPERSEDED { end = End::Superseded; }
                            if code == close_code::DEVICE_REVOKED { end = End::Offline; }
                            close = Some((code, reason));
                            break;
                        }
                        None => break,
                    }
                }
                _ = ping.tick() => {
                    if last_heard.elapsed() > sig.timing.heartbeat_timeout {
                        close = Some((close_code::HEARTBEAT_TIMEOUT, "heartbeat timeout"));
                        break;
                    }
                    ping_id += 1;
                    if send(&mut sink, &ServerMessage::Ping { id: ping_id }).await.is_err() { break; }
                }
                _ = &mut token_deadline => {
                    close = Some((close_code::TOKEN_EXPIRED, "device token expired"));
                    break;
                }
                _ = shutdown.wait() => {
                    close = Some((close_code::SERVICE_RESTART, "server restarting"));
                    break;
                }
            }
        }
    }
    if let Some((code, reason)) = close {
        let _ = sink
            .send(Message::Close(Some(CloseFrame {
                code,
                reason: reason.into(),
            })))
            .await;
    }
    let _ = sink.close().await;

    {
        let mut links = sig.links.lock().unwrap();
        if let Some(v) = links.get_mut(&device) {
            v.retain(|l| l.conn_id != conn_id);
            if v.is_empty() {
                links.remove(&device);
            }
        }
    }
    let release = match end {
        End::Offline => sig.presence.release(device, conn_id, None).await,
        End::Lost => {
            sig.presence
                .release(device, conn_id, Some((record, sig.timing.reconnect_grace)))
                .await
        }
        End::Superseded => Ok(()),
    };
    if let Err(e) = release {
        warn!(error = %e, "could not release presence");
    }
    let _ = devices::mark_seen(&sig.db, device).await;
    // Counted down only after presence was released, so `drain` returning means every
    // device already shows Connecting (not a stale Online).
    sig.link_count.fetch_sub(1, Ordering::SeqCst);
    metrics::gauge!("aionix_signaling_links").set(sig.link_count() as f64);
    info!(%conn_id, end = ?end, close = ?close.map(|c| c.0), "device link closed");
}

async fn send(
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
    msg: &ServerMessage,
) -> Result<(), axum::Error> {
    let text = serde_json::to_string(msg).expect("server messages serialize");
    sink.send(Message::Text(text.into())).await
}

/// Refreshes presence; `false` once a newer link owns it.
async fn heartbeat(sig: &Signaling, device: Uuid, record: &PresenceRecord) -> bool {
    match sig
        .presence
        .refresh(device, record, sig.timing.presence_ttl())
        .await
    {
        Ok(owned) => owned,
        Err(e) => {
            // Redis hiccup: keep the link; the next heartbeat recreates the record.
            warn!(error = %e, "presence refresh failed");
            true
        }
    }
}

async fn forward_offer_answer(sig: &Signaling, device: Uuid, session: Uuid, answer: OfferAnswer) {
    let host: Result<Option<Uuid>, _> =
        sqlx::query_scalar("SELECT host_device_id FROM sessions WHERE id = $1")
            .bind(session)
            .fetch_optional(&sig.db)
            .await;
    match host {
        Ok(Some(h)) if h == device => {
            if let Err(e) = sig.bus.publish_offer_answer(session, &answer).await {
                warn!(error = %e, session_id = %session, "could not forward offer answer");
            }
        }
        Ok(_) => {
            let _ = AuditRecord::security(action::PERMISSION_DENIED, Outcome::Denied)
                .device(device)
                .session(session)
                .details(json!({"reason": "offer_answer_from_non_host"}))
                .write(&sig.db)
                .await;
        }
        Err(e) => warn!(error = %e, "database unavailable"),
    }
}
