//! DXGI Desktop Duplication backend: lowest overhead, dirty rects, hardware cursor
//! information, and (when running as SYSTEM inside the session) the secure desktop.

use std::time::Instant;

use tracing::{debug, info, warn};
use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, RECT},
        Graphics::{
            Direct3D11::{ID3D11Device, ID3D11Texture2D},
            Dxgi::{
                Common::DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ERROR_ACCESS_LOST,
                DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_INVALID_CALL,
                DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT,
                DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO, IDXGIOutput,
                IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource,
            },
        },
    },
    core::Interface,
};

use super::{CapturedFrame, FrameEvent, cursor, device::GpuDevice, displays::DxgiDisplay};
use crate::{CaptureError, CursorPosition, CursorShape, Rect};

pub struct DxgiCapturer {
    gpu: GpuDevice,
    duplication: IDXGIOutputDuplication,
    copy: ID3D11Texture2D,
    width: u32,
    height: u32,
    holding_frame: bool,
    shape_buf: Vec<u8>,
    rect_buf: Vec<RECT>,
}

impl DxgiCapturer {
    pub(crate) fn new(display: &DxgiDisplay) -> Result<Self, CaptureError> {
        let gpu = GpuDevice::create(&display.adapter.cast()?)?;
        let duplication = duplicate(&display.output, &gpu.device).map_err(|e| {
            let code = e.code();
            if code == E_ACCESSDENIED || code == DXGI_ERROR_SESSION_DISCONNECTED {
                debug!(?code, "DuplicateOutput denied");
                CaptureError::AccessDenied
            } else if code == DXGI_ERROR_UNSUPPORTED {
                CaptureError::Unsupported(
                    "desktop duplication not supported on this adapter".into(),
                )
            } else {
                e.into()
            }
        })?;
        let dd = unsafe { duplication.GetDesc() };
        let (width, height) = (dd.ModeDesc.Width, dd.ModeDesc.Height);
        let copy = gpu.create_bgra_texture(width, height)?;
        info!(width, height, adapter = %gpu.adapter_name, "desktop duplication started");
        Ok(Self {
            gpu,
            duplication,
            copy,
            width,
            height,
            holding_frame: false,
            shape_buf: Vec::new(),
            rect_buf: Vec::new(),
        })
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
        self.release_held_frame();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        if let Err(e) = unsafe {
            self.duplication
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
        } {
            let code = e.code();
            if code == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(FrameEvent::Timeout);
            }
            if code == DXGI_ERROR_ACCESS_LOST
                || code == DXGI_ERROR_INVALID_CALL
                || code == DXGI_ERROR_DEVICE_REMOVED
                || code == DXGI_ERROR_DEVICE_RESET
            {
                debug!(?code, "duplication access lost");
                return Err(CaptureError::AccessLost);
            }
            return Err(e.into());
        }
        self.holding_frame = true;
        let started = Instant::now();
        let mut frame = CapturedFrame::empty(started);

        if (info.LastPresentTime != 0 || info.AccumulatedFrames > 0)
            && let Some(resource) = resource.as_ref()
        {
            let tex: ID3D11Texture2D = resource.cast()?;
            unsafe { self.gpu.context.CopyResource(&self.copy, &tex) };
            frame.texture = Some(self.copy.clone());
            frame.dirty = self.dirty_rects(info.TotalMetadataBufferSize);
        }
        if info.LastMouseUpdateTime != 0 {
            frame.cursor_position = Some(CursorPosition {
                x: info.PointerPosition.Position.x,
                y: info.PointerPosition.Position.y,
                visible: info.PointerPosition.Visible.as_bool(),
            });
        }
        if info.PointerShapeBufferSize > 0 {
            frame.cursor_shape = self.pointer_shape(info.PointerShapeBufferSize);
        }

        // Release as soon as possible so the compositor is never blocked on us.
        self.release_held_frame();
        frame.capture_us = started.elapsed().as_micros() as u32;
        Ok(FrameEvent::Captured(frame))
    }

    fn release_held_frame(&mut self) {
        if self.holding_frame {
            unsafe {
                let _ = self.duplication.ReleaseFrame();
            }
            self.holding_frame = false;
        }
    }

    fn dirty_rects(&mut self, metadata_size: u32) -> Vec<Rect> {
        if metadata_size == 0 {
            return Vec::new();
        }
        let capacity = metadata_size as usize / std::mem::size_of::<RECT>() + 1;
        self.rect_buf.resize(capacity, RECT::default());
        let mut required = 0u32;
        let ok = unsafe {
            self.duplication.GetFrameDirtyRects(
                (self.rect_buf.len() * std::mem::size_of::<RECT>()) as u32,
                self.rect_buf.as_mut_ptr(),
                &mut required,
            )
        };
        if ok.is_err() {
            return Vec::new();
        }
        let n = (required as usize / std::mem::size_of::<RECT>()).min(self.rect_buf.len());
        self.rect_buf[..n]
            .iter()
            .map(|r| Rect {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            })
            .collect()
    }

    fn pointer_shape(&mut self, size: u32) -> Option<CursorShape> {
        self.shape_buf.resize(size as usize, 0);
        let mut required = 0u32;
        let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        let res = unsafe {
            self.duplication.GetFramePointerShape(
                size,
                self.shape_buf.as_mut_ptr().cast(),
                &mut required,
                &mut shape,
            )
        };
        if let Err(e) = res {
            warn!(error = %e, "failed to read pointer shape");
            return None;
        }
        cursor::convert_shape(
            shape.Type,
            shape.Width,
            shape.Height,
            shape.Pitch,
            (shape.HotSpot.x.max(0) as u32, shape.HotSpot.y.max(0) as u32),
            &self.shape_buf[..(required as usize).min(self.shape_buf.len())],
        )
    }
}

impl Drop for DxgiCapturer {
    fn drop(&mut self) {
        self.release_held_frame();
    }
}

/// Prefers `DuplicateOutput1` (Windows 10 1703+, required for per-monitor DPI aware
/// processes) and falls back to `DuplicateOutput`.
fn duplicate(
    output: &IDXGIOutput,
    device: &ID3D11Device,
) -> windows::core::Result<IDXGIOutputDuplication> {
    if let Ok(output5) = output.cast::<IDXGIOutput5>() {
        let formats = [DXGI_FORMAT_B8G8R8A8_UNORM];
        match unsafe { output5.DuplicateOutput1(device, 0, &formats) } {
            Ok(d) => return Ok(d),
            Err(e) => debug!(code = ?e.code(), "DuplicateOutput1 failed, trying DuplicateOutput"),
        }
    }
    let output1: IDXGIOutput1 = output.cast()?;
    unsafe { output1.DuplicateOutput(device) }
}
