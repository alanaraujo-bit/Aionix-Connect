//! Screen and cursor capture.
//!
//! The Windows backend uses DXGI Desktop Duplication (see ADR-0004): frames stay on the
//! GPU as `ID3D11Texture2D` so the encoder can consume them without CPU copies, dirty
//! regions are reported by the OS, and the cursor is delivered separately so viewers can
//! render it locally at full rate.

#[cfg(windows)]
pub mod windows;

/// A monitor attached to the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Stable for the lifetime of the current display configuration.
    pub id: u32,
    /// Human name, e.g. `"DELL U2720Q"`, falling back to the GDI device name.
    pub name: String,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    /// Rotation in degrees (0, 90, 180, 270).
    pub rotation: u32,
}

/// Axis-aligned rectangle in display pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Cursor image in straight RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    /// Content hash, stable for identical shapes (lets viewers cache them).
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub rgba: Vec<u8>,
}

/// Latest cursor state relative to the captured display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPosition {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// Display configuration changed, desktop switched (UAC / lock screen) or the
    /// GPU was reset. The capturer must be recreated (after re-attaching to the
    /// current input desktop).
    #[error("capture access lost; recreate the capturer")]
    AccessLost,
    /// The process may not capture the current desktop with this backend.
    #[error("capture access denied")]
    AccessDenied,
    #[error("display {0} not found")]
    DisplayNotFound(u32),
    #[error("capture unsupported: {0}")]
    Unsupported(String),
    #[error("platform error: {0}")]
    Platform(String),
}

#[cfg(windows)]
impl From<::windows::core::Error> for CaptureError {
    fn from(e: ::windows::core::Error) -> Self {
        CaptureError::Platform(format!("{e} ({:#x})", e.code().0))
    }
}

/// FNV-1a 64-bit, used for cheap content ids (not security sensitive).
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in data {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
