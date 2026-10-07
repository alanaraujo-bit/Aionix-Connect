//! Control plane contract shared by the server and every client.
//!
//! * [`claims`]: claim sets carried by signed tokens.
//! * [`rest`]: request/response bodies of the HTTP API (`/v1/...`).
//! * [`signal`]: messages exchanged on the signaling WebSocket (`/v1/signal`).
//! * [`ApiError`]: uniform error body. `code` is a stable machine identifier; user-facing
//!   text is produced by the clients' localization layer, never by the server.

pub mod claims;
pub mod rest;
pub mod signal;

use serde::{Deserialize, Serialize};

/// Uniform error body for every non-2xx response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiError {
    pub code: ErrorCode,
    /// Developer-oriented English detail. Never shown verbatim to end users.
    #[serde(default)]
    pub detail: String,
    /// Seconds the client should wait before retrying (rate limits, lockouts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    InvalidCredentials,
    AccountLocked,
    TokenExpired,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    DeviceOffline,
    DeviceBusy,
    DeviceRevoked,
    DeviceRejected,
    DeviceTimeout,
    EnrollmentInvalid,
    InvalidSignature,
    ChallengeExpired,
    Internal,
    Unavailable,
}

/// HTTP header carrying the technician workstation's device token alongside the
/// user's bearer access token.
pub const DEVICE_TOKEN_HEADER: &str = "x-aionix-device-token";
