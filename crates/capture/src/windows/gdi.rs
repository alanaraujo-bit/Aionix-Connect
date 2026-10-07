//! GDI fallback backend.
//!
//! Works everywhere a desktop exists (including restricted environments where both
//! Desktop Duplication and Windows.Graphics.Capture are refused), at a higher CPU cost.
//! The screen is sampled at a fixed cadence and only changed images are reported; each
//! image is uploaded to a GPU texture so the rest of the pipeline is identical.

use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

use tracing::info;
use windows::{
    Win32::Graphics::{
        Direct3D11::ID3D11Texture2D,
        Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP, HDC,
            HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
        },
    },
    core::Interface,
};

use super::{
    CapturedFrame, FrameEvent, cursor_poll::CursorTracker, device::GpuDevice,
    displays::DxgiDisplay,
};
use crate::CaptureError;

/// Sampling period (about 30 images per second).
const SAMPLE_PERIOD: Duration = Duration::from_millis(33);

pub struct GdiCapturer {
    gpu: GpuDevice,
    copy: ID3D11Texture2D,
    origin: (i32, i32),
    width: u32,
    height: u32,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_object: HGDIOBJ,
    bits: *mut u8,
    previous: Vec<u8>,
    last_sample: Option<Instant>,
    cursor: CursorTracker,
}

// The raw pointers are owned exclusively by this struct and only used on the capture
// thread that drives it.
unsafe impl Send for GdiCapturer {}

impl GdiCapturer {
    pub(crate) fn new(display: &DxgiDisplay) -> Result<Self, CaptureError> {
        let gpu = GpuDevice::create(&display.adapter.cast()?)?;
        let (width, height) = (display.info.width, display.info.height);
        let copy = gpu.create_bgra_texture(width, height)?;
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let screen = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen));
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(screen), &bi, DIB_RGB_COLORS, &mut bits, None, 0);
            ReleaseDC(None, screen);
            let bitmap = bitmap.map_err(|e| CaptureError::Platform(format!("CreateDIBSection: {e}")))?;
            let old_object = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
            info!(width, height, "GDI capture started");
            Ok(Self {
                gpu,
                copy,
                origin: (display.info.x, display.info.y),
                width,
                height,
                mem_dc,
                bitmap,
                old_object,
                bits: bits.cast(),
                previous: Vec::new(),
                last_sample: None,
                cursor: CursorTracker::new((display.info.x, display.info.y)),
            })
        }
    }

    pub fn gpu(&self) -> &GpuDevice {
        &self.gpu
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn last_texture(&self) -> ID3D11Texture2D {
        self.copy.clone()
    }

    pub fn next_frame(&mut self, timeout_ms: u32) -> Result<FrameEvent, CaptureError> {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(u64::from(timeout_ms));
        loop {
            let mut frame = CapturedFrame::empty(started);
            let (pos, shape) = self.cursor.poll();
            frame.cursor_position = pos;
            frame.cursor_shape = shape;

            let due = self.last_sample.is_none_or(|t| t.elapsed() >= SAMPLE_PERIOD);
            if due {
                let sample_started = Instant::now();
                self.last_sample = Some(sample_started);
                if self.sample()? {
                    frame.texture = Some(self.copy.clone());
                    frame.captured_at = sample_started;
                    frame.capture_us = sample_started.elapsed().as_micros() as u32;
                }
            }
            if frame.texture.is_some() || frame.cursor_position.is_some() || frame.cursor_shape.is_some() {
                return Ok(FrameEvent::Captured(frame));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(FrameEvent::Timeout);
            }
            std::thread::sleep(Duration::from_millis(8).min(deadline - now));
        }
    }

    /// Copies the screen; returns whether the image changed.
    fn sample(&mut self) -> Result<bool, CaptureError> {
        unsafe {
            let screen = GetDC(None);
            let ok = BitBlt(
                self.mem_dc,
                0,
                0,
                self.width as i32,
                self.height as i32,
                Some(screen),
                self.origin.0,
                self.origin.1,
                SRCCOPY | CAPTUREBLT,
            );
            ReleaseDC(None, screen);
            if ok.is_err() {
                // Typically the secure desktop became active.
                return Err(CaptureError::AccessLost);
            }
        }
        let len = (self.width * self.height * 4) as usize;
        let current = unsafe { std::slice::from_raw_parts(self.bits, len) };
        if self.previous.len() == len && self.previous.as_slice() == current {
            return Ok(false);
        }
        self.previous.clear();
        self.previous.extend_from_slice(current);
        unsafe {
            self.gpu.context.UpdateSubresource(
                &self.copy,
                0,
                None,
                self.previous.as_ptr().cast(),
                self.width * 4,
                0,
            );
        }
        Ok(true)
    }
}

impl Drop for GdiCapturer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old_object);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.mem_dc);
        }
    }
}
