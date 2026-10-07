//! End-to-end session tests: technician → authorization → host approval → secure
//! session → real protocol → media/control lifecycle.
//!
//! Real components: iroh/QUIC transport through an in-process relay (end-to-end
//! encrypted, mutually authenticated keys), signed tickets from the development
//! authority, the host ticket validator, Windows screen capture, D3D11 conversion,
//! Media Foundation H.264 encoder and decoder, the protocol and both coordinators.
//!
//! Test doubles, at OS boundaries only: the approval prompt is scripted (the real
//! window needs a human; it is exercised by `consent_probe` and the platform crate's
//! ignored tests), input is recorded instead of injected with `SendInput` (which would
//! move the developer's mouse), and clipboards are in memory (the system clipboard is
//! shared by both peers in one process).
//!
//! Everything runs in one process on one machine; this is not cross-internet
//! validation.

#![cfg(windows)]

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use aionix_crypto::IdentityKey;
use aionix_input::HeldState;
use aionix_protocol::{ALPN, pb};
use aionix_remote_core::session::{
    ApprovalDecision, ApprovalPrompt, ApprovalRequest, DeviceLookup, HostConfig, HostService,
    HostServices, InputRejected, ReconnectPolicy, TechnicianConfig, TechnicianEvent,
    TechnicianIdentity, TechnicianServices, TechnicianSession, TechnicianSessionHandle,
    TicketValidator,
    dev_authority::DevelopmentAuthority,
    diagnostics::{Metric, Unavailable},
    media::{
        DecodedVideoFrame, DisplayGeometry, FrameConsumer, HostInput, HostInputFactory, MediaError,
        MemoryClipboard, VideoPipeline, VideoSource, VideoStartParams,
    },
    state::{RejectReason, SessionState, SessionStatus},
    windows_media::{MfDecoderFactory, ScreenVideoSource},
};
use aionix_shared::{FriendlyId, TransportKind};
use aionix_transport::{RelayEndpoint, Transport, TransportConfig};
use async_trait::async_trait;
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use uuid::Uuid;

/// Capture devices and hardware encoder sessions are shared machine resources: run
/// the media scenarios one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const T: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Answer {
    Approve,
    Reject,
    Never,
}

struct ScriptedApproval {
    answer: Answer,
    asked: Mutex<Vec<ApprovalRequest>>,
}

impl ScriptedApproval {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            asked: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl ApprovalPrompt for ScriptedApproval {
    async fn ask(&self, request: &ApprovalRequest) -> ApprovalDecision {
        self.asked.lock().unwrap().push(request.clone());
        // The "user" takes a moment to read the prompt.
        tokio::time::sleep(Duration::from_millis(150)).await;
        match self.answer {
            Answer::Approve => ApprovalDecision::Approved,
            Answer::Reject => ApprovalDecision::Rejected,
            Answer::Never => std::future::pending().await,
        }
    }
}

#[derive(Default)]
struct InputLog {
    events: Vec<pb::InputEvent>,
    held: HeldState,
    release_all_calls: u32,
    display: Option<DisplayGeometry>,
}

struct RecordingInput(Arc<Mutex<InputLog>>);

impl HostInput for RecordingInput {
    fn set_display(&mut self, display: DisplayGeometry) {
        self.0.lock().unwrap().display = Some(display);
    }

    fn apply(&mut self, event: &pb::InputEvent) -> Result<(), String> {
        use pb::input_event::Event;
        let mut log = self.0.lock().unwrap();
        match &event.event {
            Some(Event::Key(k)) => log.held.key(k.scancode, k.extended, k.down),
            Some(Event::MouseButton(b)) => {
                if let Some(btn) = aionix_remote_core::session::media::mouse_button(b.button) {
                    log.held.button(btn, b.down);
                }
            }
            _ => {}
        }
        log.events.push(event.clone());
        Ok(())
    }

    fn release_all(&mut self) {
        let mut log = self.0.lock().unwrap();
        let _ = log.held.take();
        log.release_all_calls += 1;
    }
}

struct RecordingInputFactory(Arc<Mutex<InputLog>>);

impl HostInputFactory for RecordingInputFactory {
    fn create(&self) -> Box<dyn HostInput> {
        Box::new(RecordingInput(self.0.clone()))
    }
}

/// Real screen source, counting how often a pipeline is started and stopped.
struct CountingVideo {
    inner: ScreenVideoSource,
    started: Arc<AtomicU32>,
    stopped: Arc<AtomicU32>,
}

struct CountingPipeline {
    inner: Box<dyn VideoPipeline>,
    stopped: Arc<AtomicU32>,
}

impl VideoPipeline for CountingPipeline {
    fn command(&self, cmd: aionix_remote_core::session::media::MediaCommand) {
        self.inner.command(cmd);
    }
    fn stop(self: Box<Self>) {
        self.inner.stop();
        self.stopped.fetch_add(1, Ordering::SeqCst);
    }
}

impl VideoSource for CountingVideo {
    fn start(&self, params: VideoStartParams) -> Result<Box<dyn VideoPipeline>, MediaError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountingPipeline {
            inner: self.inner.start(params)?,
            stopped: self.stopped.clone(),
        }))
    }
}

#[derive(Debug, Clone)]
struct FrameInfo {
    epoch: u32,
    seq: u64,
    keyframe: bool,
    width: u32,
    height: u32,
    nv12_len: usize,
    nonzero: bool,
}

struct FrameProbe(Arc<Mutex<Vec<FrameInfo>>>);

impl FrameConsumer for FrameProbe {
    fn on_frame(&mut self, f: DecodedVideoFrame) {
        self.0.lock().unwrap().push(FrameInfo {
            epoch: f.epoch,
            seq: f.seq,
            keyframe: f.keyframe,
            width: f.width,
            height: f.height,
            nv12_len: f.nv12.len(),
            nonzero: f.nv12.iter().any(|b| *b != 0),
        });
    }
}

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

async fn spawn_relay() -> (Server, String) {
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(RelayConfig::new(
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
    ));
    let server = Server::spawn(cfg).await.expect("relay spawns");
    let url = format!("http://{}", server.http_addr().unwrap());
    (server, url)
}

async fn bind(relay: &str) -> Transport {
    let t = Transport::bind(TransportConfig {
        secret_key: *IdentityKey::generate().secret_bytes(),
        relays: vec![RelayEndpoint {
            url: relay.to_owned(),
            token: None,
            qad_port: None,
        }],
        alpns: vec![ALPN.to_vec()],
        port: 0,
        relay_only: false,
    })
    .await
    .expect("transport binds");
    assert!(
        t.wait_online(Duration::from_secs(10)).await,
        "reaches relay"
    );
    t
}

fn primary_display() -> u32 {
    aionix_capture::windows::enumerate_displays()
        .expect("displays")
        .into_iter()
        .find(|d| d.primary)
        .map(|d| d.id)
        .unwrap_or(1)
}

struct Env {
    _relay: Server,
    relay_url: String,
    host_transport: Transport,
    host: HostService,
    host_id: Uuid,
    friendly: FriendlyId,
    authority: Arc<DevelopmentAuthority>,
    approval: Arc<ScriptedApproval>,
    input: Arc<Mutex<InputLog>>,
    host_clipboard: Arc<MemoryClipboard>,
    video_started: Arc<AtomicU32>,
    video_stopped: Arc<AtomicU32>,
}

async fn env(answer: Answer, tweak: impl FnOnce(&mut HostConfig)) -> Env {
    let (relay, relay_url) = spawn_relay().await;
    let host_transport = bind(&relay_url).await;
    let authority = Arc::new(DevelopmentAuthority::new());
    let host_id = Uuid::new_v4();
    let friendly = FriendlyId::random();
    authority.register_host(
        host_id,
        friendly,
        host_transport.local_key(),
        host_transport.addr(),
    );
    let approval = ScriptedApproval::new(answer);
    let input = Arc::new(Mutex::new(InputLog::default()));
    let host_clipboard = Arc::new(MemoryClipboard::new());
    let video_started = Arc::new(AtomicU32::new(0));
    let video_stopped = Arc::new(AtomicU32::new(0));
    let mut config = HostConfig {
        host_name: "PC-TESTE".into(),
        display_id: primary_display(),
        approval_timeout: Duration::from_secs(10),
        resume_grace: Duration::from_secs(10),
        ..Default::default()
    };
    tweak(&mut config);
    let host = HostService::start(
        host_transport.clone(),
        config,
        HostServices {
            validator: Arc::new(TicketValidator::new(
                authority.verifier(),
                host_id,
                host_transport.local_key(),
            )),
            approval: approval.clone(),
            video: Arc::new(CountingVideo {
                inner: ScreenVideoSource::default(),
                started: video_started.clone(),
                stopped: video_stopped.clone(),
            }),
            input: Arc::new(RecordingInputFactory(input.clone())),
            clipboard: host_clipboard.clone(),
        },
    );
    Env {
        _relay: relay,
        relay_url,
        host_transport,
        host,
        host_id,
        friendly,
        authority,
        approval,
        input,
        host_clipboard,
        video_started,
        video_stopped,
    }
}

struct Tech {
    handle: TechnicianSessionHandle,
    frames: Arc<Mutex<Vec<FrameInfo>>>,
    clipboard: Arc<MemoryClipboard>,
    _transport: Transport,
}

fn identity(transport: &Transport) -> TechnicianIdentity {
    TechnicianIdentity {
        user_id: Uuid::new_v4(),
        device_id: Uuid::new_v4(),
        key: transport.local_key(),
        display_name: "Ana Técnica".into(),
        email: "ana@example.com".into(),
        org_name: Some("Suporte Exemplo".into()),
    }
}

async fn technician(
    env: &Env,
    authority: Arc<dyn aionix_remote_core::session::AuthorizationProvider>,
    target: DeviceLookup,
    policy: ReconnectPolicy,
) -> Tech {
    let transport = bind(&env.relay_url).await;
    let frames = Arc::new(Mutex::new(Vec::new()));
    let clipboard = Arc::new(MemoryClipboard::new());
    let mut config = TechnicianConfig::new(identity(&transport), target);
    config.reconnect = policy;
    let handle = TechnicianSession::start(
        transport.clone(),
        config,
        TechnicianServices {
            authority,
            decoder: Arc::new(MfDecoderFactory),
            frames: Box::new(FrameProbe(frames.clone())),
            clipboard: clipboard.clone(),
        },
    );
    Tech {
        handle,
        frames,
        clipboard,
        _transport: transport,
    }
}

fn fast_policy() -> ReconnectPolicy {
    ReconnectPolicy {
        max_attempts: 3,
        initial_delay: Duration::from_millis(100),
        max_delay: Duration::from_millis(400),
        attempt_timeout: Duration::from_secs(2),
    }
}

fn is(state: SessionState) -> impl Fn(&SessionStatus) -> bool {
    move |s| s.state == state
}

async fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn key(scancode: u32, down: bool) -> pb::InputEvent {
    pb::InputEvent {
        event: Some(pb::input_event::Event::Key(pb::KeyEvent {
            scancode,
            extended: false,
            down,
        })),
    }
}

fn mouse_move(x: i32, y: i32) -> pb::InputEvent {
    pb::InputEvent {
        event: Some(pb::input_event::Event::MouseMove(pb::MouseMove { x, y })),
    }
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,aionix_remote_core=info")
        .with_test_writer()
        .try_init();
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// The full happy path, a transport interruption with resume, a refused concurrent
/// request, and teardown.
#[tokio::test(flavor = "multi_thread")]
async fn full_session_lifecycle() {
    let _serial = SERIAL.lock().await;
    init_tracing();
    let env = env(Answer::Approve, |_| {}).await;
    // The technician addresses the host by its friendly id: a lookup handle only;
    // the ticket binds the host's device id and key.
    let tech = technician(
        &env,
        env.authority.clone(),
        DeviceLookup::Friendly(env.friendly),
        fast_policy(),
    )
    .await;
    let mut tech_events = tech.handle.take_events().unwrap();

    // Authorization → visible approval → secure session.
    let st = tech
        .handle
        .wait_for(T, is(SessionState::Connected))
        .await
        .expect("technician connects");
    assert_eq!(st.reconnect_count, 0);
    let host = env.host.active_session().expect("host session");
    host.wait_for(T, is(SessionState::Connected))
        .await
        .expect("host connects");
    {
        let asked = env.approval.asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "the local user was asked exactly once");
        assert_eq!(asked[0].technician_name, "Ana Técnica");
        assert_eq!(asked[0].organization.as_deref(), Some("Suporte Exemplo"));
        assert!(asked[0].permissions.control);
    }
    assert_eq!(env.video_started.load(Ordering::SeqCst), 1);
    let perms = tech.handle.permissions().unwrap();
    assert!(perms.view && perms.control && perms.clipboard);

    // Real video: capture → H.264 → QUIC → decode → consumer.
    let stream = eventually("stream started event", || match tech_events.try_recv() {
        Ok(TechnicianEvent::StreamStarted(s)) => Some(s),
        _ => None,
    })
    .await;
    assert_eq!(stream.codec, pb::VideoCodec::H264 as i32);
    assert!(stream.width >= 2 && stream.height >= 2);
    let first = eventually("first decoded frame", || {
        tech.frames.lock().unwrap().first().cloned()
    })
    .await;
    assert!(first.keyframe, "decoding starts at a keyframe");
    assert_eq!(first.epoch, stream.stream_epoch);
    assert_eq!((first.width, first.height), (stream.width, stream.height));
    assert_eq!(
        first.nv12_len,
        (stream.width * stream.height * 3 / 2) as usize
    );
    assert!(first.nonzero, "decoded picture has content");

    // Allowed input reaches the host input layer.
    tech.handle.send_input(mouse_move(10, 20)).unwrap();
    tech.handle.send_input(key(0x1D, true)).unwrap(); // Ctrl held down
    eventually("input applied on host", || {
        (env.input.lock().unwrap().events.len() >= 2).then_some(())
    })
    .await;
    {
        let log = env.input.lock().unwrap();
        assert!(
            log.display.is_some(),
            "input layer knows the captured display"
        );
        assert!(!log.held.is_empty(), "Ctrl is held");
    }

    // Text clipboard, both directions.
    env.host_clipboard.set("copiado no host");
    eventually("host → technician clipboard", || {
        (tech.clipboard.get().as_deref() == Some("copiado no host")).then_some(())
    })
    .await;
    tech.clipboard.set("copiado pelo técnico");
    eventually("technician → host clipboard", || {
        (env.host_clipboard.get().as_deref() == Some("copiado pelo técnico")).then_some(())
    })
    .await;

    // Diagnostics expose measured values.
    let d = eventually("technician diagnostics", || {
        let d = tech.handle.diagnostics();
        (d.encoder.is_available() && d.decoder.is_available() && d.rtt_ms.is_available())
            .then_some(d)
    })
    .await;
    assert!(matches!(
        d.transport,
        Metric::Available(TransportKind::Lan | TransportKind::Relay | TransportKind::Direct)
    ));
    assert!(d.capture_backend.is_available());
    assert!(d.decode_latency_ms.is_available());
    assert!(d.duration.is_available());
    assert_eq!(d.session_id, Metric::Available(host_session_id(&env)));
    let hd = eventually("host diagnostics", || {
        let d = host.diagnostics();
        (d.encoder.is_available() && d.decode_latency_ms.is_available()).then_some(d)
    })
    .await;
    assert_eq!(
        hd.decoder,
        Metric::Unavailable(Unavailable::NotReportedByPeer)
    );
    assert!(hd.resolution.is_available());

    // A second technician is refused while the host is busy.
    let other = technician(
        &env,
        env.authority.clone(),
        DeviceLookup::Id(env.host_id),
        fast_policy(),
    )
    .await;
    let st = other
        .handle
        .wait_for(T, |s| s.state.is_terminal())
        .await
        .unwrap();
    assert_eq!(st.state, SessionState::Rejected);
    assert_eq!(st.reject_reason, Some(RejectReason::Busy));

    // Short interruption: Connected → Reconnecting → Connected.
    let frames_before = tech.frames.lock().unwrap().len();
    let releases_before = env.input.lock().unwrap().release_all_calls;
    tech.handle.inject_transport_failure();
    let st = tech
        .handle
        .wait_for(T, |s| {
            s.state == SessionState::Connected && s.reconnect_count == 1
        })
        .await
        .expect("technician resumes");
    assert!(st.reconnect_attempts >= 1);
    host.wait_for(T, |s| {
        s.state == SessionState::Connected && s.reconnect_count == 1
    })
    .await
    .expect("host resumes");
    assert_eq!(
        env.approval.asked.lock().unwrap().len(),
        1,
        "resuming within the grace window does not ask again"
    );
    {
        let log = env.input.lock().unwrap();
        assert!(
            log.release_all_calls > releases_before,
            "held input is released when the link drops"
        );
        assert!(log.held.is_empty(), "nothing stays pressed");
    }
    // Video continues on the new connection, restarting from a keyframe. (Frames
    // decoded just before the loss may also sit after `frames_before`.)
    let after = eventually("keyframe after resume", || {
        let f = tech.frames.lock().unwrap();
        f[frames_before..].iter().find(|f| f.keyframe).cloned()
    })
    .await;
    assert!(after.seq > first.seq);
    assert!(after.nonzero);
    tech.handle.send_input(key(0x2A, true)).unwrap(); // Shift held
    eventually("input after resume", || {
        (!env.input.lock().unwrap().held.is_empty()).then_some(())
    })
    .await;

    // Teardown from the technician side.
    let releases_before = env.input.lock().unwrap().release_all_calls;
    tech.handle.end();
    let st = tech
        .handle
        .wait_for(T, |s| s.state.is_terminal())
        .await
        .unwrap();
    assert_eq!(st.state, SessionState::Ended);
    let hs = host.wait_for(T, |s| s.state.is_terminal()).await.unwrap();
    assert_eq!(hs.state, SessionState::Ended);
    {
        let log = env.input.lock().unwrap();
        assert!(log.release_all_calls > releases_before);
        assert!(log.held.is_empty(), "input cleaned up after termination");
    }
    eventually("media stopped", || {
        (env.video_stopped.load(Ordering::SeqCst) == 1).then_some(())
    })
    .await;
    assert_eq!(
        tech.handle.send_input(key(0x1E, true)),
        Err(InputRejected::NotConnected)
    );
    let d = tech.handle.diagnostics();
    assert_eq!(d.rtt_ms, Metric::Unavailable(Unavailable::NotConnected));
    assert!(!d.duration.is_available());
    env.host.shutdown().await;
}

fn host_session_id(env: &Env) -> Uuid {
    match env.host.active_session().unwrap().diagnostics().session_id {
        Metric::Available(id) => id,
        other => panic!("host has no session id: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rejection_terminates_cleanly() {
    let _serial = SERIAL.lock().await;
    init_tracing();
    let env = env(Answer::Reject, |_| {}).await;
    let tech = technician(
        &env,
        env.authority.clone(),
        DeviceLookup::Id(env.host_id),
        fast_policy(),
    )
    .await;
    let st = tech
        .handle
        .wait_for(T, |s| s.state.is_terminal())
        .await
        .unwrap();
    assert_eq!(st.state, SessionState::Rejected);
    assert_eq!(st.reject_reason, Some(RejectReason::UserRejected));
    let host = env.host.next_session().await.unwrap();
    let hs = host.wait_for(T, |s| s.state.is_terminal()).await.unwrap();
    assert_eq!(hs.state, SessionState::Rejected);
    assert_eq!(hs.reject_reason, Some(RejectReason::UserRejected));
    assert_eq!(env.approval.asked.lock().unwrap().len(), 1);
    assert_eq!(
        env.video_started.load(Ordering::SeqCst),
        0,
        "nothing is captured without approval"
    );
    assert!(env.input.lock().unwrap().events.is_empty());
    assert!(tech.frames.lock().unwrap().is_empty());
    assert_eq!(
        tech.handle.send_input(mouse_move(1, 1)),
        Err(InputRejected::NotConnected)
    );
    // The host is free again.
    eventually("host slot released", || {
        env.host.active_session().is_none().then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unanswered_approval_times_out() {
    let _serial = SERIAL.lock().await;
    init_tracing();
    let env = env(Answer::Never, |c| {
        c.approval_timeout = Duration::from_secs(1)
    })
    .await;
    let tech = technician(
        &env,
        env.authority.clone(),
        DeviceLookup::Id(env.host_id),
        fast_policy(),
    )
    .await;
    let st = tech
        .handle
        .wait_for(T, |s| s.state.is_terminal())
        .await
        .unwrap();
    assert_eq!(st.state, SessionState::Rejected);
    assert_eq!(st.reject_reason, Some(RejectReason::ApprovalTimeout));
    let host = env.host.next_session().await.unwrap();
    let hs = host.wait_for(T, |s| s.state.is_terminal()).await.unwrap();
    assert_eq!(hs.reject_reason, Some(RejectReason::ApprovalTimeout));
    assert_eq!(env.video_started.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_from_untrusted_authority_is_rejected_before_approval() {
    let _serial = SERIAL.lock().await;
    init_tracing();
    let env = env(Answer::Approve, |_| {}).await;
    // A different authority knows the host (id, key, address) but the host does not
    // trust its signing key.
    let rogue = Arc::new(DevelopmentAuthority::new());
    rogue.register_host(
        env.host_id,
        env.friendly,
        env.host_transport.local_key(),
        env.host_transport.addr(),
    );
    let tech = technician(&env, rogue, DeviceLookup::Id(env.host_id), fast_policy()).await;
    let st = tech
        .handle
        .wait_for(T, |s| s.state.is_terminal())
        .await
        .unwrap();
    assert_eq!(st.state, SessionState::Rejected);
    assert!(matches!(
        st.reject_reason,
        Some(RejectReason::InvalidTicket(_))
    ));
    let host = env.host.next_session().await.unwrap();
    let hs = host.wait_for(T, |s| s.state.is_terminal()).await.unwrap();
    assert!(matches!(
        hs.reject_reason,
        Some(RejectReason::InvalidTicket(_))
    ));
    assert!(
        env.approval.asked.lock().unwrap().is_empty(),
        "an invalid ticket never reaches the local user"
    );
    assert_eq!(env.video_started.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_reconnect_ends_in_failed() {
    let _serial = SERIAL.lock().await;
    init_tracing();
    let env = env(Answer::Approve, |c| c.resume_grace = Duration::from_secs(2)).await;
    let tech = technician(
        &env,
        env.authority.clone(),
        DeviceLookup::Id(env.host_id),
        fast_policy(),
    )
    .await;
    tech.handle
        .wait_for(T, is(SessionState::Connected))
        .await
        .expect("connected");
    let host = env.host.active_session().unwrap();
    host.wait_for(T, is(SessionState::Connected)).await.unwrap();
    tech.handle.send_input(key(0x38, true)).unwrap(); // Alt held
    eventually("input applied", || {
        (!env.input.lock().unwrap().held.is_empty()).then_some(())
    })
    .await;

    // Drop the link and make every resume attempt fail: the technician's own
    // connection is cut and the host side becomes unreachable.
    host.inject_transport_failure();
    env.host_transport.close().await;

    let st = tech
        .handle
        .wait_for(Duration::from_secs(40), |s| s.state.is_terminal())
        .await
        .unwrap();
    assert_eq!(st.state, SessionState::Failed);
    assert_eq!(st.reconnect_count, 1);
    assert_eq!(st.reconnect_attempts, 3, "bounded by the policy");
    let hs = host
        .wait_for(Duration::from_secs(40), |s| s.state.is_terminal())
        .await
        .unwrap();
    assert_eq!(
        hs.state,
        SessionState::Failed,
        "no zombie session on the host"
    );
    let log = env.input.lock().unwrap();
    assert!(log.held.is_empty(), "held keys released after the failure");
    assert!(log.release_all_calls >= 1);
}
