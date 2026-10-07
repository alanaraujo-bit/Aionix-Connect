use aionix_crypto::PublicKey;
use aionix_shared::TransportKind;
use iroh::{TransportAddr, endpoint::VarInt};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{TransportError, addr::classify_ip};

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("stream reset by peer (code {0})")]
    Reset(u64),
    #[error("connection lost: {0}")]
    ConnectionLost(String),
    #[error("stream closed")]
    Closed,
    #[error("{0}")]
    Other(String),
}

/// Diagnostics for the path currently used to carry application data.
#[derive(Debug, Clone, PartialEq)]
pub struct PathSnapshot {
    pub kind: TransportKind,
    pub rtt: Duration,
    /// Remote transport address (IP:port or relay URL), for technical diagnostics.
    pub remote: String,
    /// Number of open candidate paths (direct + relay).
    pub path_count: usize,
    pub cwnd: u64,
    pub lost_packets: u64,
    pub sent_packets: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}

/// An authenticated, end-to-end encrypted connection to a peer.
#[derive(Clone, Debug)]
pub struct PeerConnection {
    conn: iroh::endpoint::Connection,
}

impl PeerConnection {
    pub(crate) fn new(conn: iroh::endpoint::Connection) -> Self {
        Self { conn }
    }

    /// Ed25519 key of the peer, proven during the handshake.
    pub fn remote_key(&self) -> PublicKey {
        PublicKey::from_bytes(self.conn.remote_id().as_bytes()).expect("iroh ids are ed25519 keys")
    }

    pub fn alpn(&self) -> Vec<u8> {
        self.conn.alpn().to_vec()
    }

    /// Stable id for logging.
    pub fn id(&self) -> usize {
        self.conn.stable_id()
    }

    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), TransportError> {
        let (s, r) = self.conn.open_bi().await.map_err(|e| TransportError::Connection(e.to_string()))?;
        Ok((SendStream(s), RecvStream(r)))
    }

    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), TransportError> {
        let (s, r) = self.conn.accept_bi().await.map_err(|e| TransportError::Connection(e.to_string()))?;
        Ok((SendStream(s), RecvStream(r)))
    }

    pub async fn open_uni(&self) -> Result<SendStream, TransportError> {
        let s = self.conn.open_uni().await.map_err(|e| TransportError::Connection(e.to_string()))?;
        Ok(SendStream(s))
    }

    pub async fn accept_uni(&self) -> Result<RecvStream, TransportError> {
        let r = self.conn.accept_uni().await.map_err(|e| TransportError::Connection(e.to_string()))?;
        Ok(RecvStream(r))
    }

    /// Snapshot of the selected path. Falls back to the first open path while the
    /// selection is still being made.
    pub fn path(&self) -> PathSnapshot {
        let stats = self.conn.stats();
        let paths = self.conn.paths();
        let path_count = paths.iter().count();
        let selected = paths.iter().find(|p| p.is_selected()).or_else(|| paths.iter().next());
        let (kind, rtt, remote, cwnd) = match selected {
            Some(p) => {
                let s = p.stats();
                let (kind, remote) = match p.remote_addr() {
                    TransportAddr::Ip(sock) => (classify_ip(sock.ip()), sock.to_string()),
                    TransportAddr::Relay(url) => (TransportKind::Relay, url.to_string()),
                    other => (TransportKind::Unknown, format!("{other:?}")),
                };
                (kind, s.rtt, remote, s.cwnd)
            }
            None => (TransportKind::Unknown, Duration::ZERO, String::new(), 0),
        };
        PathSnapshot {
            kind,
            rtt,
            remote,
            path_count,
            cwnd,
            lost_packets: stats.lost_packets,
            sent_packets: stats.udp_tx.datagrams,
            sent_bytes: stats.udp_tx.bytes,
            received_bytes: stats.udp_rx.bytes,
        }
    }

    /// Closes the connection immediately with an application error code.
    pub fn close(&self, code: u32, reason: &str) {
        self.conn.close(VarInt::from_u32(code), reason.as_bytes());
    }

    /// Resolves when the connection is closed, with a human-readable reason.
    pub async fn closed(&self) -> String {
        self.conn.closed().await.to_string()
    }

    pub fn is_closed(&self) -> bool {
        self.conn.close_reason().is_some()
    }
}

/// Sending half of a stream.
#[derive(Debug)]
pub struct SendStream(iroh::endpoint::SendStream);

impl SendStream {
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), StreamError> {
        self.0.write_all(buf).await.map_err(|e| StreamError::Other(e.to_string()))
    }

    /// Gracefully ends the stream after buffered data is delivered.
    pub fn finish(&mut self) -> Result<(), StreamError> {
        self.0.finish().map_err(|_| StreamError::Closed)
    }

    /// Abandons the stream; undelivered data is discarded (used to drop stale frames).
    pub fn reset(&mut self, code: u32) {
        let _ = self.0.reset(VarInt::from_u32(code));
    }

    /// Higher values are sent first when several streams compete for bandwidth.
    pub fn set_priority(&self, priority: i32) {
        let _ = self.0.set_priority(priority);
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.get_mut().0), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.get_mut().0), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.get_mut().0), cx)
    }
}

/// Receiving half of a stream.
#[derive(Debug)]
pub struct RecvStream(iroh::endpoint::RecvStream);

impl RecvStream {
    /// Reads until the peer finishes the stream, rejecting more than `limit` bytes.
    pub async fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, StreamError> {
        self.0.read_to_end(limit).await.map_err(|e| StreamError::Other(e.to_string()))
    }

    /// Tells the peer to stop sending on this stream.
    pub fn stop(&mut self, code: u32) {
        let _ = self.0.stop(VarInt::from_u32(code));
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.get_mut().0), cx, buf)
    }
}
