//! Encrypted peer-to-peer transport.
//!
//! This crate is the only place that knows about the networking backend (currently
//! [iroh], QUIC with hole punching and relay fallback; see ADR-0002). The rest of the
//! system talks to [`Transport`], [`PeerConnection`], [`SendStream`] and [`RecvStream`].
//!
//! Guarantees provided to callers:
//! * Every connection is mutually authenticated: [`PeerConnection::remote_key`] is the
//!   Ed25519 key proven during the TLS 1.3 handshake.
//! * Traffic is end-to-end encrypted with ephemeral keys (forward secrecy), including
//!   when forwarded by a relay.
//! * The best path is chosen automatically (LAN → internet direct → relay) and can
//!   change during a connection without the application noticing, except through
//!   [`PeerConnection::path`] diagnostics.

mod addr;
mod connection;
mod endpoint;

pub use addr::{classify_ip, to_dto};
pub use connection::{PathSnapshot, PeerConnection, RecvStream, SendStream, StreamError};
pub use endpoint::{Incoming, RelayEndpoint, Transport, TransportConfig};

/// Errors surfaced by the transport layer.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("failed to bind transport: {0}")]
    Bind(String),
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("connection lost: {0}")]
    Connection(String),
    #[error("transport closed")]
    Closed,
    #[error("timed out")]
    Timeout,
}
