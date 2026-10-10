//! Aionix Connect desktop applications (Windows).
//!
//! * `aionix-connect` — "Aionix Connect": one app for both sides of remote support.
//!   [`host_side`] keeps this computer reachable by its 9-digit ID (enrollment,
//!   signaling, consent prompt, capture/H.264/input/clipboard via the Remote Core);
//!   [`tech_side`] signs in once and connects to other computers (devices with live
//!   presence, session tickets from the Control Plane, the remote viewer).
//! * `aionix-launcher` — development launcher: starts the server stack, prepares test
//!   data and opens the app.
//!
//! Nothing here authorizes anything by itself: tickets come from the Control Plane
//! (`ControlPlaneAuthority`) and the host user's visible approval is always required.

pub mod common;
#[cfg(windows)]
pub mod host_side;
pub mod icon;
pub mod icons;
pub mod keymap;
#[cfg(windows)]
pub mod tech_side;
pub mod theme;
pub mod updater;
pub mod video;
pub mod views;
