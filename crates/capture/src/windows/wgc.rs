//! Windows.Graphics.Capture backend (Windows 10 1903+).
//!
//! Used when Desktop Duplication is unavailable to the process. Frames stay on the GPU.
//! The border highlight is disabled where the OS allows it (Windows 11); the cursor is
//! excluded from the image and tracked separately so viewers can draw it locally.

use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use tracing::{debug, info};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Graphics::{Direct3D11::ID3D11Texture2D, Dxgi::IDXGIDevice},
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RO_INIT_MULTITHREADED, RoInitialize,
        },
    },
    core::{IInspectable, Interface},
};

use super::{
    CapturedFrame, FrameEvent, cursor_poll::CursorTracker, device::GpuDevice,
    displays::DxgiDisplay,
};
use crate::CaptureError;

type Signal = Arc<(Mutex<bool>, Condvar)>;

pub struct WgcCapturer {
    gpu: GpuDevice,
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    copy: ID3D11Texture2D,
    width: u32,
    height: u32,
    signal: Signal,
    cursor: CursorTracker,
}

impl WgcCapturer {
    pub(crate) fn new(display: &DxgiDisplay) -> Result<Self, CaptureError> {
        unsafe {
            // Already-initialized apartments are fine.
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err(CaptureError::Unsupported("Windows.Graphics.Capture unavailable".into()));
        }
        let gpu = GpuDevice::create(&display.adapter.cast()?)?;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(display.monitor)? };
        let dxgi: IDXGIDevice = gpu.device.cast()?;
        let inspectable: IInspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
        let d3d: IDirect3DDevice = inspectable.cast()?;
        let size = item.Size()?;
        let (width, height) = (size.Width as u32, size.Height as u32);
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        if let Err(e) = session.SetIsBorderRequired(false) {
            debug!(error = %e, "capture border cannot be disabled on this Windows version");
        }
        let _ = session.SetIsCursorCaptureEnabled(false);

        let signal: Signal = Arc::new((Mutex::new(false), Condvar::new()));
        let s = signal.clone();
        pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
            let (lock, cv) = &*s;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_one();
            Ok(())
        }))?;
        session.StartCapture()?;
        let copy = gpu.create_bgra_texture(width, height)?;
        info!(width, height, adapter = %gpu.adapter_name, "graphics capture started");
        Ok(Self {
            gpu,
            _item: item,
            pool,
            session,
            copy,
            width,
            height,
            signal,
            cursor: CursorTracker::new((display.info.x, display.info.y)),
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
        let started = Instant::now();
        let deadline = started + Duration::from_millis(u64::from(timeout_ms));
        // Wake up regularly to sample the cursor (it is not part of the frame stream).
        let cursor_period = Duration::from_millis(8);
        loop {
            let (pos, shape) = self.cursor.poll();
            let mut frame = CapturedFrame::empty(started);
            frame.cursor_position = pos;
            frame.cursor_shape = shape;
            if self.take_signal(cursor_period.min(deadline.saturating_duration_since(Instant::now()))) {
                let copy_started = Instant::now();
                self.drain_latest(&mut frame)?;
                frame.captured_at = copy_started;
                frame.capture_us = copy_started.elapsed().as_micros() as u32;
            }
            if frame.texture.is_some() || frame.cursor_position.is_some() || frame.cursor_shape.is_some() {
                return Ok(FrameEvent::Captured(frame));
            }
            if Instant::now() >= deadline {
                return Ok(FrameEvent::Timeout);
            }
        }
    }

    fn take_signal(&self, wait: Duration) -> bool {
        let (lock, cv) = &*self.signal;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let (mut guard, _) = cv
            .wait_timeout_while(guard, wait, |ready| !*ready)
            .unwrap_or_else(|e| e.into_inner());
        std::mem::replace(&mut *guard, false)
    }

    /// Consumes every pending frame and keeps only the newest one.
    fn drain_latest(&mut self, frame: &mut CapturedFrame) -> Result<(), CaptureError> {
        while let Ok(f) = self.pool.TryGetNextFrame() {
            let size = f.ContentSize()?;
            if size.Width as u32 != self.width || size.Height as u32 != self.height {
                // Resolution changed: the caller recreates the capturer.
                let _ = f.Close();
                return Err(CaptureError::AccessLost);
            }
            let surface = f.Surface()?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            let tex: ID3D11Texture2D = unsafe { access.GetInterface()? };
            unsafe { self.gpu.context.CopyResource(&self.copy, &tex) };
            let _ = f.Close();
            frame.texture = Some(self.copy.clone());
        }
        Ok(())
    }
}

impl Drop for WgcCapturer {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
