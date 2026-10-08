//! Aionix Connect desktop applications (Windows).
//!
//! * `aionix-host` — "Aionix Connect": the computer that receives support. Enrolls its
//!   device identity, stays reachable through signaling, shows the consent prompt and
//!   serves the session (capture, H.264, input, clipboard) with the Remote Core.
//! * `aionix-technician` — "Aionix Connect Técnico": signs in, lists the organization's
//!   devices with live presence, requests sessions through the Control Plane and shows
//!   the remote screen.
//! * `aionix-launcher` — development launcher: starts the server stack, prepares test
//!   data and opens both applications.
//!
//! Nothing here authorizes anything by itself: tickets come from the Control Plane
//! (`ControlPlaneAuthority`) and the host user's visible approval is always required.

pub mod common;
pub mod icon;
pub mod keymap;
pub mod theme;
pub mod video;
