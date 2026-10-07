//! Windows implementations of the session media boundaries, built on the existing
//! capture (DXGI → WGC → GDI), D3D11 conversion, Media Foundation H.264 encoder and
//! decoder, and the platform clipboard.

use std::{
    sync::{
        atomic::Ordering,
        mpsc::{
            Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, channel, sync_channel,
        },
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use aionix_capture::{
    CaptureError, DisplayInfo,
    windows::{FrameEvent, ScreenCapturer},
};
use aionix_codec::{
    EncoderConfig, EncoderPreference, fit_pixels,
    windows::{H264Decoder, H264Encoder, VideoConverter},
};
use aionix_platform::windows::{clipboard, desktop, init_dpi_awareness};
use aionix_protocol::{limits, pb};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::media::{
    ClipboardAccess, DecodedPicture, DisplayGeometry, EncodedVideoFrame, MediaCommand, MediaError,
    MediaEvent, VideoDecoder, VideoDecoderFactory, VideoPipeline, VideoSource, VideoStartParams,
    VideoStats,
};
use crate::adaptive::Decision;

/// Screen capture + H.264 encoding of one display.
#[derive(Debug, Clone, Default)]
pub struct ScreenVideoSource {
    pub encoder: EncoderPreference,
}

enum Control {
    Command(MediaCommand),
    Stop,
}

struct ScreenPipeline {
    control: Sender<Control>,
    thread: Option<JoinHandle<()>>,
}

impl VideoPipeline for ScreenPipeline {
    fn command(&self, cmd: MediaCommand) {
        let _ = self.control.send(Control::Command(cmd));
    }

    fn stop(mut self: Box<Self>) {
        let _ = self.control.send(Control::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for ScreenPipeline {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Stop);
    }
}

impl VideoSource for ScreenVideoSource {
    fn start(&self, params: VideoStartParams) -> Result<Box<dyn VideoPipeline>, MediaError> {
        let (control, control_rx) = channel();
        let (init_tx, init_rx) = sync_channel(1);
        let preference = self.encoder.clone();
        let thread = std::thread::Builder::new()
            .name("aionix-capture".into())
            .spawn(move || pipeline_thread(params, preference, control_rx, init_tx))
            .map_err(|e| MediaError(e.to_string()))?;
        match init_rx.recv() {
            Ok(Ok(())) => Ok(Box::new(ScreenPipeline {
                control,
                thread: Some(thread),
            })),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(MediaError("capture thread exited during start".into()))
            }
        }
    }
}

fn capture_err(e: CaptureError) -> MediaError {
    MediaError(format!("capture: {e}"))
}

fn pipeline_thread(
    params: VideoStartParams,
    preference: EncoderPreference,
    control: Receiver<Control>,
    init: SyncSender<Result<(), MediaError>>,
) {
    init_dpi_awareness();
    if let Err(e) = desktop::attach_thread_to_input_desktop() {
        debug!(error = %e, "capture thread stays on its desktop");
    }
    let mut p = match Pipeline::open(params, preference) {
        Ok(p) => p,
        Err(e) => {
            let _ = init.send(Err(e));
            return;
        }
    };
    let _ = init.send(Ok(()));
    if let Err(e) = p.run(&control) {
        warn!(error = %e, "video pipeline stopped");
        let _ = p.events.blocking_send(MediaEvent::Failed(e.0));
    }
}

struct Encoding {
    converter: VideoConverter,
    encoder: H264Encoder,
}

struct Pipeline {
    capturer: ScreenCapturer,
    display: DisplayInfo,
    preference: EncoderPreference,
    events: mpsc::Sender<MediaEvent>,
    in_flight: std::sync::Arc<std::sync::atomic::AtomicU32>,
    decision: Decision,
    encoding: Option<Encoding>,
    epoch: u32,
    seq: u64,
    started: Instant,
    force_keyframe: bool,
    dirty: bool,
    paused: bool,
    last_encode: Option<Instant>,
    last_shape_id: u64,
    encode_failures: u32,
    // Interval statistics.
    interval_start: Instant,
    frames: u32,
    bytes: u64,
    capture_us: u64,
    captures: u32,
    encode_us: u64,
    skipped: u64,
    frames_encoded: u64,
}

const STATS_INTERVAL: Duration = Duration::from_secs(1);
const MAX_ENCODE_FAILURES: u32 = 10;
const MAX_REOPEN_ATTEMPTS: u32 = 20;

impl Pipeline {
    fn open(params: VideoStartParams, preference: EncoderPreference) -> Result<Self, MediaError> {
        let (capturer, display) =
            ScreenCapturer::open(params.display_id, None).map_err(capture_err)?;
        let monitor = display.name.clone();
        info!(backend = capturer.backend().name(), %monitor, "capture started");
        let now = Instant::now();
        let mut p = Self {
            capturer,
            display,
            preference,
            events: params.events,
            in_flight: params.in_flight,
            decision: params.initial,
            encoding: None,
            epoch: 0,
            seq: 0,
            started: now,
            force_keyframe: true,
            dirty: true,
            paused: false,
            last_encode: None,
            last_shape_id: 0,
            encode_failures: 0,
            interval_start: now,
            frames: 0,
            bytes: 0,
            capture_us: 0,
            captures: 0,
            encode_us: 0,
            skipped: 0,
            frames_encoded: 0,
        };
        p.rebuild_encoder()?;
        Ok(p)
    }

    fn emit(&self, event: MediaEvent) -> bool {
        self.events.blocking_send(event).is_ok()
    }

    fn rebuild_encoder(&mut self) -> Result<(), MediaError> {
        self.encoding = None;
        let (sw, sh) = self.capturer.size();
        let (w, h) = fit_pixels(sw, sh, self.decision.max_pixels);
        let gpu = self.capturer.gpu().clone();
        let converter = VideoConverter::new(&gpu.device, &gpu.context, (sw, sh), (w, h))
            .map_err(|e| MediaError(format!("converter: {e}")))?;
        let encoder = H264Encoder::new(
            &gpu.device,
            &gpu.context,
            gpu.adapter_luid,
            EncoderConfig {
                width: w,
                height: h,
                fps: self.decision.fps.max(1),
                bitrate_kbps: self.decision.bitrate_kbps.max(100),
            },
            &self.preference,
        )
        .map_err(|e| MediaError(format!("encoder: {e}")))?;
        self.encoding = Some(Encoding { converter, encoder });
        self.epoch += 1;
        self.force_keyframe = true;
        self.dirty = true;
        let d = &self.display;
        self.emit(MediaEvent::Display(DisplayGeometry {
            id: d.id,
            x: d.x,
            y: d.y,
            width: sw,
            height: sh,
        }));
        self.emit(MediaEvent::Stream(pb::StreamStarted {
            stream_epoch: self.epoch,
            display_id: d.id,
            codec: pb::VideoCodec::H264 as i32,
            width: w,
            height: h,
            source_width: sw,
            source_height: sh,
        }));
        Ok(())
    }

    fn handle(&mut self, cmd: MediaCommand) -> Result<(), MediaError> {
        match cmd {
            MediaCommand::RequestKeyframe => self.force_keyframe = true,
            MediaCommand::Pause => self.paused = true,
            MediaCommand::Resume => {
                self.paused = false;
                self.force_keyframe = true;
                self.dirty = true;
            }
            MediaCommand::Apply(d) => {
                let (sw, sh) = self.capturer.size();
                let new_size = fit_pixels(sw, sh, d.max_pixels);
                let current = self
                    .encoding
                    .as_ref()
                    .map(|e| (e.encoder.config().width, e.encoder.config().height));
                self.decision = d;
                if current != Some(new_size) {
                    debug!(?new_size, "resolution change: new stream epoch");
                    self.rebuild_encoder()?;
                } else if let Some(e) = &mut self.encoding
                    && e.encoder.config().bitrate_kbps != d.bitrate_kbps
                {
                    e.encoder.set_bitrate(d.bitrate_kbps.max(100));
                }
            }
        }
        Ok(())
    }

    /// Returns false when asked to stop.
    fn drain_control(&mut self, control: &Receiver<Control>) -> Result<bool, MediaError> {
        loop {
            match control.try_recv() {
                Ok(Control::Command(c)) => self.handle(c)?,
                Ok(Control::Stop) | Err(TryRecvError::Disconnected) => return Ok(false),
                Err(TryRecvError::Empty) => return Ok(true),
            }
        }
    }

    fn run(&mut self, control: &Receiver<Control>) -> Result<(), MediaError> {
        loop {
            if !self.drain_control(control)? {
                return Ok(());
            }
            if self.paused {
                match control.recv_timeout(Duration::from_millis(100)) {
                    Ok(Control::Command(c)) => self.handle(c)?,
                    Ok(Control::Stop) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(RecvTimeoutError::Timeout) => {}
                }
                continue;
            }
            let interval = Duration::from_secs(1) / self.decision.fps.max(1);
            let wait = self
                .last_encode
                .map(|t| interval.saturating_sub(t.elapsed()))
                .unwrap_or_default()
                .max(Duration::from_millis(1))
                .min(interval);
            match self.capturer.next_frame(wait.as_millis().max(1) as u32) {
                Ok(FrameEvent::Captured(f)) => {
                    if let Some(s) = f.cursor_shape
                        && s.rgba.len() <= limits::MAX_CURSOR_BYTES
                    {
                        self.last_shape_id = s.id;
                        let _ = self
                            .events
                            .try_send(MediaEvent::CursorShape(pb::CursorShape {
                                id: s.id,
                                width: s.width,
                                height: s.height,
                                hotspot_x: s.hotspot_x,
                                hotspot_y: s.hotspot_y,
                                rgba: s.rgba,
                            }));
                    }
                    if let Some(p) = f.cursor_position {
                        let _ =
                            self.events
                                .try_send(MediaEvent::CursorPosition(pb::CursorPosition {
                                    x: p.x,
                                    y: p.y,
                                    visible: p.visible,
                                    shape_id: self.last_shape_id,
                                }));
                    }
                    if f.texture.is_some() {
                        self.dirty = true;
                        self.capture_us += u64::from(f.capture_us);
                        self.captures += 1;
                    }
                }
                Ok(FrameEvent::Timeout) => {}
                Err(CaptureError::AccessLost) => self.reopen()?,
                Err(e) => return Err(capture_err(e)),
            }

            let due = self.last_encode.is_none_or(|t| t.elapsed() >= interval);
            if due && (self.dirty || self.force_keyframe) {
                let limit = self.decision.max_in_flight.max(1);
                if !self.force_keyframe && self.in_flight.load(Ordering::Relaxed) >= limit {
                    // The network has not absorbed what was sent: skip, keep latency low.
                    self.skipped += 1;
                } else {
                    self.encode_once()?;
                }
            }
            if self.interval_start.elapsed() >= STATS_INTERVAL {
                self.report_stats();
            }
            if self.events.is_closed() {
                return Ok(());
            }
        }
    }

    /// Desktop switch, display change or GPU reset: reattach and recreate everything.
    fn reopen(&mut self) -> Result<(), MediaError> {
        let mut delay = Duration::from_millis(100);
        for attempt in 1..=MAX_REOPEN_ATTEMPTS {
            std::thread::sleep(delay);
            let _ = desktop::attach_thread_to_input_desktop();
            match ScreenCapturer::open(self.display.id, None) {
                Ok((c, d)) => {
                    info!(attempt, backend = c.backend().name(), "capture reopened");
                    self.capturer = c;
                    self.display = d;
                    // New device: converter and encoder must be recreated.
                    return self.rebuild_encoder();
                }
                Err(e) => debug!(attempt, error = %e, "capture not available yet"),
            }
            delay = (delay * 2).min(Duration::from_secs(1));
        }
        Err(MediaError("capture could not be reopened".into()))
    }

    fn encode_once(&mut self) -> Result<(), MediaError> {
        let Some(enc) = self.encoding.as_mut() else {
            return Ok(());
        };
        let texture = self.capturer.last_texture();
        let pts = self.started.elapsed().as_micros() as u64;
        let keyframe = self.force_keyframe;
        let result = enc
            .converter
            .convert(&texture)
            .and_then(|nv12| enc.encoder.encode(&nv12, pts, keyframe));
        self.last_encode = Some(Instant::now());
        let units = match result {
            Ok(u) => {
                self.encode_failures = 0;
                u
            }
            Err(e) => {
                self.encode_failures += 1;
                warn!(error = %e, failures = self.encode_failures, "encode failed");
                if self.encode_failures >= MAX_ENCODE_FAILURES {
                    return Err(MediaError(format!("encoder keeps failing: {e}")));
                }
                return Ok(());
            }
        };
        self.dirty = false;
        if keyframe {
            self.force_keyframe = false;
        }
        for au in units {
            self.seq += 1;
            self.frames += 1;
            self.frames_encoded += 1;
            self.bytes += au.data.len() as u64;
            self.encode_us += u64::from(au.encode_us);
            let frame = EncodedVideoFrame {
                epoch: self.epoch,
                seq: self.seq,
                keyframe: au.keyframe,
                capture_time_us: au.pts_us,
                encode_us: au.encode_us,
                data: au.data,
            };
            if self.events.try_send(MediaEvent::Frame(frame)).is_err() {
                // A gap breaks every following inter frame; restart from a keyframe.
                self.force_keyframe = true;
            }
        }
        Ok(())
    }

    fn report_stats(&mut self) {
        let secs = self.interval_start.elapsed().as_secs_f64().max(1e-3);
        let (backend, encoder, hardware, (w, h), bitrate_cfg) = match &self.encoding {
            Some(e) => (
                self.capturer.backend().name().to_owned(),
                e.encoder.info().name.clone(),
                e.encoder.info().hardware,
                (e.encoder.config().width, e.encoder.config().height),
                e.encoder.config().bitrate_kbps,
            ),
            None => (
                self.capturer.backend().name().to_owned(),
                String::new(),
                false,
                (0, 0),
                0,
            ),
        };
        let stats = VideoStats {
            capture_backend: backend,
            encoder,
            hardware_encoder: hardware,
            width: w,
            height: h,
            fps: f64::from(self.frames) / secs,
            target_fps: self.decision.fps,
            bitrate_kbps: self.bytes as f64 * 8.0 / 1000.0 / secs,
            target_bitrate_kbps: bitrate_cfg,
            capture_us_avg: (self.capture_us / u64::from(self.captures.max(1))) as u32,
            encode_us_avg: (self.encode_us / u64::from(self.frames.max(1))) as u32,
            frames_skipped: self.skipped,
            frames_encoded: self.frames_encoded,
        };
        let _ = self.events.try_send(MediaEvent::Stats(stats));
        self.interval_start = Instant::now();
        self.frames = 0;
        self.bytes = 0;
        self.capture_us = 0;
        self.captures = 0;
        self.encode_us = 0;
    }
}

/// Media Foundation H.264 decoder.
#[derive(Debug, Default, Clone, Copy)]
pub struct MfDecoderFactory;

struct MfDecoder(H264Decoder);

impl VideoDecoder for MfDecoder {
    fn name(&self) -> String {
        self.0.name().to_owned()
    }

    fn decode(&mut self, annex_b: &[u8]) -> Result<Vec<DecodedPicture>, MediaError> {
        self.0
            .decode(annex_b)
            .map(|frames| {
                frames
                    .into_iter()
                    .map(|f| DecodedPicture {
                        width: f.width,
                        height: f.height,
                        nv12: f.nv12,
                    })
                    .collect()
            })
            .map_err(|e| MediaError(e.to_string()))
    }
}

impl VideoDecoderFactory for MfDecoderFactory {
    fn create(&self) -> Result<Box<dyn VideoDecoder>, MediaError> {
        H264Decoder::new()
            .map(|d| Box::new(MfDecoder(d)) as Box<dyn VideoDecoder>)
            .map_err(|e| MediaError(e.to_string()))
    }
}

/// The Windows text clipboard.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClipboard;

impl ClipboardAccess for SystemClipboard {
    fn sequence(&self) -> u64 {
        u64::from(clipboard::sequence_number())
    }

    fn read_text(&self) -> Option<String> {
        clipboard::get_text().ok().flatten()
    }

    fn write_text(&self, text: &str) -> Result<u64, String> {
        clipboard::set_text(text)
            .map(u64::from)
            .map_err(|e| e.to_string())
    }
}
