//! Client side of the Aionix Connect Control Plane, used by the Agent, Quick Support
//! and Technician applications.
//!
//! * [`api`]: typed HTTP client for `/v1/...`.
//! * [`identity`]: the device's Ed25519 identity and registration, stored locally
//!   (DPAPI-protected on Windows). The private key never leaves the device.
//! * [`enroll`]: enrollment (proof of possession) and device login with cached,
//!   automatically renewed device tokens and relay grants.
//! * [`signaling`]: the permanent signaling link with heartbeat answers, presence
//!   updates, offer handling and reconnection with backoff.
//! * [`authority`]: user login with refresh, and [`authority::ControlPlaneAuthority`],
//!   the production `AuthorizationProvider` that replaces the development authority.
//! * [`host`]: an offer handler wiring signaling offers into the host's
//!   `TicketValidator` (session id pinning) from `aionix-remote-core`.

pub mod api;
pub mod authority;
pub mod enroll;
pub mod host;
pub mod identity;
pub mod signaling;

pub use api::{ApiClient, ClientError};
