//! Signaling WebSocket (`GET /v1/signal`, bearer device token).
//!
//! JSON text frames, each an object tagged by `"type"`. Hosts keep this socket open
//! permanently: it carries presence, session offers and revocations. It never carries
//! screen content or input.

use aionix_crypto::PublicKey;
use aionix_shared::{AccessMode, FriendlyId, TransportKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Transport-agnostic description of how to reach an endpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EndpointAddrDto {
    #[serde(default)]
    pub relay_urls: Vec<String>,
    /// `ip:port` socket addresses (LAN and public candidates).
    #[serde(default)]
    pub direct_addrs: Vec<String>,
}

/// Messages sent by a device to the control plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Current reachability and local policy. Sent on connect and on change.
    Presence {
        addr: EndpointAddrDto,
        unattended_enabled: bool,
        busy: bool,
    },
    /// Answer to [`ServerMessage::SessionOffer`].
    SessionOfferResponse {
        session_id: Uuid,
        accept: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<OfferRejectReason>,
        #[serde(default)]
        addr: EndpointAddrDto,
    },
    /// Host-side lifecycle report used for history and audit.
    SessionEvent {
        session_id: Uuid,
        kind: crate::rest::SessionEventKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transport: Option<TransportKind>,
        #[serde(default)]
        detail: String,
    },
    Pong {
        id: u64,
    },
    /// Orderly disconnect (application exit): the device is offline immediately
    /// instead of "connecting" for the reconnect grace period.
    Goodbye,
}

/// WebSocket close codes used by the signaling service.
pub mod close_code {
    /// No message within the heartbeat timeout.
    pub const HEARTBEAT_TIMEOUT: u16 = 4000;
    /// The device token expired; log in again and reconnect.
    pub const TOKEN_EXPIRED: u16 = 4001;
    /// A newer connection of the same device replaced this one.
    pub const SUPERSEDED: u16 = 4002;
    /// The device identity was revoked. Do not reconnect.
    pub const DEVICE_REVOKED: u16 = 4003;
    /// Protocol violation or message rate exceeded.
    pub const POLICY: u16 = 4008;
    /// The server is restarting; reconnect with backoff.
    pub const SERVICE_RESTART: u16 = 1012;
}

/// `GET /v1/signal` must be retried after this many seconds at most (clients add
/// jitter). Exposed so client and server agree on reconnect pacing.
pub const MAX_RECONNECT_BACKOFF_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OfferRejectReason {
    Busy,
    Policy,
    ShuttingDown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OfferViewer {
    pub display_name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    pub key: PublicKey,
}

/// Messages sent by the control plane to a device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready {
        device_id: Uuid,
        friendly_id: FriendlyId,
        /// Keepalive interval the client should expect (seconds).
        ping_interval: u64,
    },
    /// A technician wants to connect. The host answers with its current address and
    /// then waits for the incoming connection carrying `ticket`.
    SessionOffer {
        session_id: Uuid,
        ticket: String,
        mode: AccessMode,
        viewer: Box<OfferViewer>,
    },
    /// An active or pending session must be terminated (revoked by an admin, the
    /// technician account was disabled, ...).
    SessionRevoked {
        session_id: Uuid,
    },
    /// This device identity was revoked. The client must stop using it.
    DeviceRevoked,
    Ping {
        id: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_json_shape_is_stable() {
        let m = ClientMessage::Pong { id: 3 };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"type":"pong","id":3}"#
        );
        let s: ServerMessage = serde_json::from_str(r#"{"type":"ping","id":9}"#).unwrap();
        assert_eq!(s, ServerMessage::Ping { id: 9 });
        let r: ServerMessage = serde_json::from_str(r#"{"type":"device_revoked"}"#).unwrap();
        assert_eq!(r, ServerMessage::DeviceRevoked);
    }
}
