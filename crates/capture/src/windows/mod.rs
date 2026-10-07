//! Windows capture backends.
//!
//! [`ScreenCapturer::open`] tries, in order:
//! 1. DXGI Desktop Duplication — fastest, dirty rects, secure desktop when SYSTEM.
//! 2. Windows.Graphics.Capture — GPU frames when duplication is refused.
//! 3. GDI — universal fallback.
//!
//! All backends produce a BGRA `ID3D11Texture2D` on the same [`GpuDevice`], so the
//! encoder pipeline does not care which one is active.

mod cursor;
mod cursor_poll;
mod device;
mod displays;
mod dxgi;
mod gdi;
mod wgc;

use std::time::Instant;

use tracing::warn;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

pub use device::GpuDevice;
pub use displays::enumerate_displays;
pub use dxgi::DxgiCapturer;
pub use gdi::GdiCapturer;
pub use wgc::WgcCapturer;

use crate::{CaptureError, CursorPosition, CursorShape, DisplayInfo, Rect};

/// Result of waiting for the next frame.
pub enum FrameEvent {
    /// Something changed (image and/or cursor).
    Captured(CapturedFrame),
    /// Nothing changed within the timeout.
    Timeout,
}

pub struct CapturedFrame {
    /// Latest full desktop image. Valid until the next `next_frame` call.
    /// `None` when only the cursor changed.
    pub texture: Option<ID3D11Texture2D>,
    /// Changed regions when the backend reports them (empty = unknown / whole frame).
    pub dirty: Vec<Rect>,
    pub captured_at: Instant,
    pub capture_us: u32,
    pub cursor_position: Option<CursorPosition>,
    pub cursor_shape: Option<CursorShape>,
}

impl CapturedFrame {
    pub(crate) fn empty(captured_at: Instant) -> Self {
        Self {
            texture: None,
            dirty: Vec::new(),
            captured_at,
            capture_us: 0,
            cursor_position: None,
            cursor_shape: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    DesktopDuplication,
    GraphicsCapture,
    Gdi,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::DesktopDuplication => "DXGI Desktop Duplication",
            Self::GraphicsCapture => "Windows.Graphics.Capture",
            Self::Gdi => "GDI",
        }
    }
}

pub enum ScreenCapturer {
    Dxgi(DxgiCapturer),
    Wgc(WgcCapturer),
    Gdi(GdiCapturer),
}

impl ScreenCapturer {
    /// Opens the best available backend for `display_id`. `preferred` skips backends
    /// before it (useful after a backend failed repeatedly).
    pub fn open(
        display_id: u32,
        preferred: Option<Backend>,
    ) -> Result<(Self, DisplayInfo), CaptureError> {
        let display = displays::enumerate_dxgi()?
            .into_iter()
            .find(|d| d.info.id == display_id)
            .ok_or(CaptureError::DisplayNotFound(display_id))?;
        let order: &[Backend] = match preferred {
            None | Some(Backend::DesktopDuplication) => &[
                Backend::DesktopDuplication,
                Backend::GraphicsCapture,
                Backend::Gdi,
            ],
            Some(Backend::GraphicsCapture) => &[Backend::GraphicsCapture, Backend::Gdi],
            Some(Backend::Gdi) => &[Backend::Gdi],
        };
        let mut last_err = None;
        for backend in order {
            let result = match backend {
                Backend::DesktopDuplication => DxgiCapturer::new(&display).map(Self::Dxgi),
                Backend::GraphicsCapture => WgcCapturer::new(&display).map(Self::Wgc),
                Backend::Gdi => GdiCapturer::new(&display).map(Self::Gdi),
            };
            match result {
                Ok(c) => return Ok((c, display.info)),
                // Secure desktop transitions must be handled by the caller (re-attach
                // to the input desktop); other backends would fail the same way.
                Err(CaptureError::AccessLost) => return Err(CaptureError::AccessLost),
                Err(e) => {
                    warn!(backend = backend.name(), error = %e, "capture backend unavailable");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or(CaptureError::Unsupported("no capture backend".into())))
    }

    pub fn backend(&self) -> Backend {
        match self {
            Self::Dxgi(_) => Backend::DesktopDuplication,
            Self::Wgc(_) => Backend::GraphicsCapture,
            Self::Gdi(_) => Backend::Gdi,
        }
    }

    pub fn gpu(&self) -> &GpuDevice {
        match self {
            Self::Dxgi(c) => c.gpu(),
            Self::Wgc(c) => c.gpu(),
            Self::Gdi(c) => c.gpu(),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        match self {
            Self::Dxgi(c) => c.size(),
            Self::Wgc(c) => c.size(),
            Self::Gdi(c) => c.size(),
        }
    }

    /// The most recent full image, for re-encoding (keyframe requests, quality
    /// changes) without waiting for the screen to change.
    pub fn last_texture(&self) -> ID3D11Texture2D {
        match self {
            Self::Dxgi(c) => c.last_texture(),
            Self::Wgc(c) => c.last_texture(),
            Self::Gdi(c) => c.last_texture(),
        }
    }

    /// Waits up to `timeout_ms` for the image or cursor to change.
    pub fn next_frame(&mut self, timeout_ms: u32) -> Result<FrameEvent, CaptureError> {
        match self {
            Self::Dxgi(c) => c.next_frame(timeout_ms),
            Self::Wgc(c) => c.next_frame(timeout_ms),
            Self::Gdi(c) => c.next_frame(timeout_ms),
        }
    }
}
