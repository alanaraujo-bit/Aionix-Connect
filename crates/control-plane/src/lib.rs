//! Aionix Connect Control Plane (work in progress).
//!
//! The business side of the server: accounts and authentication, organizations,
//! roles and permissions, device enrollment and identity, presence, session
//! authorization and ticket issuance, session metadata and audit. It never carries
//! remote desktop data.
//!
//! * PostgreSQL ([`db`], `migrations/`) is the source of truth.
//! * Redis ([`redis_conn`]) carries ephemeral state: presence, cross-service messages
//!   and shared rate-limit counters.
//!
//! Still to be written: `api`, `audit`, `authz`, `bus`, `presence`, `ratelimit`,
//! `sessions`, `janitor`, `admin`, `state`, `testing` and the binaries.

pub mod db;
pub mod error;
pub mod password;
pub mod redis_conn;
pub mod tokens;

pub use error::ApiFailure;
