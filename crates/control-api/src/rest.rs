//! HTTP API bodies. Paths are documented next to each type.

use aionix_crypto::PublicKey;
use aionix_shared::{AccessMode, DeviceKind, FriendlyId, TransportKind};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{claims::SessionPermission, signal::EndpointAddrDto};

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// `GET /v1/server/info` (public)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    /// Keys whose signatures on tokens and session tickets clients must accept.
    pub token_keys: Vec<PublicKey>,
    pub relay_urls: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub server_time: OffsetDateTime,
}

// ---------------------------------------------------------------------------
// User authentication
// ---------------------------------------------------------------------------

/// `POST /v1/auth/login`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// `POST /v1/auth/refresh`, `POST /v1/auth/logout`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// Response of login and refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Access token lifetime in seconds.
    pub expires_in: i64,
    pub user: UserProfile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OrgRole {
    /// Read-only access to the device inventory and history.
    Viewer,
    /// Can connect to the organization's devices.
    Technician,
    /// Manages devices, customers and technicians.
    Admin,
    /// Full control including billing and organization deletion.
    Owner,
}

impl OrgRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Technician => "technician",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "viewer" => Some(Self::Viewer),
            "technician" => Some(Self::Technician),
            "admin" => Some(Self::Admin),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }

    pub fn can_connect(self) -> bool {
        self >= Self::Technician
    }

    pub fn can_manage(self) -> bool {
        self >= Self::Admin
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Membership {
    pub org_id: Uuid,
    pub org_name: String,
    pub role: OrgRole,
}

/// `GET /v1/me`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub memberships: Vec<Membership>,
}

// ---------------------------------------------------------------------------
// Device identity
// ---------------------------------------------------------------------------

/// `POST /v1/devices/challenge` response. The nonce is single-use and short-lived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Challenge {
    pub nonce: String,
    pub expires_in: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceInfo {
    pub hostname: String,
    pub os_name: String,
    pub os_version: String,
    pub app_version: String,
    #[serde(default)]
    pub username: String,
}

/// `POST /v1/devices/register`
///
/// `signature` = domain-separated (DeviceRegistration) signature over the nonce bytes.
/// Agents must present an `enrollment_token`; technician workstations must send the
/// user's bearer access token; quick support registrations are open but rate limited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterDeviceRequest {
    pub public_key: PublicKey,
    pub kind: DeviceKind,
    pub nonce: String,
    pub signature: String,
    pub info: DeviceInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterDeviceResponse {
    pub device_id: Uuid,
    pub friendly_id: FriendlyId,
}

/// `POST /v1/devices/login`; `signature` uses the DeviceLogin domain over the nonce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceLoginRequest {
    pub public_key: PublicKey,
    pub nonce: String,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<DeviceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayGrant {
    pub url: String,
    pub token: String,
    pub expires_in: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceLoginResponse {
    pub device_id: Uuid,
    pub friendly_id: FriendlyId,
    pub device_token: String,
    pub expires_in: i64,
    pub relays: Vec<RelayGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub id: Uuid,
    pub friendly_id: FriendlyId,
    pub kind: DeviceKind,
    pub display_name: String,
    pub hostname: String,
    pub os_name: String,
    pub os_version: String,
    /// `presence == Online`; kept for older clients.
    pub online: bool,
    #[serde(default)]
    pub presence: PresenceState,
    pub unattended_enabled: bool,
    pub favorite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_name: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_session_at: Option<OffsetDateTime>,
}

/// `GET /v1/orgs/{org}/devices`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceList {
    pub devices: Vec<DeviceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Customer {
    pub id: Uuid,
    pub name: String,
    pub device_count: i64,
    pub online_count: i64,
}

/// `POST /v1/orgs/{org}/customers`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCustomerRequest {
    pub name: String,
}

/// `POST /v1/orgs/{org}/enrollment-tokens`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateEnrollmentTokenRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<Uuid>,
    /// Validity in hours (1..=720).
    pub valid_hours: i64,
    /// Number of agents that may enroll with this token (1..=1000).
    pub max_uses: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentToken {
    pub id: Uuid,
    /// Shown once. Only its hash is stored.
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub max_uses: i32,
}

/// `PATCH /v1/devices/{id}` (org admin) — organize a device.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateDeviceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// `Some(None)` clears the customer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<Option<Uuid>>,
}

/// `GET /v1/devices/lookup/{friendly_id}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceLookup {
    pub friendly_id: FriendlyId,
    pub kind: DeviceKind,
    pub online: bool,
    #[serde(default)]
    pub presence: PresenceState,
    /// Only disclosed when the caller belongs to the device's organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceSummary>,
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// `POST /v1/sessions` — requires the user's bearer token and the workstation's
/// device token in [`crate::DEVICE_TOKEN_HEADER`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub target: FriendlyId,
    /// Requested capabilities. The server grants at most what the technician's role
    /// allows; omitted = everything the role allows. `view` is always required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<SessionPermission>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostEndpoint {
    pub device_id: Uuid,
    pub key: PublicKey,
    pub addr: EndpointAddrDto,
    pub display_name: String,
    pub kind: DeviceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    pub session_id: Uuid,
    pub ticket: String,
    pub mode: AccessMode,
    pub host: HostEndpoint,
    /// Permissions granted by the ticket.
    #[serde(default)]
    pub permissions: Vec<SessionPermission>,
    /// The ticket must be presented to the host before this instant.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

/// Truthful device reachability as seen by the signaling service.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    /// Authenticated signaling link alive, reachability published, heartbeat on time.
    Online,
    /// Link authenticated but reachability not published yet, or the link was lost
    /// unexpectedly moments ago and the device is expected to reconnect.
    Connecting,
    /// No signaling link.
    #[default]
    Offline,
}

impl PresenceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Connecting => "connecting",
            Self::Offline => "offline",
        }
    }
}

// ---------------------------------------------------------------------------
// Organization administration
// ---------------------------------------------------------------------------

/// `GET /v1/orgs/{org}/members`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub user_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub role: OrgRole,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberList {
    pub members: Vec<Member>,
}

/// `PUT /v1/orgs/{org}/members` — add an existing account or change its role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertMemberRequest {
    pub email: String,
    pub role: OrgRole,
}

/// `POST /v1/devices/{id}/revoke`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RevokeDeviceRequest {
    #[serde(default)]
    pub reason: String,
}

/// One audit/security record (`GET /v1/orgs/{org}/audit`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    /// `audit` or `security`.
    pub category: String,
    pub action: String,
    /// `success`, `denied` or `failure`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_user_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_device_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default)]
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditList {
    pub events: Vec<AuditEvent>,
}

/// `POST /v1/sessions/{id}/events` — viewer-side lifecycle reports for history/audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEventReport {
    pub kind: SessionEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportKind>,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    Connected,
    AuthGranted,
    AuthDenied,
    TransportChanged,
    Reconnecting,
    Reconnected,
    Ended,
    Failed,
}

impl SessionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::AuthGranted => "auth_granted",
            Self::AuthDenied => "auth_denied",
            Self::TransportChanged => "transport_changed",
            Self::Reconnecting => "reconnecting",
            Self::Reconnected => "reconnected",
            Self::Ended => "ended",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: Uuid,
    pub host_device_id: Uuid,
    pub host_name: String,
    pub host_friendly_id: FriendlyId,
    pub technician_name: String,
    pub mode: AccessMode,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportKind>,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub connected_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
}

/// `GET /v1/sessions`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionRecord>,
}
