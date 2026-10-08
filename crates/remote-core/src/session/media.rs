//! Boundaries between session orchestration and the media, input and clipboard
//! subsystems.
//!
//! The coordinators talk to traits; [`super::windows_media`] provides the real Windows
//! implementations (DXGI/WGC/GDI capture → D3D11 conversion → Media Foundation H.264
//! encode, Media Foundation decode, `SendInput`, the system clipboard).
//!
//! Capture, encode and decode objects are COM objects bound to the thread that created
//! them, so pipelines run on dedicated threads and exchange plain data with the async
//! session tasks.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::JoinHandle,
    time::Instant,
};

use aionix_input::{DisplayRect, InputInjector, MouseButton};
use aionix_protocol::{limits, pb};
use tokio::sync::{Notify, mpsc};
use tracing::{debug, warn};

use crate::adaptive::Decision;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MediaError(pub String);

/// One encoded access unit.
#[derive(Debug, Clone)]
pub struct EncodedVideoFrame {
    pub epoch: u32,
    pub seq: u64,
    pub keyframe: bool,
    pub capture_time_us: u64,
    pub encode_us: u32,
    pub data: Vec<u8>,
}

/// Captured display in desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayGeometry {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Pipeline counters over the last reporting interval.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VideoStats {
    pub capture_backend: String,
    pub encoder: String,
    pub hardware_encoder: bool,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub target_fps: u32,
    pub bitrate_kbps: f64,
    pub target_bitrate_kbps: u32,
    pub capture_us_avg: u32,
    pub encode_us_avg: u32,
    /// Frames not encoded because too many were in flight (cumulative).
    pub frames_skipped: u64,
    /// Frames encoded since the pipeline started.
    pub frames_encoded: u64,
}

#[derive(Debug)]
pub enum MediaEvent {
    Display(DisplayGeometry),
    /// A new encoded stream (resolution/codec) starts with the next keyframe.
    Stream(pb::StreamStarted),
    Frame(EncodedVideoFrame),
    CursorShape(pb::CursorShape),
    CursorPosition(pb::CursorPosition),
    Stats(VideoStats),
    /// The pipeline stopped because of an unrecoverable error.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MediaCommand {
    RequestKeyframe,
    Apply(Decision),
    /// Stop capturing (no viewer connected); keeps the pipeline ready.
    Pause,
    /// Resume after a pause, starting with a keyframe.
    Resume,
}

pub struct VideoStartParams {
    pub display_id: u32,
    pub initial: Decision,
    /// Frames sent but not acknowledged; the pipeline skips frames above the limit.
    pub in_flight: Arc<AtomicU32>,
    pub events: mpsc::Sender<MediaEvent>,
}

/// Produces the host's video stream.
pub trait VideoSource: Send + Sync {
    fn start(&self, params: VideoStartParams) -> Result<Box<dyn VideoPipeline>, MediaError>;
}

pub trait VideoPipeline: Send + Sync {
    fn command(&self, cmd: MediaCommand);
    /// Stops the pipeline and waits for its thread (blocking).
    fn stop(self: Box<Self>);
}

/// A decoded picture: tightly packed NV12.
#[derive(Debug, Clone)]
pub struct DecodedPicture {
    pub width: u32,
    pub height: u32,
    pub nv12: Vec<u8>,
}

/// A decoder instance; lives on the decode thread that created it.
pub trait VideoDecoder {
    fn name(&self) -> String;
    fn decode(&mut self, annex_b: &[u8]) -> Result<Vec<DecodedPicture>, MediaError>;
}

pub trait VideoDecoderFactory: Send + Sync {
    /// Called on the decode thread.
    fn create(&self) -> Result<Box<dyn VideoDecoder>, MediaError>;
}

/// A frame ready for display.
#[derive(Debug, Clone)]
pub struct DecodedVideoFrame {
    pub epoch: u32,
    pub seq: u64,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub nv12: Vec<u8>,
    pub decode_us: u32,
}

/// Receives decoded frames (renderer, recorder, test probe). Called on the decode thread.
pub trait FrameConsumer: Send {
    fn on_frame(&mut self, frame: DecodedVideoFrame);
}

/// Decode-side counters, reset by [`DecodeWorker::take_interval`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecodeStats {
    pub decoder: Option<String>,
    pub last_epoch: u32,
    pub last_seq: u64,
    pub width: u32,
    pub height: u32,
    pub frames_received: u32,
    pub frames_decoded: u32,
    pub frames_dropped: u32,
    pub decode_us_total: u64,
    pub bytes_received: u64,
    pub total_decoded: u64,
    pub total_dropped: u64,
    pub last_error: Option<String>,
}

/// Bound of the decode queue; a full queue drops frames until the next keyframe.
const DECODE_QUEUE: usize = 8;

enum DecodeMsg {
    Frame(EncodedVideoFrame),
    Stop,
}

/// Feeds received frames to a [`DecodeWorker`]; cheap to clone per connection.
#[derive(Clone)]
pub(crate) struct DecodeSender {
    tx: SyncSender<DecodeMsg>,
    stats: Arc<Mutex<DecodeStats>>,
    queued: Arc<AtomicU32>,
    resync: Arc<AtomicBool>,
    keyframe_needed: Arc<Notify>,
}

impl DecodeSender {
    /// Queues a received frame. A full queue drops it and forces a resync at the
    /// next keyframe (a gap would corrupt every following inter frame).
    pub fn push(&self, frame: EncodedVideoFrame) {
        let bytes = frame.data.len() as u64;
        {
            let mut s = self.stats.lock().unwrap();
            s.frames_received += 1;
            s.bytes_received += bytes;
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        if self.tx.try_send(DecodeMsg::Frame(frame)).is_err() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            self.resync.store(true, Ordering::Relaxed);
            let mut s = self.stats.lock().unwrap();
            s.frames_dropped += 1;
            s.total_dropped += 1;
            drop(s);
            self.keyframe_needed.notify_one();
        }
    }
}

/// Runs a decoder on its own thread and feeds a [`FrameConsumer`].
pub(crate) struct DecodeWorker {
    sender: DecodeSender,
    thread: Option<JoinHandle<()>>,
}

impl DecodeWorker {
    pub fn start(
        factory: Arc<dyn VideoDecoderFactory>,
        mut consumer: Box<dyn FrameConsumer>,
    ) -> Result<Self, MediaError> {
        let (tx, rx): (SyncSender<DecodeMsg>, Receiver<DecodeMsg>) = sync_channel(DECODE_QUEUE);
        let stats = Arc::new(Mutex::new(DecodeStats::default()));
        let queued = Arc::new(AtomicU32::new(0));
        let resync = Arc::new(AtomicBool::new(false));
        let keyframe_needed = Arc::new(Notify::new());
        let (t_stats, t_queued, t_resync, t_key) = (
            stats.clone(),
            queued.clone(),
            resync.clone(),
            keyframe_needed.clone(),
        );
        let thread = std::thread::Builder::new()
            .name("aionix-decode".into())
            .spawn(move || {
                let mut decoder: Option<Box<dyn VideoDecoder>> = None;
                let mut epoch: Option<u32> = None;
                let mut last_seq: Option<u64> = None;
                let mut waiting_for_key = true;
                while let Ok(DecodeMsg::Frame(frame)) = rx.recv() {
                    t_queued.fetch_sub(1, Ordering::Relaxed);
                    if t_resync.swap(false, Ordering::Relaxed) {
                        waiting_for_key = true;
                    }
                    if epoch != Some(frame.epoch) {
                        // New stream geometry: new decoder, starting at a keyframe.
                        epoch = Some(frame.epoch);
                        decoder = None;
                        waiting_for_key = true;
                    } else if last_seq.is_some_and(|s| frame.seq != s + 1) {
                        // A frame went missing (dropped by the host or lost with a
                        // connection): inter frames would decode into garbage.
                        waiting_for_key = true;
                    }
                    last_seq = Some(frame.seq);
                    let drop_frame = |s: &Arc<Mutex<DecodeStats>>, err: Option<String>| {
                        let mut s = s.lock().unwrap();
                        s.frames_dropped += 1;
                        s.total_dropped += 1;
                        if err.is_some() {
                            s.last_error = err;
                        }
                    };
                    if waiting_for_key && !frame.keyframe {
                        drop_frame(&t_stats, None);
                        t_key.notify_one();
                        continue;
                    }
                    if decoder.is_none() {
                        match factory.create() {
                            Ok(d) => {
                                t_stats.lock().unwrap().decoder = Some(d.name());
                                decoder = Some(d);
                            }
                            Err(e) => {
                                warn!(error = %e, "decoder unavailable");
                                drop_frame(&t_stats, Some(e.0));
                                continue;
                            }
                        }
                    }
                    let Some(dec) = decoder.as_mut() else {
                        continue;
                    };
                    let t0 = Instant::now();
                    match dec.decode(&frame.data) {
                        Ok(pictures) => {
                            waiting_for_key = false;
                            let decode_us = t0.elapsed().as_micros() as u32;
                            {
                                let mut s = t_stats.lock().unwrap();
                                s.last_epoch = frame.epoch;
                                s.last_seq = frame.seq;
                                s.decode_us_total += u64::from(decode_us);
                                s.frames_decoded += pictures.len() as u32;
                                s.total_decoded += pictures.len() as u64;
                                if let Some(p) = pictures.last() {
                                    s.width = p.width;
                                    s.height = p.height;
                                }
                            }
                            for p in pictures {
                                consumer.on_frame(DecodedVideoFrame {
                                    epoch: frame.epoch,
                                    seq: frame.seq,
                                    keyframe: frame.keyframe,
                                    width: p.width,
                                    height: p.height,
                                    nv12: p.nv12,
                                    decode_us,
                                });
                            }
                        }
                        Err(e) => {
                            debug!(error = %e, "decode failed; waiting for a keyframe");
                            decoder = None;
                            waiting_for_key = true;
                            drop_frame(&t_stats, Some(e.0));
                            t_key.notify_one();
                        }
                    }
                }
            })
            .map_err(|e| MediaError(e.to_string()))?;
        Ok(Self {
            sender: DecodeSender {
                tx,
                stats,
                queued,
                resync,
                keyframe_needed,
            },
            thread: Some(thread),
        })
    }

    pub fn sender(&self) -> DecodeSender {
        self.sender.clone()
    }

    #[cfg(test)]
    pub fn push(&self, frame: EncodedVideoFrame) {
        self.sender.push(frame);
    }

    /// Notified whenever the decoder needs a keyframe to continue.
    pub fn keyframe_needed(&self) -> Arc<Notify> {
        self.sender.keyframe_needed.clone()
    }

    pub fn queue_len(&self) -> u32 {
        self.sender.queued.load(Ordering::Relaxed)
    }

    /// Returns the interval counters and resets them (totals are kept).
    pub fn take_interval(&self) -> DecodeStats {
        let mut s = self.sender.stats.lock().unwrap();
        let snapshot = s.clone();
        s.frames_received = 0;
        s.frames_decoded = 0;
        s.frames_dropped = 0;
        s.decode_us_total = 0;
        s.bytes_received = 0;
        snapshot
    }

    /// Stops the decode thread (blocking until the queued frames are done).
    pub fn stop(mut self) {
        let _ = self.sender.tx.send(DecodeMsg::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// The host's input layer as seen by a session.
pub trait HostInput: Send {
    /// Display that mouse coordinates refer to.
    fn set_display(&mut self, display: DisplayGeometry);
    fn apply(&mut self, event: &pb::InputEvent) -> Result<(), String>;
    /// Releases every key and button held on behalf of the technician.
    fn release_all(&mut self);
}

pub trait HostInputFactory: Send + Sync {
    fn create(&self) -> Box<dyn HostInput>;
}

/// Longest text accepted in one `TextInput` event.
pub const MAX_TEXT_INPUT_CHARS: usize = 256;

/// Real input injection through [`InputInjector`] (Windows `SendInput`).
pub struct SystemInput {
    injector: InputInjector,
}

impl SystemInput {
    pub fn new() -> Self {
        Self {
            injector: InputInjector::new(DisplayRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            }),
        }
    }
}

impl Default for SystemInput {
    fn default() -> Self {
        Self::new()
    }
}

impl HostInput for SystemInput {
    fn set_display(&mut self, d: DisplayGeometry) {
        self.injector.set_display(DisplayRect {
            x: d.x,
            y: d.y,
            width: d.width,
            height: d.height,
        });
    }

    fn apply(&mut self, event: &pb::InputEvent) -> Result<(), String> {
        use pb::input_event::Event;
        let inj = &mut self.injector;
        if inj.display().width == 0 {
            return Err("display geometry not known yet".into());
        }
        let r = match &event.event {
            Some(Event::MouseMove(m)) => inj.mouse_move(m.x, m.y),
            Some(Event::MouseButton(b)) => match mouse_button(b.button) {
                Some(btn) => inj.mouse_button(btn, b.down, b.x, b.y),
                None => Ok(()),
            },
            Some(Event::MouseWheel(w)) => inj.mouse_wheel(w.delta_x, w.delta_y),
            Some(Event::Key(k)) => inj.key(k.scancode, k.extended, k.down),
            Some(Event::Text(t)) if t.text.chars().count() <= MAX_TEXT_INPUT_CHARS => {
                inj.text(&t.text)
            }
            Some(Event::Text(_)) => return Err("text input too long".into()),
            Some(Event::ReleaseAll(_)) => inj.release_all(),
            None => Ok(()),
        };
        r.map_err(|e| e.to_string())
    }

    fn release_all(&mut self) {
        let _ = self.injector.release_all();
    }
}

/// Creates a [`SystemInput`] per session.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemInputFactory;

impl HostInputFactory for SystemInputFactory {
    fn create(&self) -> Box<dyn HostInput> {
        Box::new(SystemInput::new())
    }
}

pub fn mouse_button(kind: i32) -> Option<MouseButton> {
    match pb::MouseButtonKind::try_from(kind).ok()? {
        pb::MouseButtonKind::MouseButtonLeft => Some(MouseButton::Left),
        pb::MouseButtonKind::MouseButtonRight => Some(MouseButton::Right),
        pb::MouseButtonKind::MouseButtonMiddle => Some(MouseButton::Middle),
        pb::MouseButtonKind::MouseButtonBack => Some(MouseButton::Back),
        pb::MouseButtonKind::MouseButtonForward => Some(MouseButton::Forward),
        pb::MouseButtonKind::MouseButtonUnspecified => None,
    }
}

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

/// Text clipboard of one side of the session.
pub trait ClipboardAccess: Send + Sync {
    /// Changes whenever the clipboard content changes, whoever changed it.
    fn sequence(&self) -> u64;
    fn read_text(&self) -> Option<String>;
    /// Replaces the content; returns the sequence number after the write.
    fn write_text(&self, text: &str) -> Result<u64, String>;
}

/// Clipboard held in memory, for embedders that own the clipboard UI themselves
/// (e.g. a web view) and for tests.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    inner: Mutex<(u64, Option<String>)>,
}

impl MemoryClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The local user copied `text`.
    pub fn set(&self, text: &str) {
        let _ = self.write_text(text);
    }

    pub fn get(&self) -> Option<String> {
        self.read_text()
    }
}

impl ClipboardAccess for MemoryClipboard {
    fn sequence(&self) -> u64 {
        self.inner.lock().unwrap().0
    }

    fn read_text(&self) -> Option<String> {
        self.inner.lock().unwrap().1.clone()
    }

    fn write_text(&self, text: &str) -> Result<u64, String> {
        let mut g = self.inner.lock().unwrap();
        g.0 += 1;
        g.1 = Some(text.to_owned());
        Ok(g.0)
    }
}

/// Change detection for one side of the clipboard synchronization. Only changes made
/// during the session are sent (the content present when the session starts is not),
/// and writes caused by the peer are not echoed back.
///
/// Text identical to the last text exchanged in either direction is not sent either:
/// when both peers share one clipboard (both ends on the same machine, or a clipboard
/// manager rewriting the same content) the peer's write would otherwise come back as a
/// "local change" and bounce between the peers forever.
pub(crate) struct ClipboardSync {
    clipboard: Arc<dyn ClipboardAccess>,
    last_seen: u64,
    last_synced: Option<String>,
}

impl ClipboardSync {
    pub fn new(clipboard: Arc<dyn ClipboardAccess>) -> Self {
        let last_seen = clipboard.sequence();
        Self {
            clipboard,
            last_seen,
            last_synced: None,
        }
    }

    /// Returns new local text to send, if the clipboard changed. Blocking.
    pub fn poll(&mut self) -> Option<String> {
        let seq = self.clipboard.sequence();
        if seq == self.last_seen {
            return None;
        }
        self.last_seen = seq;
        let text = self.clipboard.read_text()?;
        if text.is_empty()
            || text.len() > limits::MAX_CLIPBOARD_TEXT
            || self.last_synced.as_deref() == Some(text.as_str())
        {
            return None;
        }
        self.last_synced = Some(text.clone());
        Some(text)
    }

    /// Applies text received from the peer. Blocking.
    pub fn apply_remote(&mut self, text: &str) -> Result<(), String> {
        if text.len() > limits::MAX_CLIPBOARD_TEXT {
            return Err("clipboard text too large".into());
        }
        self.last_synced = Some(text.to_owned());
        let seq = self.clipboard.write_text(text)?;
        self.last_seen = seq;
        Ok(())
    }
}

/// Clipboard synchronization running off the async threads (the system clipboard
/// may block briefly while another application holds it).
pub(crate) struct ClipboardChannel {
    sync: Arc<Mutex<ClipboardSync>>,
}

impl ClipboardChannel {
    pub fn new(clipboard: Arc<dyn ClipboardAccess>) -> Self {
        Self {
            sync: Arc::new(Mutex::new(ClipboardSync::new(clipboard))),
        }
    }

    pub async fn poll(&self) -> Option<String> {
        let sync = self.sync.clone();
        tokio::task::spawn_blocking(move || sync.lock().unwrap().poll())
            .await
            .ok()
            .flatten()
    }

    pub async fn apply_remote(&self, text: String) -> Result<(), String> {
        let sync = self.sync.clone();
        tokio::task::spawn_blocking(move || sync.lock().unwrap().apply_remote(&text))
            .await
            .map_err(|e| e.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_sync_sends_local_changes_only_once_and_never_echoes() {
        let clip = Arc::new(MemoryClipboard::new());
        clip.set("before the session");
        let mut sync = ClipboardSync::new(clip.clone());
        assert_eq!(sync.poll(), None, "pre-existing content is not sent");
        clip.set("copied during session");
        assert_eq!(sync.poll().as_deref(), Some("copied during session"));
        assert_eq!(sync.poll(), None);
        sync.apply_remote("from peer").unwrap();
        assert_eq!(clip.get().as_deref(), Some("from peer"));
        assert_eq!(sync.poll(), None, "peer's text is not echoed back");
        let huge = "x".repeat(limits::MAX_CLIPBOARD_TEXT + 1);
        assert!(sync.apply_remote(&huge).is_err());
    }

    #[test]
    fn clipboard_shared_by_both_peers_does_not_bounce() {
        // Host and technician on one machine: one system clipboard, two syncs.
        let shared = Arc::new(MemoryClipboard::new());
        let mut host = ClipboardSync::new(shared.clone());
        let mut tech = ClipboardSync::new(shared.clone());
        shared.set("copiado no host");
        let sent = host.poll().expect("host sends its change");
        tech.apply_remote(&sent).unwrap();
        assert_eq!(host.poll(), None, "the peer's write is not sent back");
        assert_eq!(tech.poll(), None);
        shared.set("outro texto");
        assert_eq!(tech.poll().as_deref(), Some("outro texto"));
    }

    #[test]
    fn mouse_button_mapping() {
        assert_eq!(
            mouse_button(pb::MouseButtonKind::MouseButtonLeft as i32),
            Some(MouseButton::Left)
        );
        assert_eq!(mouse_button(0), None);
        assert_eq!(mouse_button(999), None);
    }

    struct Fake {
        fail_on: Option<u8>,
    }
    impl VideoDecoder for Fake {
        fn name(&self) -> String {
            "test decoder".into()
        }
        fn decode(&mut self, data: &[u8]) -> Result<Vec<DecodedPicture>, MediaError> {
            if Some(data[0]) == self.fail_on {
                return Err(MediaError("corrupt".into()));
            }
            Ok(vec![DecodedPicture {
                width: 2,
                height: 2,
                nv12: vec![data[0]; 6],
            }])
        }
    }
    struct FakeFactory;
    impl VideoDecoderFactory for FakeFactory {
        fn create(&self) -> Result<Box<dyn VideoDecoder>, MediaError> {
            Ok(Box::new(Fake { fail_on: Some(9) }))
        }
    }
    struct Collect(Arc<Mutex<Vec<u64>>>);
    impl FrameConsumer for Collect {
        fn on_frame(&mut self, f: DecodedVideoFrame) {
            self.0.lock().unwrap().push(f.seq);
        }
    }

    fn frame(epoch: u32, seq: u64, keyframe: bool, byte: u8) -> EncodedVideoFrame {
        EncodedVideoFrame {
            epoch,
            seq,
            keyframe,
            capture_time_us: 0,
            encode_us: 0,
            data: vec![byte],
        }
    }

    /// Exercises the worker's resynchronisation rules (decoder stubbed; real H.264 is
    /// covered by the Windows integration test).
    #[test]
    fn decode_worker_waits_for_keyframes() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let w =
            DecodeWorker::start(Arc::new(FakeFactory), Box::new(Collect(seen.clone()))).unwrap();
        w.push(frame(1, 1, false, 1)); // dropped: no keyframe yet
        w.push(frame(1, 2, true, 1));
        w.push(frame(1, 3, false, 1));
        w.push(frame(1, 4, false, 9)); // decode error → resync
        w.push(frame(1, 5, false, 1)); // dropped
        w.push(frame(2, 6, false, 1)); // new epoch without keyframe: dropped
        w.push(frame(2, 7, true, 1));
        let stats = w.take_interval();
        w.stop();
        assert_eq!(*seen.lock().unwrap(), vec![2, 3, 7]);
        let _ = stats;
    }
}
