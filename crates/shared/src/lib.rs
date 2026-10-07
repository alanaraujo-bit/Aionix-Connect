//! Domain types shared by clients, agents and server components.

mod friendly_id;
mod modes;

pub use friendly_id::{FriendlyId, FriendlyIdError};
pub use modes::{AccessMode, DeviceKind, QualityMode, TransportKind};

/// Product version reported in handshakes and registrations.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
