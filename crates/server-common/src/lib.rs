//! Plumbing shared by every Aionix Connect server process (`aionix-api`,
//! `aionix-signaling`, `aionix-relay`, `aionix-admin`).
//!
//! * [`config`]: environment-based configuration with validation that reports every
//!   missing or invalid variable at once and never echoes secret values.
//! * [`keys`]: loading the token signing key and the trusted token public keys.
//! * [`telemetry`]: structured (JSON) logging and the Prometheus metrics recorder.
//! * [`health`]: liveness (`/healthz`) vs readiness (`/readyz`) endpoints.
//! * [`http`]: request id + tracing layers, client IP resolution behind the reverse proxy.
//! * [`ratelimit`]: in-process fixed-window limiter.
//! * [`shutdown`]: SIGTERM / Ctrl+C handling for graceful shutdown.

pub mod config;
pub mod health;
pub mod http;
pub mod keys;
pub mod ratelimit;
pub mod shutdown;
pub mod telemetry;

/// Version of the server build, reported by `/healthz` and `/v1/server/info`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
