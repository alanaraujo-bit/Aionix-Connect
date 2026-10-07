//! Signed token claim sets. Each has a distinct `TYPE` so tokens cannot be confused.

use aionix_crypto::{PublicKey, token::TokenClaims};
use aionix_shared::{AccessMode, DeviceKind};
use serde::{Deserialize, Serialize};
use time::Duration;
use uuid::Uuid;

/// Technician (user) API access. Short-lived; renewed with a rotating refresh token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessClaims {
    /// User id.
    pub sub: Uuid,
    /// Login session (refresh token family). Revoking it invalidates renewals.
    pub sid: Uuid,
}

impl TokenClaims for AccessClaims {
    const TYPE: &'static str = "access";
}

impl AccessClaims {
    pub const TTL: Duration = Duration::minutes(15);
}

/// Device API / signaling access, obtained by proving possession of the device key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceClaims {
    /// Device id.
    pub sub: Uuid,
    pub key: PublicKey,
    pub kind: DeviceKind,
}

impl TokenClaims for DeviceClaims {
    const TYPE: &'static str = "device";
}

impl DeviceClaims {
    pub const TTL: Duration = Duration::hours(1);
}

/// Grants an endpoint key the right to use the relay servers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelayClaims {
    /// Device id.
    pub sub: Uuid,
    /// Endpoint key allowed to connect (must match the relay handshake identity).
    pub key: PublicKey,
}

impl TokenClaims for RelayClaims {
    const TYPE: &'static str = "relay";
}

impl RelayClaims {
    pub const TTL: Duration = Duration::hours(12);
}

/// Who is on the other side of a session, as vouched for by the control plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TicketViewer {
    pub device_id: Uuid,
    pub key: PublicKey,
    pub user_id: Uuid,
    pub display_name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TicketHost {
    pub device_id: Uuid,
    pub key: PublicKey,
}

/// Capabilities granted within a session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionPermission {
    View,
    Control,
    Clipboard,
    FileTransfer,
    Restart,
}

/// Authorizes one viewer key to open one session to one host key.
///
/// The host verifies the signature with the control plane key it pinned at
/// enrollment, checks that the connecting peer's authenticated endpoint key equals
/// `viewer.key`, that `host.key` is its own key, and that `sid` was not used before.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTicketClaims {
    pub sid: Uuid,
    pub host: TicketHost,
    pub viewer: TicketViewer,
    pub mode: AccessMode,
    pub perms: Vec<SessionPermission>,
}

impl TokenClaims for SessionTicketClaims {
    const TYPE: &'static str = "session_ticket";
}

impl SessionTicketClaims {
    /// Window in which the viewer must start the connection.
    pub const TTL: Duration = Duration::seconds(90);
}
