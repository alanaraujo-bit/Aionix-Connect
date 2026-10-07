//! `aionix-relay` — standalone relay service (ADR-0013).
//!
//! Wraps the iroh relay server already used by the transport (ADR-0002) with Aionix
//! access control. A relay forwards QUIC packets that are end-to-end encrypted between
//! the peers; it never holds keys able to decrypt screen, input or clipboard data.
//!
//! An endpoint is admitted only when all of these hold:
//! * it presents a relay grant (`axt1` token of type `relay`) signed by a trusted
//!   Control Plane key, unexpired;
//! * the grant's `key` equals the endpoint id the relay handshake just proved;
//! * the device was not revoked (Redis marker written by the Control Plane; checked
//!   when Redis is configured — see SECURITY.md for the fail-open choice);
//! * the client IP is under the connection-attempt rate limit and the relay is under
//!   its connection cap.
//!
//! Per-connection receive bandwidth can be capped (`AIONIX_RELAY_CLIENT_RX_BYTES_PER_SEC`).
//! A separate status listener serves `/healthz`, `/readyz`, `/metrics` and `/status`.

use std::{
    net::SocketAddr,
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use aionix_control_api::claims::RelayClaims;
use aionix_crypto::{PublicKey, token::TokenVerifier};
use aionix_server_common::{
    config::{ConfigError, Env, VarSource, check_redis_url},
    health::{FnCheck, Health},
    keys::{parse_public_keys, verifier_for},
    ratelimit::{Limit, MemoryRateLimiter},
};
use axum::{Json, Router, routing::get};
use iroh_relay::server::{
    Access, AccessControl, ClientRateLimit, ClientRequest, ConnectionId, Limits, Metrics,
    RelayConfig, Server, ServerConfig,
};
use serde::Serialize;
use tracing::{debug, info, warn};

/// Redis key read to refuse revoked devices: `<prefix>:revoked:<device uuid>`.
pub const REVOKED_KEY_FORMAT: &str = "<prefix>:revoked:<device>";

#[derive(Debug, Clone)]
pub struct RelaySettings {
    /// Relay HTTP listener (`/relay`, `/ping`, `/generate_204`); TLS is terminated by
    /// the reverse proxy.
    pub bind: SocketAddr,
    /// Status listener (`/healthz`, `/readyz`, `/metrics`, `/status`); internal only.
    pub status_bind: SocketAddr,
    pub token_keys: Vec<PublicKey>,
    pub redis_url: Option<String>,
    pub redis_prefix: String,
    pub client_rx_bytes_per_sec: Option<u32>,
    pub connect_limit_per_minute: u32,
    pub max_connections: i64,
}

impl RelaySettings {
    pub fn from_env(src: &dyn VarSource) -> Result<Self, ConfigError> {
        let mut env = Env::new(src);
        let bind = env.parse_or("AIONIX_RELAY_BIND", "0.0.0.0:3340".parse().unwrap());
        let status_bind = env.parse_or("AIONIX_RELAY_STATUS_BIND", "0.0.0.0:9090".parse().unwrap());
        let raw = env.list("AIONIX_TOKEN_PUBLIC_KEYS");
        if raw.is_empty() {
            env.problem(
                "AIONIX_TOKEN_PUBLIC_KEYS",
                "required (output of `aionix-admin public-key`)",
            );
        }
        let token_keys = parse_public_keys(&raw).unwrap_or_else(|e| {
            env.problem("AIONIX_TOKEN_PUBLIC_KEYS", e.to_string());
            Vec::new()
        });
        let redis_url = env.optional("AIONIX_REDIS_URL");
        if let Some(u) = &redis_url {
            check_redis_url(&mut env, "AIONIX_REDIS_URL", u);
        }
        let redis_prefix = env.string_or("AIONIX_REDIS_PREFIX", "aionix");
        let rx: u32 = env.parse_or("AIONIX_RELAY_CLIENT_RX_BYTES_PER_SEC", 0);
        let settings = Self {
            bind,
            status_bind,
            token_keys,
            redis_url,
            redis_prefix,
            client_rx_bytes_per_sec: (rx > 0).then_some(rx),
            connect_limit_per_minute: env.parse_or("AIONIX_RELAY_CONNECT_LIMIT_PER_MINUTE", 120),
            max_connections: env.parse_or("AIONIX_RELAY_MAX_CONNECTIONS", 10_000),
        };
        env.finish()?;
        Ok(settings)
    }
}

/// Counters exposed by `/status` and `/metrics`.
#[derive(Debug, Default)]
pub struct RelayStats {
    pub active: AtomicI64,
    pub accepted_total: AtomicU64,
    pub denied_token: AtomicU64,
    pub denied_key_mismatch: AtomicU64,
    pub denied_revoked: AtomicU64,
    pub denied_rate_limited: AtomicU64,
    pub denied_capacity: AtomicU64,
    pub revocation_check_unavailable: AtomicU64,
}

/// Aionix access control for the iroh relay.
#[derive(Debug)]
pub struct RelayAccess {
    verifier: TokenVerifier,
    revocation: Option<(redis::aio::ConnectionManager, String)>,
    limiter: MemoryRateLimiter,
    connect_limit: Limit,
    max_connections: i64,
    stats: Arc<RelayStats>,
}

impl RelayAccess {
    pub fn new(
        verifier: TokenVerifier,
        revocation: Option<(redis::aio::ConnectionManager, String)>,
        connect_limit_per_minute: u32,
        max_connections: i64,
        stats: Arc<RelayStats>,
    ) -> Self {
        Self {
            verifier,
            revocation,
            limiter: MemoryRateLimiter::new(),
            connect_limit: Limit::per_minute(connect_limit_per_minute.max(1)),
            max_connections,
            stats,
        }
    }

    fn deny(counter: &AtomicU64, reason: &str) -> Access {
        counter.fetch_add(1, Ordering::Relaxed);
        Access::Deny {
            reason: Some(reason.to_owned()),
        }
    }

    async fn is_revoked(&self, device: uuid::Uuid) -> bool {
        let Some((conn, prefix)) = &self.revocation else {
            return false;
        };
        let mut c = conn.clone();
        let r: Result<bool, _> = redis::cmd("EXISTS")
            .arg(format!("{prefix}:revoked:{device}"))
            .query_async(&mut c)
            .await;
        match r {
            Ok(v) => v,
            Err(e) => {
                self.stats
                    .revocation_check_unavailable
                    .fetch_add(1, Ordering::Relaxed);
                warn!(error = %e, "revocation check unavailable; admitting on signature alone");
                false
            }
        }
    }
}

/// Client address as forwarded by the reverse proxy (the relay listener is only
/// reachable through it).
fn client_ip(req: &ClientRequest) -> String {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "direct".to_owned())
}

impl AccessControl for RelayAccess {
    async fn on_connect(&self, req: &ClientRequest) -> Access {
        let ip = client_ip(req);
        if !self
            .limiter
            .check(&format!("connect:{ip}"), self.connect_limit)
            .is_allowed()
        {
            return Self::deny(&self.stats.denied_rate_limited, "rate limited");
        }
        if self.stats.active.load(Ordering::SeqCst) >= self.max_connections {
            return Self::deny(&self.stats.denied_capacity, "relay at capacity");
        }
        let Some(token) = req.auth_token() else {
            return Self::deny(&self.stats.denied_token, "missing relay grant");
        };
        let claims = match self.verifier.verify::<RelayClaims>(&token) {
            Ok(v) => v.claims,
            Err(e) => {
                debug!(error = %e, %ip, "relay grant refused");
                return Self::deny(&self.stats.denied_token, "invalid relay grant");
            }
        };
        if claims.key.to_bytes() != *req.endpoint_id().as_bytes() {
            return Self::deny(
                &self.stats.denied_key_mismatch,
                "grant issued to another key",
            );
        }
        if self.is_revoked(claims.sub).await {
            return Self::deny(&self.stats.denied_revoked, "device revoked");
        }
        self.stats.active.fetch_add(1, Ordering::SeqCst);
        self.stats.accepted_total.fetch_add(1, Ordering::Relaxed);
        debug!(device_id = %claims.sub, connection = %req.connection_id(), "relay client admitted");
        Access::Allow
    }

    fn on_disconnect(&self, _endpoint: iroh_base::EndpointId, _conn: ConnectionId) {
        self.stats.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Serialize)]
pub struct RelayStatus {
    pub connections_active: i64,
    pub connections_accepted_total: u64,
    pub denied: DeniedCounts,
    pub bytes_sent_total: u64,
    pub bytes_received_total: u64,
    pub packets_dropped_total: u64,
    pub revocation_checks_unavailable: u64,
    pub uptime_seconds: u64,
}

#[derive(Debug, Serialize)]
pub struct DeniedCounts {
    pub invalid_grant: u64,
    pub key_mismatch: u64,
    pub revoked: u64,
    pub rate_limited: u64,
    pub capacity: u64,
}

/// A running relay with its status listener.
pub struct RunningRelay {
    server: Server,
    pub relay_addr: SocketAddr,
    pub status_addr: SocketAddr,
    pub stats: Arc<RelayStats>,
    status_task: tokio::task::JoinHandle<()>,
}

impl RunningRelay {
    pub fn relay_url(&self) -> String {
        format!("http://{}", self.relay_addr)
    }

    /// Stops accepting, closes client connections and the status listener.
    pub async fn shutdown(self) {
        self.status_task.abort();
        if let Err(e) = self.server.shutdown().await {
            warn!(error = %e, "relay shutdown error");
        }
        info!("relay stopped");
    }
}

fn status_of(stats: &RelayStats, m: &Metrics, started: Instant) -> RelayStatus {
    RelayStatus {
        connections_active: stats.active.load(Ordering::SeqCst),
        connections_accepted_total: stats.accepted_total.load(Ordering::Relaxed),
        denied: DeniedCounts {
            invalid_grant: stats.denied_token.load(Ordering::Relaxed),
            key_mismatch: stats.denied_key_mismatch.load(Ordering::Relaxed),
            revoked: stats.denied_revoked.load(Ordering::Relaxed),
            rate_limited: stats.denied_rate_limited.load(Ordering::Relaxed),
            capacity: stats.denied_capacity.load(Ordering::Relaxed),
        },
        bytes_sent_total: m.bytes_sent.get(),
        bytes_received_total: m.bytes_recv.get(),
        packets_dropped_total: m.send_packets_dropped.get(),
        revocation_checks_unavailable: stats.revocation_check_unavailable.load(Ordering::Relaxed),
        uptime_seconds: started.elapsed().as_secs(),
    }
}

fn prometheus(s: &RelayStatus) -> String {
    let mut out = String::new();
    let mut gauge = |name: &str, kind: &str, help: &str, v: String| {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {v}\n"
        ));
    };
    gauge(
        "aionix_relay_connections_active",
        "gauge",
        "Admitted relay connections currently open.",
        s.connections_active.to_string(),
    );
    gauge(
        "aionix_relay_connections_accepted_total",
        "counter",
        "Relay connections admitted.",
        s.connections_accepted_total.to_string(),
    );
    gauge(
        "aionix_relay_bytes_sent_total",
        "counter",
        "Bytes forwarded to clients.",
        s.bytes_sent_total.to_string(),
    );
    gauge(
        "aionix_relay_bytes_received_total",
        "counter",
        "Bytes received from clients.",
        s.bytes_received_total.to_string(),
    );
    gauge(
        "aionix_relay_packets_dropped_total",
        "counter",
        "Packets dropped (recipient gone).",
        s.packets_dropped_total.to_string(),
    );
    gauge(
        "aionix_relay_revocation_checks_unavailable_total",
        "counter",
        "Revocation checks skipped because Redis was unavailable.",
        s.revocation_checks_unavailable.to_string(),
    );
    out.push_str("# HELP aionix_relay_denied_total Refused relay connections by reason.\n# TYPE aionix_relay_denied_total counter\n");
    for (reason, v) in [
        ("invalid_grant", s.denied.invalid_grant),
        ("key_mismatch", s.denied.key_mismatch),
        ("revoked", s.denied.revoked),
        ("rate_limited", s.denied.rate_limited),
        ("capacity", s.denied.capacity),
    ] {
        out.push_str(&format!(
            "aionix_relay_denied_total{{reason=\"{reason}\"}} {v}\n"
        ));
    }
    out
}

/// Starts the relay and its status listener.
pub async fn spawn(settings: &RelaySettings) -> anyhow::Result<RunningRelay> {
    let revocation = match &settings.redis_url {
        Some(url) => {
            let client = redis::Client::open(url.as_str())?;
            match client.get_connection_manager().await {
                Ok(m) => Some((m, settings.redis_prefix.clone())),
                Err(e) => {
                    // The connection manager reconnects later; start anyway.
                    warn!(error = %e, "Redis unavailable at startup; revocation checks disabled until restart");
                    None
                }
            }
        }
        None => {
            warn!(
                "AIONIX_REDIS_URL not set: revoked devices are refused only once their relay grant expires"
            );
            None
        }
    };
    spawn_with(settings, revocation).await
}

pub async fn spawn_with(
    settings: &RelaySettings,
    revocation: Option<(redis::aio::ConnectionManager, String)>,
) -> anyhow::Result<RunningRelay> {
    let stats = Arc::new(RelayStats::default());
    let redis_for_health = revocation.as_ref().map(|(c, _)| c.clone());
    let access = RelayAccess::new(
        verifier_for(&settings.token_keys),
        revocation,
        settings.connect_limit_per_minute,
        settings.max_connections,
        stats.clone(),
    );
    let mut relay = RelayConfig::new(settings.bind);
    relay.access = Arc::new(access);
    let mut limits = Limits::default();
    if let Some(bps) = settings.client_rx_bytes_per_sec.and_then(NonZeroU32::new) {
        limits.client_rx = Some(ClientRateLimit::new(bps));
    }
    relay.limits = limits;
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(relay);
    let server = Server::spawn(cfg)
        .await
        .map_err(|e| anyhow::anyhow!("relay failed to start: {e}"))?;
    let relay_addr = server
        .http_addr()
        .ok_or_else(|| anyhow::anyhow!("relay has no HTTP address"))?;
    let metrics = server.metrics().server.clone();
    let started = Instant::now();

    let mut checks: Vec<Box<dyn aionix_server_common::health::ReadinessCheck>> =
        vec![Box::new(FnCheck::new("relay_listener", move || {
            Box::pin(async move {
                let target = if relay_addr.ip().is_unspecified() {
                    SocketAddr::from(([127, 0, 0, 1], relay_addr.port()))
                } else {
                    relay_addr
                };
                tokio::time::timeout(
                    Duration::from_secs(2),
                    tokio::net::TcpStream::connect(target),
                )
                .await
                .map_err(|_| "relay listener timed out".to_owned())?
                .map(|_| ())
                .map_err(|e| format!("relay listener unreachable: {e}"))
            })
        }))];
    if let Some(conn) = redis_for_health {
        // Reported, but readiness does not depend on it: the relay keeps forwarding
        // (fail-open revocation, see SECURITY.md).
        checks.push(Box::new(FnCheck::new("revocation_store", move || {
            let mut c = conn.clone();
            Box::pin(async move {
                let pong: Result<String, _> = redis::cmd("PING").query_async(&mut c).await;
                if let Err(e) = pong {
                    warn!(error = %e, "revocation store unavailable");
                }
                Ok(())
            })
        })));
    }
    let health = Health::new("aionix-relay", checks);
    let st = stats.clone();
    let m2 = metrics.clone();
    let st2 = stats.clone();
    let status_router = Router::new()
        .route(
            "/status",
            get(move || {
                let s = status_of(&st, &m2, started);
                async move { Json(s) }
            }),
        )
        .route(
            "/metrics",
            get(move || {
                let s = status_of(&st2, &metrics, started);
                async move {
                    (
                        [(
                            axum::http::header::CONTENT_TYPE,
                            "text/plain; version=0.0.4",
                        )],
                        prometheus(&s),
                    )
                }
            }),
        )
        .merge(
            Router::new()
                .route(
                    "/healthz",
                    get(|| async {
                        Json(serde_json::json!({"service": "aionix-relay", "alive": true}))
                    }),
                )
                .merge(readiness_only(health)),
        );
    let listener = tokio::net::TcpListener::bind(settings.status_bind).await?;
    let status_addr = listener.local_addr()?;
    let status_task = tokio::spawn(async move {
        let _ = axum::serve(listener, status_router).await;
    });
    info!(relay = %relay_addr, status = %status_addr, "aionix-relay running");
    Ok(RunningRelay {
        server,
        relay_addr,
        status_addr,
        stats,
        status_task,
    })
}

/// `/readyz` from the shared health module (its `/metrics` is replaced by ours).
fn readiness_only(health: Health) -> Router {
    Router::new().route(
        "/readyz",
        get(move || {
            let h = health.clone();
            async move {
                let r = h.readiness().await;
                let code = if r.ready {
                    axum::http::StatusCode::OK
                } else {
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                };
                (code, Json(r))
            }
        }),
    )
}
