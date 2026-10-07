use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0,
                D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
            },
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice,
                ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                IDXGIAdapter,
            },
        },
    },
    core::Interface,
};

use super::displays::wide_to_string;
use crate::CaptureError;

/// A Direct3D 11 device bound to the GPU that scans out the captured display.
/// Shared with the encoder so frames never leave GPU memory.
#[derive(Clone)]
pub struct GpuDevice {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_name: String,
    pub vendor_id: u32,
    /// LUID of the adapter, used to pick the hardware encoder on the same GPU.
    pub adapter_luid: (u32, i32),
}

impl std::fmt::Debug for GpuDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuDevice")
            .field("adapter", &self.adapter_name)
            .field("vendor_id", &format_args!("{:#06x}", self.vendor_id))
            .finish()
    }
}

impl GpuDevice {
    pub fn create(adapter: &IDXGIAdapter) -> Result<Self, CaptureError> {
        let levels: [D3D_FEATURE_LEVEL; 3] =
            [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0];
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device: ID3D11Device =
            device.ok_or_else(|| CaptureError::Platform("no device".into()))?;
        let context = context.ok_or_else(|| CaptureError::Platform("no context".into()))?;
        // Media Foundation uses the device from its own threads.
        let mt: ID3D11Multithread = device.cast()?;
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
        let desc = unsafe { adapter.GetDesc()? };
        Ok(Self {
            device,
            context,
            adapter_name: wide_to_string(&desc.Description),
            vendor_id: desc.VendorId,
            adapter_luid: (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart),
        })
    }

    /// BGRA texture usable as copy destination, shader input and render target.
    pub fn create_bgra_texture(&self, width: u32, height: u32) -> Result<ID3D11Texture2D, CaptureError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut tex = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
        tex.ok_or_else(|| CaptureError::Platform("CreateTexture2D returned null".into()))
    }
}
