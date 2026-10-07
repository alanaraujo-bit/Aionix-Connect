//! H.264 encoding through Media Foundation transforms.
//!
//! Selection order:
//! 1. Hardware MFT on the same GPU as the captured display (NVENC, Quick Sync, AMF),
//!    fed directly with D3D11 NV12 textures (zero copy).
//! 2. Hardware MFT of another vendor, fed through system memory.
//! 3. Microsoft software H.264 encoder (system memory).
//!
//! Latency settings: low-latency mode, no B-frames, CBR, effectively infinite GOP with
//! keyframes only on demand (session start, viewer request, loss recovery).

use std::{
    collections::VecDeque,
    sync::mpsc::{Receiver, RecvTimeoutError, channel},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use tracing::{debug, info, warn};
use windows::{
    Win32::{
        Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D},
        Media::MediaFoundation::{
            CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonQualityVsSpeed,
            CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
            CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
            ICodecAPI, IMFActivate, IMFAttributes, IMFDXGIDeviceManager, IMFMediaEventGenerator,
            IMFMediaType, IMFSample, IMFShutdown, IMFTransform,
            MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, METransformHaveOutput, METransformNeedInput,
            MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
            MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
            MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
            MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MFCreateAttributes,
            MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
            MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSampleExtension_CleanPoint,
            MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG,
            MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
            MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
            MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
            MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
            MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnum2, MFTEnumEx,
            MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
            eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High,
        },
    },
    core::Interface,
};

use super::{
    convert,
    mf::{self, SendCom, pack_u64, variant_bool, variant_u32},
};
use crate::{CodecError, EncodedFrame, EncoderConfig, EncoderInfo, EncoderPreference};

/// How long we wait for an asynchronous encoder to accept input.
const INPUT_TIMEOUT: Duration = Duration::from_millis(250);
/// How long we wait for the output of a submitted frame before returning (the frame is
/// then delivered by a later call).
const OUTPUT_WAIT: Duration = Duration::from_millis(60);
/// Practically infinite GOP: keyframes are forced explicitly.
const GOP_SIZE: u32 = 0xFFFF;

enum Mode {
    Async {
        events: Receiver<i32>,
        need_input: u32,
        _pump: JoinHandle<()>,
    },
    Sync,
}

pub struct H264Encoder {
    transform: IMFTransform,
    codec_api: Option<ICodecAPI>,
    info: EncoderInfo,
    config: EncoderConfig,
    mode: Mode,
    /// Input as D3D11 textures (true) or system memory (false).
    gpu_input: bool,
    _device_manager: Option<IMFDXGIDeviceManager>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    provides_samples: bool,
    output_buffer_size: u32,
    submitted: VecDeque<(i64, Instant)>,
    ready: VecDeque<EncodedFrame>,
}

/// Lists usable encoders for diagnostics, best first.
pub fn list_encoders(adapter_luid: Option<(u32, i32)>) -> Vec<EncoderInfo> {
    if mf::startup().is_err() {
        return Vec::new();
    }
    candidates(adapter_luid)
        .into_iter()
        .map(|c| EncoderInfo {
            name: mf::friendly_name(&c.activate),
            hardware: c.hardware,
        })
        .collect()
}

struct Candidate {
    activate: IMFActivate,
    hardware: bool,
    same_adapter: bool,
}

fn enumerate(flags: MFT_ENUM_FLAG, luid: Option<(u32, i32)>) -> Vec<IMFActivate> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut ptr = std::ptr::null_mut();
    let mut count = 0u32;
    let ok = unsafe {
        match luid {
            Some((low, high)) => {
                let mut attrs: Option<IMFAttributes> = None;
                if MFCreateAttributes(&mut attrs, 1).is_err() {
                    return Vec::new();
                }
                let Some(attrs) = attrs else {
                    return Vec::new();
                };
                let mut blob = Vec::with_capacity(8);
                blob.extend_from_slice(&low.to_le_bytes());
                blob.extend_from_slice(&high.to_le_bytes());
                if attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, &blob).is_err() {
                    return Vec::new();
                }
                MFTEnum2(
                    MFT_CATEGORY_VIDEO_ENCODER,
                    flags,
                    Some(&input),
                    Some(&output),
                    &attrs,
                    &mut ptr,
                    &mut count,
                )
            }
            None => MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                Some(&input),
                Some(&output),
                &mut ptr,
                &mut count,
            ),
        }
    };
    if ok.is_err() {
        return Vec::new();
    }
    mf::take_activates(ptr, count)
}

fn candidates(adapter_luid: Option<(u32, i32)>) -> Vec<Candidate> {
    let mut out = Vec::new();
    let hw = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let mut same_names = Vec::new();
    if adapter_luid.is_some() {
        for a in enumerate(hw, adapter_luid) {
            same_names.push(mf::friendly_name(&a));
            out.push(Candidate {
                activate: a,
                hardware: true,
                same_adapter: true,
            });
        }
    }
    for a in enumerate(hw, None) {
        if !same_names.contains(&mf::friendly_name(&a)) {
            out.push(Candidate {
                activate: a,
                hardware: true,
                same_adapter: false,
            });
        }
    }
    let sw = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    for a in enumerate(sw, None) {
        out.push(Candidate {
            activate: a,
            hardware: false,
            same_adapter: false,
        });
    }
    out
}

impl H264Encoder {
    /// Creates the best encoder for `device` (the capture device) that satisfies
    /// `preference`.
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        adapter_luid: (u32, i32),
        config: EncoderConfig,
        preference: &EncoderPreference,
    ) -> Result<Self, CodecError> {
        mf::startup()?;
        let mut last_err = CodecError::NoEncoder;
        let filter = preference.name_contains.as_ref().map(|f| f.to_lowercase());
        for c in candidates(Some(adapter_luid)) {
            if c.hardware && !preference.allow_hardware {
                continue;
            }
            let name = mf::friendly_name(&c.activate);
            if filter
                .as_ref()
                .is_some_and(|f| !name.to_lowercase().contains(f))
            {
                continue;
            }
            match Self::init(&c, &name, device, context, config) {
                Ok(enc) => {
                    info!(encoder = %name, hardware = c.hardware, gpu_input = enc.gpu_input,
                        width = config.width, height = config.height, "H.264 encoder ready");
                    return Ok(enc);
                }
                Err(e) => {
                    warn!(encoder = %name, error = %e, "encoder rejected configuration");
                    let _ = unsafe { c.activate.ShutdownObject() };
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    fn init(
        c: &Candidate,
        name: &str,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: EncoderConfig,
    ) -> Result<Self, CodecError> {
        let transform: IMFTransform =
            unsafe { c.activate.ActivateObject() }.map_err(ctx("activate"))?;
        let attrs = unsafe { transform.GetAttributes()? };
        let is_async = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
        if is_async {
            unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)? };
        }
        let _ = unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, 1) };

        // Zero-copy input only when the encoder lives on the capture GPU.
        let d3d_aware = unsafe { attrs.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) != 0;
        let mut device_manager = None;
        let mut gpu_input = false;
        if d3d_aware && c.same_adapter {
            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager)? };
            let manager = manager.ok_or_else(|| CodecError::Platform("no DXGI manager".into()))?;
            unsafe {
                manager
                    .ResetDevice(device, token)
                    .map_err(ctx("reset device"))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                    .map_err(ctx("set D3D manager"))?;
            }
            device_manager = Some(manager);
            gpu_input = true;
        }

        let codec_api = transform.cast::<ICodecAPI>().ok();
        if let Some(api) = &codec_api {
            set_codec(
                api,
                &CODECAPI_AVLowLatencyMode,
                variant_bool(true),
                "low latency",
            );
            set_codec(
                api,
                &CODECAPI_AVEncCommonRateControlMode,
                variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
                "CBR",
            );
            set_codec(
                api,
                &CODECAPI_AVEncCommonMeanBitRate,
                variant_u32(config.bitrate_kbps * 1000),
                "bitrate",
            );
            set_codec(
                api,
                &CODECAPI_AVEncMPVDefaultBPictureCount,
                variant_u32(0),
                "no B-frames",
            );
            set_codec(api, &CODECAPI_AVEncMPVGOPSize, variant_u32(GOP_SIZE), "GOP");
            set_codec(
                api,
                &CODECAPI_AVEncCommonQualityVsSpeed,
                variant_u32(0),
                "speed preset",
            );
        }

        // Output type first (required by encoders), then input.
        let out_type = video_type(&MFVideoFormat_H264, config)?;
        unsafe {
            out_type.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate_kbps * 1000)?;
            out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            transform
                .SetOutputType(0, &out_type, 0)
                .map_err(ctx("output type"))?;
        }
        set_input_type(&transform, config)?;

        let stream_info =
            unsafe { transform.GetOutputStreamInfo(0) }.map_err(ctx("stream info"))?;
        let provides_samples =
            stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;

        unsafe {
            // Flushing an idle encoder is optional; some encoders reject it.
            let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(ctx("begin streaming"))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(ctx("start of stream"))?;
        }

        let mode = if is_async {
            let generator: IMFMediaEventGenerator = transform.cast()?;
            let (tx, rx) = channel();
            let generator = SendCom(generator);
            let pump = std::thread::Builder::new()
                .name("mft-events".into())
                .spawn(move || {
                    let generator = generator;
                    let _ = mf::startup();
                    loop {
                        let event = unsafe {
                            generator
                                .0
                                .GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))
                        };
                        let Ok(event) = event else { break };
                        let Ok(kind) = (unsafe { event.GetType() }) else {
                            break;
                        };
                        if tx.send(kind as i32).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|e| CodecError::Platform(e.to_string()))?;
            Mode::Async {
                events: rx,
                need_input: 0,
                _pump: pump,
            }
        } else {
            Mode::Sync
        };

        Ok(Self {
            transform,
            codec_api,
            info: EncoderInfo {
                name: name.to_owned(),
                hardware: c.hardware,
            },
            config,
            mode,
            gpu_input,
            _device_manager: device_manager,
            device: device.clone(),
            context: context.clone(),
            provides_samples,
            output_buffer_size: stream_info.cbSize.max(1024 * 1024),
            submitted: VecDeque::new(),
            ready: VecDeque::new(),
        })
    }

    pub fn info(&self) -> &EncoderInfo {
        &self.info
    }

    pub fn config(&self) -> EncoderConfig {
        self.config
    }

    /// Changes the target bitrate without restarting the stream.
    pub fn set_bitrate(&mut self, kbps: u32) -> bool {
        let Some(api) = &self.codec_api else {
            return false;
        };
        let ok =
            unsafe { api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(kbps * 1000)) }
                .is_ok();
        if ok {
            self.config.bitrate_kbps = kbps;
        }
        ok
    }

    /// Encodes one NV12 frame (`nv12` must have the configured size). Returns every
    /// access unit that became available (normally exactly the one for this frame).
    pub fn encode(
        &mut self,
        nv12: &ID3D11Texture2D,
        pts_us: u64,
        keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, CodecError> {
        if keyframe && let Some(api) = &self.codec_api {
            set_codec(
                api,
                &CODECAPI_AVEncVideoForceKeyFrame,
                variant_u32(1),
                "force keyframe",
            );
        }
        let sample = self.input_sample(nv12, pts_us)?;
        let sample_time = (pts_us * 10) as i64;

        match &mut self.mode {
            Mode::Sync => {
                unsafe { self.transform.ProcessInput(0, &sample, 0)? };
                self.submitted.push_back((sample_time, Instant::now()));
                while self.pull_output()? {}
            }
            Mode::Async { .. } => {
                // Wait for permission to submit.
                let deadline = Instant::now() + INPUT_TIMEOUT;
                while self.need_input() == 0 {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(CodecError::Stalled);
                    }
                    self.wait_event(remaining)?;
                }
                unsafe { self.transform.ProcessInput(0, &sample, 0)? };
                if let Mode::Async { need_input, .. } = &mut self.mode {
                    *need_input -= 1;
                }
                self.submitted.push_back((sample_time, Instant::now()));
                // Give the encoder a moment to produce this frame's output.
                let deadline = Instant::now() + OUTPUT_WAIT;
                while self.ready.is_empty() {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() || !self.wait_event(remaining)? {
                        break;
                    }
                }
                self.drain_events()?;
            }
        }
        Ok(self.ready.drain(..).collect())
    }

    fn need_input(&self) -> u32 {
        match &self.mode {
            Mode::Async { need_input, .. } => *need_input,
            Mode::Sync => 1,
        }
    }

    /// Waits for one event. Returns false on timeout.
    fn wait_event(&mut self, timeout: Duration) -> Result<bool, CodecError> {
        let Mode::Async { events, .. } = &self.mode else {
            return Ok(false);
        };
        match events.recv_timeout(timeout) {
            Ok(kind) => {
                self.handle_event(kind)?;
                Ok(true)
            }
            Err(RecvTimeoutError::Timeout) => Ok(false),
            Err(RecvTimeoutError::Disconnected) => {
                Err(CodecError::Platform("encoder event pump stopped".into()))
            }
        }
    }

    fn drain_events(&mut self) -> Result<(), CodecError> {
        loop {
            let Mode::Async { events, .. } = &self.mode else {
                return Ok(());
            };
            match events.try_recv() {
                Ok(kind) => self.handle_event(kind)?,
                Err(_) => return Ok(()),
            }
        }
    }

    fn handle_event(&mut self, kind: i32) -> Result<(), CodecError> {
        if kind == METransformNeedInput.0 {
            if let Mode::Async { need_input, .. } = &mut self.mode {
                *need_input += 1;
            }
        } else if kind == METransformHaveOutput.0 {
            self.pull_output()?;
        }
        Ok(())
    }

    fn input_sample(&self, nv12: &ID3D11Texture2D, pts_us: u64) -> Result<IMFSample, CodecError> {
        let sample = unsafe { MFCreateSample()? };
        let buffer = if self.gpu_input {
            unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false)? }
        } else {
            let bytes = convert::read_nv12(
                &self.device,
                &self.context,
                nv12,
                (self.config.width, self.config.height),
            )?;
            let buffer = unsafe { MFCreateMemoryBuffer(bytes.len() as u32)? };
            let mut ptr = std::ptr::null_mut();
            unsafe {
                buffer.Lock(&mut ptr, None, None)?;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
                buffer.Unlock()?;
                buffer.SetCurrentLength(bytes.len() as u32)?;
            }
            buffer
        };
        unsafe {
            if self.gpu_input
                && let Ok(b2) = buffer.cast::<windows::Win32::Media::MediaFoundation::IMF2DBuffer>()
                && let Ok(len) = b2.GetContiguousLength()
            {
                let _ = buffer.SetCurrentLength(len);
            }
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime((pts_us * 10) as i64)?;
            sample.SetSampleDuration(10_000_000 / i64::from(self.config.fps.max(1)))?;
        }
        Ok(sample)
    }

    /// Pulls one output. Returns whether an access unit was produced.
    fn pull_output(&mut self) -> Result<bool, CodecError> {
        let provided = if self.provides_samples {
            None
        } else {
            let s = unsafe { MFCreateSample()? };
            let b = unsafe { MFCreateMemoryBuffer(self.output_buffer_size)? };
            unsafe { s.AddBuffer(&b)? };
            Some(s)
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(provided),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
        let [buffer] = buffers;
        let sample = std::mem::ManuallyDrop::into_inner(buffer.pSample);
        drop(std::mem::ManuallyDrop::into_inner(buffer.pEvents));
        if let Err(e) = result {
            if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                return Ok(false);
            }
            if e.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                debug!("encoder output type changed, renegotiating");
                let t = unsafe { self.transform.GetOutputAvailableType(0, 0)? };
                unsafe { self.transform.SetOutputType(0, &t, 0)? };
                return Ok(true);
            }
            return Err(e.into());
        }
        let Some(sample) = sample else {
            return Ok(false);
        };
        let data = mf::sample_bytes(&sample)?;
        if data.is_empty() {
            return Ok(true);
        }
        let keyframe = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0
            || contains_idr(&data);
        let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
        // Match the output with its submission (encoders keep order).
        let mut encode_us = 0;
        while let Some((t, at)) = self.submitted.pop_front() {
            if t >= time {
                encode_us = at.elapsed().as_micros() as u32;
                break;
            }
        }
        self.ready.push_back(EncodedFrame {
            data,
            keyframe,
            encode_us,
            pts_us: (time / 10).max(0) as u64,
        });
        Ok(true)
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            // Unblocks the event pump thread (GetEvent returns MF_E_SHUTDOWN).
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
    }
}

fn ctx(step: &'static str) -> impl Fn(windows::core::Error) -> CodecError {
    move |e| CodecError::Platform(format!("{step}: {} ({:#x})", e.message(), e.code().0))
}

fn set_codec(
    api: &ICodecAPI,
    key: &windows::core::GUID,
    value: windows::Win32::System::Variant::VARIANT,
    what: &str,
) {
    if let Err(e) = unsafe { api.SetValue(key, &value) } {
        debug!(setting = what, error = %e, "encoder setting not supported");
    }
}

fn video_type(
    subtype: &windows::core::GUID,
    config: EncoderConfig,
) -> Result<IMFMediaType, CodecError> {
    let t = unsafe { MFCreateMediaType()? };
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(config.width, config.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(config.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    }
    Ok(t)
}

fn set_input_type(transform: &IMFTransform, config: EncoderConfig) -> Result<(), CodecError> {
    let t = video_type(&MFVideoFormat_NV12, config)?;
    match unsafe { transform.SetInputType(0, &t, 0) } {
        Ok(()) => return Ok(()),
        Err(e) => debug!(error = %e, "direct NV12 input type rejected; trying enumerated types"),
    }
    // Some encoders only accept their own enumerated types: pick NV12 and fill size/rate.
    for i in 0.. {
        let Ok(avail) = (unsafe { transform.GetInputAvailableType(0, i) }) else {
            break;
        };
        if unsafe { avail.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_NV12) {
            unsafe {
                avail.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(config.width, config.height))?;
                avail.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(config.fps, 1))?;
                transform
                    .SetInputType(0, &avail, 0)
                    .map_err(ctx("input type"))?;
            }
            return Ok(());
        }
    }
    Err(CodecError::Unsupported(
        "encoder does not accept NV12".into(),
    ))
}

/// Whether an Annex B bitstream contains an IDR slice (NAL type 5).
pub fn contains_idr(data: &[u8]) -> bool {
    nal_types(data).any(|t| t == 5)
}

/// Iterates NAL unit types of an Annex B bitstream.
pub fn nal_types(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 3 < data.len() {
            let three = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1;
            if three {
                i += 3;
                let t = data[i] & 0x1f;
                return Some(t);
            }
            i += 1;
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nal_types() {
        let stream = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 9, 9,
        ];
        assert_eq!(nal_types(&stream).collect::<Vec<_>>(), vec![7, 8, 5]);
        assert!(contains_idr(&stream));
        assert!(!contains_idr(&[0, 0, 1, 0x41, 1, 2, 3]));
    }
}
