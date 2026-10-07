//! Technician-side access to the session authority.
//!
//! The authority (the future PostgreSQL-backed Control Plane) decides whether a
//! technician may open a session to a device and, if so, issues a short-lived signed
//! session ticket plus the information needed to reach the host. The coordinators only
//! see the [`AuthorizationProvider`] trait, so the Control Plane client can replace the
//! development provider without touching session code.

use aionix_control_api::{claims::SessionPermission, signal::EndpointAddrDto};
use aionix_crypto::PublicKey;
use aionix_shared::{AccessMode, FriendlyId};
use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

/// Who is asking. In production the Control Plane derives this from the technician's
/// authenticated API session, never from client-supplied fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TechnicianIdentity {
    pub user_id: Uuid,
    /// Technician workstation device id.
    pub device_id: Uuid,
    /// Workstation endpoint key; the host checks the transport peer against it.
    pub key: PublicKey,
    pub display_name: String,
    pub email: String,
    pub org_name: Option<String>,
}

/// How the technician designates the target.
///
/// A [`FriendlyId`] is only a lookup handle typed by a human: the authority resolves it
/// to a device id and that device's public key, and the ticket binds those. Knowing a
/// friendly id grants nothing by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceLookup {
    Id(Uuid),
    Friendly(FriendlyId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRequest {
    pub technician: TechnicianIdentity,
    pub target: DeviceLookup,
    pub mode: AccessMode,
    pub permissions: Vec<SessionPermission>,
}

/// A granted request: everything needed to contact the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGrant {
    pub session_id: Uuid,
    /// Signed `session_ticket` token presented to the host in `Hello`.
    pub ticket: String,
    pub expires_at: OffsetDateTime,
    pub host_device_id: Uuid,
    /// The connection is only accepted if the transport authenticates this key.
    pub host_key: PublicKey,
    pub host_addr: EndpointAddrDto,
    pub permissions: Vec<SessionPermission>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityError {
    #[error("device not found")]
    DeviceNotFound,
    #[error("device offline")]
    DeviceOffline,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("authority unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait AuthorizationProvider: Send + Sync {
    async fn request_session(
        &self,
        request: SessionRequest,
    ) -> Result<SessionGrant, AuthorityError>;
}
