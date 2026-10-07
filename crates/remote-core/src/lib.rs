//! Remote session engine.
//!
//! * [`session`]: consent-based session foundation (state machine, ticket
//!   authorization, visible host approval, host and technician coordinators, media /
//!   input / clipboard wiring, reconnection, diagnostics).
//! * [`adaptive`]: adaptive quality controller driven by the host coordinator.

pub mod adaptive;
pub mod session;
