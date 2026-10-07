//! Aionix Connect Control Plane.
//!
//! The business side of the server: accounts and authentication, organizations,
//! roles and permissions, device enrollment and identity, presence, session
//! authorization and ticket issuance, session metadata and audit. It never carries
//! remote desktop data: video, input and clipboard travel end-to-end encrypted between
//! the peers (directly or through `aionix-relay`).
//!
//! Binaries: `aionix-api` (REST API) and `aionix-admin` (operator CLI). The signaling
//! service (`aionix-signaling`) reuses this crate's store, presence and bus modules.
//!
//! * PostgreSQL ([`db`]) is the source of truth.
//! * Redis carries ephemeral state: [`presence`], cross-service messages ([`bus`]) and
//!   shared rate-limit counters ([`ratelimit`]).

pub mod admin;
pub mod api;
pub mod audit;
pub mod authz;
pub mod bus;
pub mod config;
pub mod db;
pub mod devices;
pub mod error;
pub mod janitor;
pub mod password;
pub mod presence;
pub mod ratelimit;
pub mod redis_conn;
pub mod server;
pub mod sessions;
pub mod state;
pub mod tokens;

#[cfg(feature = "test-support")]
pub mod testing;

pub use error::ApiFailure;
pub use state::AppState;
