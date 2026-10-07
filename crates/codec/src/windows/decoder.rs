//! H.264 decoding through Media Foundation (system memory NV12 output).
//!
//! The Technician app decodes with WebCodecs inside the WebView; this decoder backs
//! automated tests, benchmarks and the command-line diagnostic viewer.

use windows::Win32::Media::MediaFoundation::{
    IMFTransform, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE,
    MF_MT_SUBTYPE, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
    MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoArea, MFVideoFormat_H264, MFVideoFormat_NV12,
};

use super::mf;
use crate::CodecError;

/// A decoded picture: tightly packed NV12 (`width * height * 3 / 2` bytes).
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub nv12: Vec<u8>,
}

pub struct H264Decoder {
    transform: IMFTransform,
    /// Friendly name of the decoder MFT (diagnostics).
    name: String,
    /// Visible size (display aperture).
    width: u32,
    height: u32,
    /// Coded size: stride and plane height of the decoder's output buffers
    /// (e.g. 1920x1088 for a 1080p stream).
    stride: u32,
    coded_height: u32,
    provides_samples: bool,
    output_size: u32,
}

impl H264Decoder {
    pub fn new() -> Result<Self, CodecError> {
        mf::startup()?;
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let mut ptr = std::ptr::null_mut();
        let mut count = 0;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0),
                Some(&input),
                Some(&output),
                &mut ptr,
                &mut count,
            )?
        };
        let activate = mf::take_activates(ptr, count)
            .into_iter()
            .next()
            .ok_or(CodecError::NoEncoder)?;
        let name = mf::allocated_string(&activate.clone().into(), &MFT_FRIENDLY_NAME_Attribute)
            .unwrap_or_else(|| "Media Foundation H.264 decoder".into());
        let transform: IMFTransform = unsafe { activate.ActivateObject()? };
        if let Ok(attrs) = unsafe { transform.GetAttributes() } {
            let _ = unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, 1) };
        }
        let in_type = unsafe { MFCreateMediaType()? };
        unsafe {
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &in_type, 0)?;
        }
        let mut dec = Self {
            transform,
            name,
            width: 0,
            height: 0,
            stride: 0,
            coded_height: 0,
            provides_samples: false,
            output_size: 0,
        };
        dec.negotiate_output()?;
        unsafe {
            dec.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            dec.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(dec)
    }

    fn negotiate_output(&mut self) -> Result<(), CodecError> {
        for i in 0.. {
            let Ok(t) = (unsafe { self.transform.GetOutputAvailableType(0, i) }) else {
                break;
            };
            if unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_NV12) {
                unsafe { self.transform.SetOutputType(0, &t, 0)? };
                if let Ok(size) = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) } {
                    self.stride = (size >> 32) as u32;
                    self.coded_height = size as u32;
                }
                if let Ok(stride) = unsafe { t.GetUINT32(&MF_MT_DEFAULT_STRIDE) } {
                    self.stride = (stride as i32).unsigned_abs().max(self.stride);
                }
                (self.width, self.height) = (self.stride, self.coded_height);
                let mut area = MFVideoArea::default();
                let blob = unsafe {
                    std::slice::from_raw_parts_mut(
                        (&mut area as *mut MFVideoArea).cast::<u8>(),
                        std::mem::size_of::<MFVideoArea>(),
                    )
                };
                if unsafe { t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, blob, None) }.is_ok()
                    && area.Area.cx > 0
                    && area.Area.cy > 0
                {
                    self.width = area.Area.cx as u32;
                    self.height = area.Area.cy as u32;
                }
                let info = unsafe { self.transform.GetOutputStreamInfo(0)? };
                self.provides_samples =
                    info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
                self.output_size = info.cbSize;
                return Ok(());
            }
        }
        Err(CodecError::Unsupported("decoder has no NV12 output".into()))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Feeds one access unit and returns the pictures that became available.
    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Vec<DecodedFrame>, CodecError> {
        let sample = unsafe { MFCreateSample()? };
        let buffer = unsafe { MFCreateMemoryBuffer(annex_b.len() as u32)? };
        let mut ptr = std::ptr::null_mut();
        unsafe {
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(annex_b.as_ptr(), ptr, annex_b.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(annex_b.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        let mut out = Vec::new();
        loop {
            match self.pull()? {
                Pull::Frame(f) => out.push(f),
                Pull::Again => continue,
                Pull::NeedInput => break,
            }
        }
        Ok(out)
    }

    fn pull(&mut self) -> Result<Pull, CodecError> {
        let provided = if self.provides_samples {
            None
        } else {
            let s = unsafe { MFCreateSample()? };
            let b = unsafe { MFCreateMemoryBuffer(self.output_size.max(1))? };
            unsafe { s.AddBuffer(&b)? };
            Some(s)
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(provided),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0;
        let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
        let [b] = buffers;
        let sample = std::mem::ManuallyDrop::into_inner(b.pSample);
        drop(std::mem::ManuallyDrop::into_inner(b.pEvents));
        match result {
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Pull::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                self.negotiate_output()?;
                Ok(Pull::Again)
            }
            Err(e) => Err(e.into()),
            Ok(()) => {
                let Some(sample) = sample else {
                    return Ok(Pull::Again);
                };
                let raw = mf::sample_bytes(&sample)?;
                Ok(Pull::Frame(self.unpad(&raw)))
            }
        }
    }

    /// Removes decoder padding (aligned height, e.g. 1088 for 1080p).
    fn unpad(&self, raw: &[u8]) -> DecodedFrame {
        let (w, h) = (self.width as usize, self.height as usize);
        let stride = self.stride as usize;
        let padded_h = self.coded_height as usize;
        let mut nv12 = Vec::with_capacity(w * h * 3 / 2);
        for row in 0..h {
            let start = row * stride;
            nv12.extend_from_slice(&raw[start.min(raw.len())..(start + w).min(raw.len())]);
        }
        let uv_base = padded_h * stride;
        for row in 0..h / 2 {
            let start = uv_base + row * stride;
            nv12.extend_from_slice(&raw[start.min(raw.len())..(start + w).min(raw.len())]);
        }
        nv12.resize(w * h * 3 / 2, 0);
        DecodedFrame {
            width: self.width,
            height: self.height,
            nv12,
        }
    }
}

enum Pull {
    Frame(DecodedFrame),
    Again,
    NeedInput,
}

/// Peak signal-to-noise ratio of the luma planes, in dB (higher is better; >35 dB is
/// visually good for desktop content).
pub fn psnr_luma(a: &[u8], b: &[u8], width: u32, height: u32) -> f64 {
    let n = (width * height) as usize;
    let mse: f64 = a[..n]
        .iter()
        .zip(&b[..n])
        .map(|(x, y)| {
            let d = f64::from(*x) - f64::from(*y);
            d * d
        })
        .sum::<f64>()
        / n as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}
