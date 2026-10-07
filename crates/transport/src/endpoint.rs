use aionix_control_api::signal::EndpointAddrDto;
use aionix_crypto::PublicKey;
use futures::{Stream, StreamExt};
use iroh::{
    Endpoint, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher,
    endpoint::{IdleTimeout, QuicTransportConfig, VarInt, presets},
};
use std::{sync::Arc, time::Duration};
use tracing::{debug, warn};

use crate::{TransportError, addr, connection::PeerConnection};

/// Connection establishment budget (hole punching may take a few round trips; the
/// relay path is available almost immediately so this is generous).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// QUIC keepalive. Short enough to keep NAT bindings open on aggressive routers and to
/// detect dead paths quickly.
const KEEP_ALIVE: Duration = Duration::from_secs(2);
/// Connection survives this long without any packet before being declared dead.
/// Path changes (Wi-Fi → cable, relay → direct) are handled transparently within it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Video uses one unidirectional stream per frame.
const MAX_UNI_STREAMS: u32 = 1024;
const MAX_BIDI_STREAMS: u32 = 32;
/// Flow control windows sized for 4K keyframes at high bitrate.
const STREAM_RECEIVE_WINDOW: u32 = 16 * 1024 * 1024;
const CONNECTION_RECEIVE_WINDOW: u32 = 64 * 1024 * 1024;
const SEND_WINDOW: u64 = 64 * 1024 * 1024;

/// A relay server this endpoint may use, with its access token.
#[derive(Debug, Clone)]
pub struct RelayEndpoint {
    pub url: String,
    pub token: Option<String>,
    /// UDP port of the relay's QUIC address discovery service, if enabled.
    pub qad_port: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Ed25519 secret seed of the local identity.
    pub secret_key: [u8; 32],
    pub relays: Vec<RelayEndpoint>,
    /// Application protocols accepted on incoming connections.
    pub alpns: Vec<Vec<u8>>,
    /// Local UDP port (0 = OS assigned).
    pub port: u16,
    /// Test/diagnostic switch: only use relays, never direct paths.
    pub relay_only: bool,
}

/// Local network endpoint: owns the UDP sockets, relay connection and NAT traversal.
#[derive(Clone, Debug)]
pub struct Transport {
    endpoint: Endpoint,
}

impl Transport {
    pub async fn bind(config: TransportConfig) -> Result<Self, TransportError> {
        let secret = SecretKey::from_bytes(&config.secret_key);
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .alpns(config.alpns.clone())
            .transport_config(quic_transport_config()?);

        builder = if config.relays.is_empty() {
            builder.relay_mode(RelayMode::Disabled)
        } else {
            builder.relay_mode(RelayMode::Custom(relay_map(&config.relays)?))
        };

        if config.relay_only {
            builder = builder.clear_ip_transports();
        } else if config.port != 0 {
            builder = builder
                .clear_ip_transports()
                .bind_addr(format!("0.0.0.0:{}", config.port).as_str())
                .map_err(|e| TransportError::Bind(e.to_string()))?
                .bind_addr(format!("[::]:{}", config.port).as_str())
                .map_err(|e| TransportError::Bind(e.to_string()))?;
        }

        let endpoint = builder.bind().await.map_err(|e| TransportError::Bind(e.to_string()))?;
        debug!(id = %endpoint.id().fmt_short(), "transport bound");
        Ok(Self { endpoint })
    }

    pub fn local_key(&self) -> PublicKey {
        PublicKey::from_bytes(self.endpoint.id().as_bytes()).expect("iroh ids are ed25519 keys")
    }

    /// Current reachability (relay URL + direct candidates) for signaling.
    pub fn addr(&self) -> EndpointAddrDto {
        addr::to_dto(&self.endpoint.addr())
    }

    /// Stream of reachability updates; yields the current value first.
    pub fn addr_updates(&self) -> impl Stream<Item = EndpointAddrDto> + Send + 'static {
        self.endpoint.watch_addr().stream().map(|a| addr::to_dto(&a))
    }

    /// Waits until connected to a home relay (or until `timeout`). Returns whether
    /// the endpoint is reachable through a relay.
    pub async fn wait_online(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.endpoint.online()).await.is_ok()
    }

    /// Relay connection status for diagnostics: `(url, connected, denial reason)`.
    pub fn relay_status(&self) -> Vec<(String, bool, Option<String>)> {
        self.endpoint
            .home_relay_status()
            .get()
            .into_iter()
            .map(|s| (s.url().to_string(), s.is_connected(), s.auth_denied_reason().map(str::to_owned)))
            .collect()
    }

    /// Replaces the access token for `url` (tokens are short-lived and renewed).
    pub async fn update_relay_token(&self, relay: &RelayEndpoint) -> Result<(), TransportError> {
        let url: RelayUrl = relay
            .url
            .parse()
            .map_err(|_| TransportError::InvalidAddress(relay.url.clone()))?;
        self.endpoint.insert_relay(url, Arc::new(relay_config(relay)?)).await;
        Ok(())
    }

    /// Opens an authenticated, encrypted connection to `peer`.
    pub async fn connect(
        &self,
        peer: &PublicKey,
        addr: &EndpointAddrDto,
        alpn: &[u8],
    ) -> Result<PeerConnection, TransportError> {
        let id = iroh::EndpointId::from_bytes(&peer.to_bytes())
            .map_err(|_| TransportError::InvalidAddress("peer key".into()))?;
        let target = addr::from_dto(id, addr)?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(target, alpn))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        Ok(PeerConnection::new(conn))
    }

    /// Waits for the next incoming connection attempt. `None` once closed.
    pub async fn accept(&self) -> Option<Incoming> {
        self.endpoint.accept().await.map(|inner| Incoming { inner })
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }

    /// Notifies the backend that the network changed (e.g. from OS events) so it
    /// re-probes paths immediately instead of waiting for timeouts.
    pub async fn network_changed(&self) {
        self.endpoint.network_change().await;
    }
}

/// An incoming connection attempt, before the handshake completes.
pub struct Incoming {
    inner: iroh::endpoint::Incoming,
}

impl Incoming {
    /// Completes the handshake. The peer's identity is authenticated afterwards;
    /// application-level authorization (session tickets) is the caller's job.
    pub async fn accept(self) -> Result<PeerConnection, TransportError> {
        let accepting = self.inner.accept().map_err(|e| TransportError::Connect(e.to_string()))?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, accepting)
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        Ok(PeerConnection::new(conn))
    }

    /// Rejects the attempt without completing the handshake.
    pub fn refuse(self) {
        self.inner.refuse();
    }
}

fn quic_transport_config() -> Result<QuicTransportConfig, TransportError> {
    let idle = IdleTimeout::try_from(IDLE_TIMEOUT)
        .map_err(|e| TransportError::Bind(format!("idle timeout: {e}")))?;
    Ok(QuicTransportConfig::builder()
        .keep_alive_interval(KEEP_ALIVE)
        .max_idle_timeout(Some(idle))
        .max_concurrent_uni_streams(VarInt::from_u32(MAX_UNI_STREAMS))
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_BIDI_STREAMS))
        .stream_receive_window(VarInt::from_u32(STREAM_RECEIVE_WINDOW))
        .receive_window(VarInt::from_u32(CONNECTION_RECEIVE_WINDOW))
        .send_window(SEND_WINDOW)
        .build())
}

fn relay_config(relay: &RelayEndpoint) -> Result<RelayConfig, TransportError> {
    let url: RelayUrl = relay
        .url
        .parse()
        .map_err(|_| TransportError::InvalidAddress(relay.url.clone()))?;
    let quic = relay.qad_port.map(iroh_relay::RelayQuicConfig::new);
    let mut cfg = RelayConfig::new(url, quic);
    if let Some(token) = &relay.token {
        cfg = cfg.with_auth_token(token.clone());
    }
    Ok(cfg)
}

fn relay_map(relays: &[RelayEndpoint]) -> Result<RelayMap, TransportError> {
    let mut configs = Vec::with_capacity(relays.len());
    for r in relays {
        configs.push(relay_config(r)?);
    }
    if configs.is_empty() {
        warn!("no relays configured");
    }
    Ok(configs.into_iter().collect())
}
