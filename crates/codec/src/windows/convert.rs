//! GPU color conversion and scaling (BGRA desktop → NV12 encoder input) using the
//! D3D11 video processor, which runs on the fixed-function video engine of every GPU.

use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
                D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
                D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
                D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
                D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
                ID3D11VideoContext, ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor,
                ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
                ID3D11VideoProcessorOutputView,
            },
            Dxgi::Common::{
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT_NV12, DXGI_RATIONAL,
                DXGI_SAMPLE_DESC,
            },
        },
    },
    core::Interface,
};

use crate::CodecError;

/// Number of output textures rotated through. Asynchronous hardware encoders may still
/// be reading frame N while frame N+1 is converted.
const RING: usize = 4;

pub struct VideoConverter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    outputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    /// Input view cache keyed by texture pointer (capture textures are long-lived).
    input: Option<(usize, ID3D11VideoProcessorInputView)>,
    pub in_size: (u32, u32),
    pub out_size: (u32, u32),
}

impl VideoConverter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        in_size: (u32, u32),
        out_size: (u32, u32),
    ) -> Result<Self, CodecError> {
        if !out_size.0.is_multiple_of(2)
            || !out_size.1.is_multiple_of(2)
            || out_size.0 == 0
            || out_size.1 == 0
        {
            return Err(CodecError::Unsupported(format!(
                "odd output size {out_size:?}"
            )));
        }
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: rate,
            OutputWidth: out_size.0,
            OutputHeight: out_size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&desc)? };
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0)? };

        unsafe {
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            let src = RECT {
                left: 0,
                top: 0,
                right: in_size.0 as i32,
                bottom: in_size.1 as i32,
            };
            let dst = RECT {
                left: 0,
                top: 0,
                right: out_size.0 as i32,
                bottom: out_size.1 as i32,
            };
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst));
            // Desktop is full-range sRGB; encoders expect BT.709 limited range.
            if let Ok(vc1) = video_context.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }
        }

        let mut outputs = Vec::with_capacity(RING);
        for _ in 0..RING {
            let tex = create_nv12(device, out_size, false)?;
            let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut view = None;
            unsafe {
                video_device.CreateVideoProcessorOutputView(
                    &tex,
                    &enumerator,
                    &view_desc,
                    Some(&mut view),
                )?
            };
            outputs.push((
                tex,
                view.ok_or_else(|| CodecError::Platform("null output view".into()))?,
            ));
        }

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            video_device,
            video_context,
            enumerator,
            processor,
            outputs,
            next: 0,
            input: None,
            in_size,
            out_size,
        })
    }

    /// Converts (and scales) `bgra` into the next NV12 texture of the ring.
    pub fn convert(&mut self, bgra: &ID3D11Texture2D) -> Result<ID3D11Texture2D, CodecError> {
        let key = bgra.as_raw() as usize;
        if self.input.as_ref().is_none_or(|(k, _)| *k != key) {
            let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut view = None;
            unsafe {
                self.video_device.CreateVideoProcessorInputView(
                    bgra,
                    &self.enumerator,
                    &desc,
                    Some(&mut view),
                )?
            };
            self.input = Some((
                key,
                view.ok_or_else(|| CodecError::Platform("null input view".into()))?,
            ));
        }
        let input_view = self.input.as_ref().map(|(_, v)| v.clone());
        let (tex, out_view) = self.outputs[self.next].clone();
        self.next = (self.next + 1) % self.outputs.len();

        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(input_view),
            ..Default::default()
        };
        let streams = [stream];
        let result = unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processor, &out_view, 0, &streams)
        };
        // Release the reference held by the ManuallyDrop field.
        let [mut stream] = streams;
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        result?;
        Ok(tex)
    }

    /// Reads an NV12 texture back to system memory as a tightly packed buffer
    /// (Y plane followed by interleaved UV). Used by software encoders and tests.
    pub fn read_nv12(&self, tex: &ID3D11Texture2D) -> Result<Vec<u8>, CodecError> {
        read_nv12(&self.device, &self.context, tex, self.out_size)
    }
}

pub(crate) fn create_nv12(
    device: &ID3D11Device,
    size: (u32, u32),
    staging: bool,
) -> Result<ID3D11Texture2D, CodecError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: if staging {
            D3D11_USAGE_STAGING
        } else {
            D3D11_USAGE_DEFAULT
        },
        BindFlags: if staging {
            0
        } else {
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32
        },
        CPUAccessFlags: if staging {
            D3D11_CPU_ACCESS_READ.0 as u32
        } else {
            0
        },
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    tex.ok_or_else(|| CodecError::Platform("CreateTexture2D(NV12) returned null".into()))
}

pub(crate) fn read_nv12(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    tex: &ID3D11Texture2D,
    size: (u32, u32),
) -> Result<Vec<u8>, CodecError> {
    use windows::Win32::Graphics::Direct3D11::{D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE};
    let staging = create_nv12(device, size, true)?;
    unsafe { context.CopyResource(&staging, tex) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };
    let (w, h) = (size.0 as usize, size.1 as usize);
    let pitch = mapped.RowPitch as usize;
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    let base = mapped.pData as *const u8;
    for row in 0..(h + h / 2) {
        // NV12 planes are contiguous in the mapped resource: UV starts at row `h`.
        let src = unsafe { std::slice::from_raw_parts(base.add(row * pitch), w) };
        out.extend_from_slice(src);
    }
    unsafe { context.Unmap(&staging, 0) };
    Ok(out)
}
