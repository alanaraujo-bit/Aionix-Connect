//! Host-side session coordinator.
//!
//! ```text
//! accept connection ─▶ Hello ─▶ validate ticket ─▶ visible approval ─▶ AuthStatus(GRANTED)
//!   ─▶ start capture/encode ─▶ StreamStarted ─▶ Connected ─▶ video / cursor / input / clipboard
//!   ─▶ (link lost ─▶ Reconnecting ─▶ resume token ─▶ Connected | grace expired ─▶ Failed)
//!   ─▶ Goodbye / end ─▶ release input, stop media, close ─▶ Ended
//! ```
//!
//! One session at a time; further requests are answered `BUSY`. Input is applied only
//! while the session is `Connected`/`Degraded` and the ticket grants `control`; every key
//! and button held for the technician is released whenever the link drops or the
//! session ends.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use aionix_crypto::{PublicKey, ct_eq, random_token, sha256_hex};
use aionix_protocol::{
    ALPN, PROTOCOL_VERSION,
    framing::{read_frame, write_frame},
    limits, pb,
};
use aionix_shared::QualityMode;
use aionix_transport::{Incoming, PeerConnection, RecvStream, SendStream, Transport};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::{debug, info, warn};

use super::{
    StateCell,
    approval::{ApprovalDecision, ApprovalPrompt, ApprovalRequest, request_approval},
    diagnostics::{LossMeter, Metric, Resolution, SessionDiagnostics, SessionRole, Unavailable},
    link::{
        CLOSE_FAULT, CLOSE_NORMAL, CLOSE_PROTOCOL, CLOSE_REJECTED, CLOSE_SUPERSEDED, ControlWriter,
        InFlight, LinkEvent, encode_video_frame, goodbye, host_msg, spawn_reader,
    },
    media::{
        ClipboardAccess, ClipboardChannel, EncodedVideoFrame, HostInput, HostInputFactory,
        MediaCommand, MediaEvent, VideoPipeline, VideoSource, VideoStartParams, VideoStats,
    },
    state::{RejectReason, SessionEvent, SessionState, SessionStatus},
    ticket::{AuthorizedSession, TicketValidator},
    wait_for_status,
};
use crate::adaptive::{AdaptiveController, Condition, Decision, NetworkSample};

use pb::{host_to_viewer::Msg as H, viewer_to_host::Msg as V};

#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Shown to the technician after authorization.
    pub host_name: String,
    pub display_id: u32,
    pub quality: QualityMode,
    /// Time allowed between the connection and a valid `Hello`.
    pub hello_timeout: Duration,
    /// The approval prompt refuses automatically after this long.
    pub approval_timeout: Duration,
    /// Time from approval until the first stream must be running.
    pub establish_timeout: Duration,
    /// How long a dropped session waits for the technician to resume it.
    pub resume_grace: Duration,
    pub stats_interval: Duration,
    pub clipboard_poll: Duration,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            host_name: String::new(),
            display_id: 1,
            quality: QualityMode::Automatic,
            hello_timeout: Duration::from_secs(10),
            approval_timeout: Duration::from_secs(60),
            establish_timeout: Duration::from_secs(15),
            resume_grace: Duration::from_secs(30),
            stats_interval: Duration::from_millis(500),
            clipboard_poll: Duration::from_millis(500),
        }
    }
}

/// Subsystems the host coordinator orchestrates.
pub struct HostServices {
    pub validator: Arc<TicketValidator>,
    pub approval: Arc<dyn ApprovalPrompt>,
    pub video: Arc<dyn VideoSource>,
    pub input: Arc<dyn HostInputFactory>,
    pub clipboard: Arc<dyn ClipboardAccess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCommand {
    End,
    InjectTransportFailure,
}

/// Observes and controls one host session.
#[derive(Clone)]
pub struct HostSessionHandle {
    cell: Arc<StateCell>,
    cmds: mpsc::UnboundedSender<HostCommand>,
}

impl HostSessionHandle {
    pub fn status(&self) -> SessionStatus {
        self.cell.subscribe().borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<SessionStatus> {
        self.cell.subscribe()
    }

    pub async fn wait_for(
        &self,
        timeout: Duration,
        pred: impl Fn(&SessionStatus) -> bool,
    ) -> Result<SessionStatus, SessionStatus> {
        wait_for_status(self.cell.subscribe(), timeout, pred).await
    }

    /// The local user (or the host application) ends the session.
    pub fn end(&self) {
        let _ = self.cmds.send(HostCommand::End);
    }

    /// Diagnostics/testing: drops the current connection abruptly, as a network
    /// failure would. The session then follows the normal reconnection rules.
    pub fn inject_transport_failure(&self) {
        let _ = self.cmds.send(HostCommand::InjectTransportFailure);
    }

    pub fn diagnostics(&self) -> SessionDiagnostics {
        self.cell.diagnostics()
    }
}

struct ResumeRequest {
    conn: PeerConnection,
    send: SendStream,
    recv: RecvStream,
    token: String,
}

struct Active {
    local_id: u64,
    technician: Option<PublicKey>,
    resume: mpsc::Sender<ResumeRequest>,
    handle: HostSessionHandle,
}

struct Shared {
    transport: Transport,
    config: HostConfig,
    services: HostServices,
    active: Mutex<Option<Active>>,
    new_sessions: mpsc::UnboundedSender<HostSessionHandle>,
    next_id: AtomicU64,
}

/// Accepts session requests on a transport and runs host sessions.
pub struct HostService {
    shared: Arc<Shared>,
    accept: JoinHandle<()>,
    sessions: tokio::sync::Mutex<mpsc::UnboundedReceiver<HostSessionHandle>>,
}

impl HostService {
    /// The transport must accept [`ALPN`].
    pub fn start(transport: Transport, config: HostConfig, services: HostServices) -> Self {
        let (new_sessions, sessions) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            transport,
            config,
            services,
            active: Mutex::new(None),
            new_sessions,
            next_id: AtomicU64::new(1),
        });
        let accept = tokio::spawn(accept_loop(shared.clone()));
        Self {
            shared,
            accept,
            sessions: tokio::sync::Mutex::new(sessions),
        }
    }

    /// Next session request that reached this host (in any outcome).
    pub async fn next_session(&self) -> Option<HostSessionHandle> {
        self.sessions.lock().await.recv().await
    }

    pub fn active_session(&self) -> Option<HostSessionHandle> {
        self.shared
            .active
            .lock()
            .unwrap()
            .as_ref()
            .map(|a| a.handle.clone())
    }

    /// Stops accepting connections and ends the active session.
    pub async fn shutdown(self) {
        self.accept.abort();
        if let Some(h) = self.active_session() {
            h.end();
            let _ = h
                .wait_for(Duration::from_secs(5), |s| s.state.is_terminal())
                .await;
        }
    }
}

async fn accept_loop(shared: Arc<Shared>) {
    while let Some(incoming) = shared.transport.accept().await {
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_incoming(shared, incoming).await {
                debug!(error = %e, "incoming connection dropped");
            }
        });
    }
}

/// Answers a request that will not become (or resume) a session, then closes.
async fn refuse(conn: &PeerConnection, mut send: SendStream, reason: pb::DenyReason) {
    let msg = host_msg(H::AuthStatus(pb::AuthStatus {
        state: pb::AuthState::Denied as i32,
        reason: reason as i32,
        ..Default::default()
    }));
    if write_frame(&mut send, &msg).await.is_ok() && send.finish().is_ok() {
        let _ = tokio::time::timeout(Duration::from_secs(1), send.acknowledged()).await;
    }
    conn.close(CLOSE_REJECTED, "refused");
}

async fn handle_incoming(shared: Arc<Shared>, incoming: Incoming) -> Result<(), String> {
    let conn = incoming.accept().await.map_err(|e| e.to_string())?;
    if conn.alpn() != ALPN {
        conn.close(CLOSE_PROTOCOL, "unsupported protocol");
        return Err("unexpected ALPN".into());
    }
    let wait = shared.config.hello_timeout;
    let (send, mut recv) = tokio::time::timeout(wait, conn.accept_bi())
        .await
        .map_err(|_| "no control stream".to_owned())?
        .map_err(|e| e.to_string())?;
    let first: pb::ViewerToHost =
        tokio::time::timeout(wait, read_frame(&mut recv, limits::MAX_CONTROL_MESSAGE))
            .await
            .map_err(|_| "no hello".to_owned())?
            .map_err(|e| e.to_string())?;
    let Some(V::Hello(hello)) = first.msg else {
        conn.close(CLOSE_PROTOCOL, "expected hello");
        return Err("first message is not hello".into());
    };
    if hello.protocol_version == 0 {
        conn.close(CLOSE_PROTOCOL, "unsupported protocol version");
        return Err("protocol version 0".into());
    }

    if !hello.resume_token.is_empty() {
        let peer = conn.remote_key();
        let target = {
            let active = shared.active.lock().unwrap();
            active
                .as_ref()
                .filter(|a| a.technician == Some(peer))
                .map(|a| a.resume.clone())
        };
        match target {
            Some(tx) => {
                let req = ResumeRequest {
                    conn,
                    send,
                    recv,
                    token: hello.resume_token,
                };
                if let Err(mpsc::error::SendError(req)) = tx.send(req).await {
                    refuse(&req.conn, req.send, pb::DenyReason::InvalidTicket).await;
                }
            }
            None => refuse(&conn, send, pb::DenyReason::InvalidTicket).await,
        }
        return Ok(());
    }

    // New session request. Check and claim the single session slot atomically.
    let cell = StateCell::new(SessionRole::Host);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (resume_tx, resume_rx) = mpsc::channel(2);
    let local_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let handle = HostSessionHandle {
        cell: cell.clone(),
        cmds: cmd_tx,
    };
    let claimed = {
        let mut active = shared.active.lock().unwrap();
        let busy = active
            .as_ref()
            .is_some_and(|a| !a.handle.cell.state().is_terminal());
        if !busy {
            *active = Some(Active {
                local_id,
                technician: None,
                resume: resume_tx,
                handle: handle.clone(),
            });
        }
        !busy
    };
    if !claimed {
        info!("session request refused: host busy");
        refuse(&conn, send, pb::DenyReason::Busy).await;
        return Ok(());
    }
    let _ = shared.new_sessions.send(handle);

    let (session, events_rx) = HostSession::new(shared.clone(), cell, local_id);
    session
        .run(conn, send, recv, hello, events_rx, cmd_rx, resume_rx)
        .await;

    let mut active = shared.active.lock().unwrap();
    if active.as_ref().is_some_and(|a| a.local_id == local_id) {
        *active = None;
    }
    Ok(())
}

/// One connection carrying a session.
struct HostLink {
    conn: PeerConnection,
    control: ControlWriter<pb::HostToViewer>,
    video: Option<mpsc::Sender<EncodedVideoFrame>>,
    tasks: Vec<JoinHandle<()>>,
}

impl HostLink {
    fn new(
        generation: u64,
        conn: PeerConnection,
        send: SendStream,
        recv: RecvStream,
        events: mpsc::Sender<LinkEvent<pb::ViewerToHost>>,
    ) -> Self {
        Self {
            control: ControlWriter::spawn(send),
            tasks: vec![spawn_reader(generation, recv, events)],
            video: None,
            conn,
        }
    }

    async fn send(&self, msg: H) -> bool {
        self.control.send(host_msg(msg)).await
    }

    fn try_send(&self, msg: H) -> bool {
        self.control.try_send(host_msg(msg))
    }

    fn start_media(&mut self, ctx: &MediaLinkContext) {
        let (tx, rx) = mpsc::channel(4);
        self.video = Some(tx);
        self.tasks.push(tokio::spawn(video_task(
            self.conn.clone(),
            rx,
            ctx.in_flight.clone(),
            ctx.sent_bytes.clone(),
        )));
        self.tasks.push(tokio::spawn(input_task(
            self.conn.clone(),
            ctx.input.clone(),
            ctx.input_enabled.clone(),
            ctx.allow_input,
        )));
    }

    async fn close(self, code: u32, reason: &str) {
        for t in &self.tasks {
            t.abort();
        }
        self.control.close().await;
        self.conn.close(code, reason);
    }
}

struct MediaLinkContext {
    in_flight: Arc<InFlight>,
    sent_bytes: Arc<AtomicU64>,
    input: Arc<Mutex<Box<dyn HostInput>>>,
    input_enabled: Arc<AtomicBool>,
    allow_input: bool,
}

async fn video_task(
    conn: PeerConnection,
    mut frames: mpsc::Receiver<EncodedVideoFrame>,
    in_flight: Arc<InFlight>,
    sent_bytes: Arc<AtomicU64>,
) {
    while let Some(frame) = frames.recv().await {
        let Ok(mut stream) = conn.open_uni().await else {
            return;
        };
        stream.set_priority(i32::from(frame.keyframe));
        let bytes = encode_video_frame(&frame);
        in_flight.begin(frame.seq);
        if stream.write_all(&bytes).await.is_err() || stream.finish().is_err() {
            in_flight.end(frame.seq);
            return;
        }
        sent_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let in_flight = in_flight.clone();
        let seq = frame.seq;
        tokio::spawn(async move {
            let _ = stream.acknowledged().await;
            in_flight.end(seq);
        });
    }
}

/// Most events accepted from one batch (a batch is at most 64 KiB anyway).
const MAX_EVENTS_PER_BATCH: usize = 512;

async fn input_task(
    conn: PeerConnection,
    input: Arc<Mutex<Box<dyn HostInput>>>,
    enabled: Arc<AtomicBool>,
    allowed: bool,
) {
    let Ok(mut recv) = conn.accept_uni().await else {
        return;
    };
    if !allowed {
        // The ticket does not grant control: refuse the input stream outright.
        recv.stop(CLOSE_REJECTED);
        return;
    }
    while let Ok(batch) = read_frame::<_, pb::InputBatch>(&mut recv, limits::MAX_INPUT_BATCH).await
    {
        let mut inp = input.lock().unwrap();
        // Checked under the lock: once the session disabled input and released held
        // keys, no later event can press anything again.
        if !enabled.load(Ordering::SeqCst) {
            continue;
        }
        for ev in batch.events.iter().take(MAX_EVENTS_PER_BATCH) {
            if let Err(e) = inp.apply(ev) {
                debug!(error = %e, "input event not applied");
            }
        }
    }
}

enum Exit {
    /// Rejected during authorization/approval (state already applied).
    Rejected,
    /// Orderly end; `goodbye` is sent if the link is still up.
    Ended(Option<pb::Goodbye>),
    Failed(String),
}

struct HostSession {
    shared: Arc<Shared>,
    cell: Arc<StateCell>,
    local_id: u64,
    events_tx: mpsc::Sender<LinkEvent<pb::ViewerToHost>>,
    generation: u64,
    link: Option<HostLink>,
    auth: Option<AuthorizedSession>,
    resume_hash: Option<String>,
    in_flight: Arc<InFlight>,
    sent_bytes: Arc<AtomicU64>,
    input: Arc<Mutex<Box<dyn HostInput>>>,
    input_enabled: Arc<AtomicBool>,
    clipboard: ClipboardChannel,
    pipeline: Option<Box<dyn VideoPipeline>>,
    stream: Option<pb::StreamStarted>,
    stats: Option<VideoStats>,
    controller: Option<AdaptiveController>,
    last_decision: Option<Decision>,
    last_report: Option<pb::ReceiverReport>,
    fresh_report: bool,
    viewer_dropped_total: u64,
    loss: LossMeter,
    last_sent_bytes: u64,
    last_tick: Instant,
    congested_ticks: u32,
    good_ticks: u32,
    last_keyframe_request: Option<Instant>,
    reconnect_deadline: Option<tokio::time::Instant>,
    /// False once every [`HostSessionHandle`] was dropped.
    cmds_open: bool,
}

/// Keyframes are expensive; honour viewer requests at most this often.
const KEYFRAME_MIN_INTERVAL: Duration = Duration::from_millis(300);
/// Congested ticks before `Degraded`, good ticks before back to `Connected`.
const DEGRADE_AFTER: u32 = 3;
const RECOVER_AFTER: u32 = 6;

impl HostSession {
    fn new(
        shared: Arc<Shared>,
        cell: Arc<StateCell>,
        local_id: u64,
    ) -> (Self, mpsc::Receiver<LinkEvent<pb::ViewerToHost>>) {
        let (events_tx, events_rx) = mpsc::channel(256);
        let input = Arc::new(Mutex::new(shared.services.input.create()));
        let clipboard = ClipboardChannel::new(shared.services.clipboard.clone());
        (
            Self {
                shared,
                cell,
                local_id,
                events_tx,
                generation: 0,
                link: None,
                auth: None,
                resume_hash: None,
                in_flight: Arc::new(InFlight::default()),
                sent_bytes: Arc::new(AtomicU64::new(0)),
                input,
                input_enabled: Arc::new(AtomicBool::new(false)),
                clipboard,
                pipeline: None,
                stream: None,
                stats: None,
                controller: None,
                last_decision: None,
                last_report: None,
                fresh_report: false,
                viewer_dropped_total: 0,
                loss: LossMeter::default(),
                last_sent_bytes: 0,
                last_tick: Instant::now(),
                congested_ticks: 0,
                good_ticks: 0,
                last_keyframe_request: None,
                reconnect_deadline: None,
                cmds_open: true,
            },
            events_rx,
        )
    }

    fn live(&self) -> bool {
        self.cell.state().is_live()
    }

    fn permissions(&self) -> super::Permissions {
        self.auth
            .as_ref()
            .map(|a| a.permissions)
            .unwrap_or_default()
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn rotate_resume_token(&mut self) -> String {
        let token = random_token(32);
        self.resume_hash = Some(sha256_hex(token.as_bytes()));
        token
    }

    /// Enables or disables input; disabling releases everything held.
    fn set_input_enabled(&self, on: bool) {
        self.input_enabled.store(on, Ordering::SeqCst);
        if !on {
            self.input.lock().unwrap().release_all();
        }
    }

    fn media_ctx(&self) -> MediaLinkContext {
        MediaLinkContext {
            in_flight: self.in_flight.clone(),
            sent_bytes: self.sent_bytes.clone(),
            input: self.input.clone(),
            input_enabled: self.input_enabled.clone(),
            allow_input: self.permissions().control,
        }
    }

    async fn send(&self, msg: H) {
        if let Some(link) = &self.link {
            link.send(msg).await;
        }
    }

    async fn deny(&self, reason: pb::DenyReason) {
        self.send(H::AuthStatus(pb::AuthStatus {
            state: pb::AuthState::Denied as i32,
            reason: reason as i32,
            ..Default::default()
        }))
        .await;
    }

    async fn reject(&self, deny: pb::DenyReason, reason: RejectReason) -> Exit {
        info!(?reason, "session rejected");
        self.deny(deny).await;
        self.cell.apply(SessionEvent::Reject(reason));
        Exit::Rejected
    }

    fn inject_fault(&self) {
        if let Some(link) = &self.link {
            warn!("injecting transport failure");
            link.conn.close(CLOSE_FAULT, "transport failure injected");
        }
    }

    fn request_keyframe(&mut self, force: bool) {
        if !force
            && self
                .last_keyframe_request
                .is_some_and(|t| t.elapsed() < KEYFRAME_MIN_INTERVAL)
        {
            return;
        }
        self.last_keyframe_request = Some(Instant::now());
        if let Some(p) = &self.pipeline {
            p.command(MediaCommand::RequestKeyframe);
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        mut self,
        conn: PeerConnection,
        send: SendStream,
        recv: RecvStream,
        hello: pb::Hello,
        mut events: mpsc::Receiver<LinkEvent<pb::ViewerToHost>>,
        mut cmds: mpsc::UnboundedReceiver<HostCommand>,
        mut resumes: mpsc::Receiver<ResumeRequest>,
    ) {
        let exit = self
            .drive(
                conn,
                send,
                recv,
                hello,
                &mut events,
                &mut cmds,
                &mut resumes,
            )
            .await;
        self.teardown(exit).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn drive(
        &mut self,
        conn: PeerConnection,
        send: SendStream,
        recv: RecvStream,
        hello: pb::Hello,
        events: &mut mpsc::Receiver<LinkEvent<pb::ViewerToHost>>,
        cmds: &mut mpsc::UnboundedReceiver<HostCommand>,
        resumes: &mut mpsc::Receiver<ResumeRequest>,
    ) -> Exit {
        self.cell.apply(SessionEvent::Start);
        self.cell.apply(SessionEvent::Authenticate);
        let generation = self.next_generation();
        self.link = Some(HostLink::new(
            generation,
            conn.clone(),
            send,
            recv,
            self.events_tx.clone(),
        ));

        // 1. Authorization: the ticket must be valid for this host and this peer.
        let validator = self.shared.services.validator.clone();
        let auth = match validator.validate(&hello.session_ticket, &conn.remote_key()) {
            Ok(a) => a,
            Err(e) => {
                warn!(error = %e, "session ticket rejected");
                return self
                    .reject(
                        pb::DenyReason::InvalidTicket,
                        RejectReason::InvalidTicket(e.to_string()),
                    )
                    .await;
            }
        };
        info!(session = %auth.session_id, technician = %auth.technician.display_name,
            "session ticket accepted");
        {
            let mut active = self.shared.active.lock().unwrap();
            if let Some(a) = active.as_mut().filter(|a| a.local_id == self.local_id) {
                a.technician = Some(auth.technician.key);
            }
        }
        let sid = auth.session_id;
        self.cell
            .update_diagnostics(|d| d.session_id = Metric::Available(sid));
        let request = ApprovalRequest {
            session_id: auth.session_id,
            technician_name: auth.technician.display_name.clone(),
            technician_email: auth.technician.email.clone(),
            organization: auth.technician.org_name.clone(),
            permissions: auth.permissions,
            timeout: self.shared.config.approval_timeout,
        };
        self.auth = Some(auth);
        self.send(H::HelloAck(pb::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            host_version: aionix_shared::VERSION.into(),
            auth_state: pb::AuthState::Pending as i32,
            methods: vec![pb::AuthMethod::LocalPrompt as i32],
            host_name: self.shared.config.host_name.clone(),
        }))
        .await;

        // 2. Visible local approval, cancelled if the technician gives up.
        self.cell.apply(SessionEvent::AwaitApproval);
        let prompt = self.shared.services.approval.clone();
        let approval = async move { request_approval(&*prompt, &request).await };
        tokio::pin!(approval);
        let decision = loop {
            tokio::select! {
                d = &mut approval => break d,
                ev = events.recv() => match ev {
                    Some(LinkEvent::Message(g, m)) if g == generation => {
                        if matches!(m.msg, Some(V::Goodbye(_))) {
                            info!("technician cancelled the request");
                            return Exit::Ended(None);
                        }
                    }
                    Some(LinkEvent::Lost(g, _)) if g == generation => {
                        info!("technician left before the request was answered");
                        return Exit::Ended(None);
                    }
                    _ => {}
                },
                cmd = cmds.recv(), if self.cmds_open => match cmd {
                    Some(HostCommand::End) => {
                        self.deny(pb::DenyReason::Policy).await;
                        return Exit::Ended(None);
                    }
                    Some(HostCommand::InjectTransportFailure) => self.inject_fault(),
                    None => self.cmds_open = false,
                },
            }
        };
        match decision {
            ApprovalDecision::Approved => {}
            ApprovalDecision::Rejected => {
                return self
                    .reject(pb::DenyReason::UserRejected, RejectReason::UserRejected)
                    .await;
            }
            ApprovalDecision::TimedOut => {
                return self
                    .reject(pb::DenyReason::Timeout, RejectReason::ApprovalTimeout)
                    .await;
            }
            ApprovalDecision::Unavailable(e) => {
                return self
                    .reject(pb::DenyReason::Policy, RejectReason::ApprovalUnavailable(e))
                    .await;
            }
        }
        info!("session approved by the local user");
        self.cell.apply(SessionEvent::Approved);
        let token = self.rotate_resume_token();
        self.send(H::AuthStatus(pb::AuthStatus {
            state: pb::AuthState::Granted as i32,
            resume_token: token,
            ..Default::default()
        }))
        .await;

        // 3. Media and input channels.
        let (media_tx, mut media) = mpsc::channel(64);
        let rtt_ms = conn.path().rtt.as_secs_f64() * 1000.0;
        let controller =
            AdaptiveController::new(self.shared.config.quality, rtt_ms, Instant::now());
        let initial = controller.decision();
        self.controller = Some(controller);
        self.last_decision = Some(initial);
        let video = self.shared.services.video.clone();
        let params = VideoStartParams {
            display_id: self.shared.config.display_id,
            initial,
            in_flight: self.in_flight.counter(),
            events: media_tx,
        };
        match tokio::task::spawn_blocking(move || video.start(params)).await {
            Ok(Ok(p)) => self.pipeline = Some(p),
            Ok(Err(e)) => return Exit::Failed(format!("video: {e}")),
            Err(e) => return Exit::Failed(format!("video: {e}")),
        }
        let ctx = self.media_ctx();
        if let Some(link) = &mut self.link {
            link.start_media(&ctx);
        }

        // 4. Live session.
        self.main_loop(events, cmds, resumes, &mut media).await
    }

    async fn main_loop(
        &mut self,
        events: &mut mpsc::Receiver<LinkEvent<pb::ViewerToHost>>,
        cmds: &mut mpsc::UnboundedReceiver<HostCommand>,
        resumes: &mut mpsc::Receiver<ResumeRequest>,
        media: &mut mpsc::Receiver<MediaEvent>,
    ) -> Exit {
        let cfg = self.shared.config.clone();
        let mut stats_tick = tokio::time::interval(cfg.stats_interval);
        let mut clip_tick = tokio::time::interval(cfg.clipboard_poll);
        stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        clip_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let establish_deadline = tokio::time::Instant::now() + cfg.establish_timeout;
        loop {
            let reconnect_deadline = self.reconnect_deadline;
            let establishing = self.cell.state() == SessionState::Establishing;
            tokio::select! {
                ev = events.recv() => {
                    if let Some(exit) = self.on_link_event(ev).await {
                        return exit;
                    }
                }
                ev = media.recv() => {
                    if let Some(exit) = self.on_media(ev).await {
                        return exit;
                    }
                }
                Some(req) = resumes.recv() => self.on_resume(req).await,
                cmd = cmds.recv(), if self.cmds_open => match cmd {
                    Some(HostCommand::End) => {
                        return Exit::Ended(Some(goodbye(
                            pb::GoodbyeReason::HostUserEnded,
                            "ended on the host",
                        )));
                    }
                    Some(HostCommand::InjectTransportFailure) => self.inject_fault(),
                    None => self.cmds_open = false,
                },
                _ = stats_tick.tick() => self.on_tick().await,
                _ = clip_tick.tick() => self.poll_clipboard().await,
                _ = sleep_until_opt(reconnect_deadline) => {
                    return Exit::Failed("technician did not resume the session in time".into());
                }
                _ = tokio::time::sleep_until(establish_deadline), if establishing => {
                    return Exit::Failed("media did not start in time".into());
                }
            }
        }
    }

    async fn on_link_event(&mut self, ev: Option<LinkEvent<pb::ViewerToHost>>) -> Option<Exit> {
        match ev? {
            LinkEvent::Message(g, m) if g == self.generation => self.on_message(m).await,
            LinkEvent::Lost(g, reason) if g == self.generation => self.on_link_lost(&reason).await,
            _ => None,
        }
    }

    async fn on_message(&mut self, m: pb::ViewerToHost) -> Option<Exit> {
        match m.msg? {
            V::Goodbye(g) => {
                info!(reason = g.reason, "technician ended the session");
                return Some(Exit::Ended(None));
            }
            V::ReceiverReport(r) => {
                self.viewer_dropped_total += u64::from(r.frames_dropped);
                self.last_report = Some(r);
                self.fresh_report = true;
            }
            V::KeyframeRequest(_) => self.request_keyframe(false),
            V::Clipboard(c) => {
                if self.permissions().clipboard
                    && self.live()
                    && let Err(e) = self.clipboard.apply_remote(c.text).await
                {
                    debug!(error = %e, "clipboard text not applied");
                }
            }
            V::Ping(p) => {
                if let Some(link) = &self.link {
                    link.try_send(H::Pong(pb::Pong {
                        id: p.id,
                        echo_sent_us: p.sent_us,
                    }));
                }
            }
            V::Quality(q) => {
                if let Some(c) = &mut self.controller {
                    c.set_mode(quality_mode(q.mode));
                }
            }
            V::Pong(_)
            | V::SelectDisplay(_)
            | V::SpecialAction(_)
            | V::Hello(_)
            | V::PasswordAuth(_) => {
                debug!("control message not handled by this host");
            }
        }
        None
    }

    /// Detaches the current link: input released, media paused, connection closed.
    async fn suspend_link(&mut self, code: u32, reason: &str) {
        self.set_input_enabled(false);
        if let Some(p) = &self.pipeline {
            p.command(MediaCommand::Pause);
        }
        self.in_flight.clear();
        self.loss.reset();
        if let Some(link) = self.link.take() {
            link.close(code, reason).await;
        }
    }

    async fn on_link_lost(&mut self, reason: &str) -> Option<Exit> {
        let state = self.cell.state();
        if state.is_live() {
            warn!(
                reason,
                "connection lost; waiting for the technician to resume"
            );
            self.cell.apply(SessionEvent::ConnectionLost);
            self.suspend_link(CLOSE_FAULT, "connection lost").await;
            self.reconnect_deadline =
                Some(tokio::time::Instant::now() + self.shared.config.resume_grace);
            None
        } else if state == SessionState::Establishing {
            Some(Exit::Failed(format!(
                "connection lost while establishing: {reason}"
            )))
        } else {
            None
        }
    }

    async fn on_resume(&mut self, req: ResumeRequest) {
        let state = self.cell.state();
        let key_ok = self
            .auth
            .as_ref()
            .is_some_and(|a| a.technician.key == req.conn.remote_key());
        let presented = sha256_hex(req.token.as_bytes());
        let token_ok = self
            .resume_hash
            .as_deref()
            .is_some_and(|h| ct_eq(h.as_bytes(), presented.as_bytes()));
        let state_ok = matches!(
            state,
            SessionState::Reconnecting | SessionState::Connected | SessionState::Degraded
        );
        if !(key_ok && token_ok && state_ok) {
            warn!(key_ok, token_ok, state = state.as_str(), "resume refused");
            refuse(&req.conn, req.send, pb::DenyReason::InvalidTicket).await;
            return;
        }
        if state.is_live() {
            // The technician noticed the loss first; the old connection is a zombie.
            self.cell.apply(SessionEvent::ConnectionLost);
            self.suspend_link(CLOSE_SUPERSEDED, "superseded by a resumed connection")
                .await;
        }
        let generation = self.next_generation();
        let mut link = HostLink::new(
            generation,
            req.conn,
            req.send,
            req.recv,
            self.events_tx.clone(),
        );
        let token = self.rotate_resume_token();
        link.send(H::HelloAck(pb::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            host_version: aionix_shared::VERSION.into(),
            auth_state: pb::AuthState::Granted as i32,
            methods: vec![pb::AuthMethod::LocalPrompt as i32],
            host_name: self.shared.config.host_name.clone(),
        }))
        .await;
        link.send(H::AuthStatus(pb::AuthStatus {
            state: pb::AuthState::Granted as i32,
            resume_token: token,
            ..Default::default()
        }))
        .await;
        link.start_media(&self.media_ctx());
        if let Some(s) = &self.stream {
            link.send(H::StreamStarted(*s)).await;
        }
        self.link = Some(link);
        if let Some(p) = &self.pipeline {
            p.command(MediaCommand::Resume);
        }
        self.reconnect_deadline = None;
        self.cell.apply(SessionEvent::Reconnected);
        self.set_input_enabled(true);
        info!(generation, "session resumed");
    }

    async fn on_media(&mut self, ev: Option<MediaEvent>) -> Option<Exit> {
        let Some(ev) = ev else {
            return Some(Exit::Failed("video pipeline stopped".into()));
        };
        match ev {
            MediaEvent::Display(d) => self.input.lock().unwrap().set_display(d),
            MediaEvent::Stream(s) => {
                let res = Resolution {
                    width: s.width,
                    height: s.height,
                };
                self.cell
                    .update_diagnostics(|d| d.resolution = Metric::Available(res));
                self.stream = Some(s);
                if let Some(link) = &self.link {
                    link.send(H::StreamStarted(s)).await;
                    if self.cell.state() == SessionState::Establishing {
                        self.cell.apply(SessionEvent::Established);
                        self.set_input_enabled(true);
                    }
                }
            }
            MediaEvent::Frame(f) => {
                if self.live()
                    && let Some(tx) = self.link.as_ref().and_then(|l| l.video.as_ref())
                    && tx.try_send(f).is_err()
                {
                    // The viewer would see a gap: restart from a keyframe.
                    self.request_keyframe(true);
                }
            }
            MediaEvent::CursorShape(c) => {
                if self.live()
                    && let Some(link) = &self.link
                {
                    link.try_send(H::CursorShape(c));
                }
            }
            MediaEvent::CursorPosition(p) => {
                if self.live()
                    && let Some(link) = &self.link
                {
                    link.try_send(H::CursorPosition(p));
                }
            }
            MediaEvent::Stats(s) => self.stats = Some(s),
            MediaEvent::Failed(e) => return Some(Exit::Failed(format!("video: {e}"))),
        }
        None
    }

    async fn on_tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().max(1e-3);
        self.last_tick = now;

        // Pipeline-side diagnostics (valid even while reconnecting).
        let stats = self.stats.clone();
        let report = self.last_report;
        let dropped = self.viewer_dropped_total;
        self.cell.update_diagnostics(|d| {
            d.decoder = Metric::Unavailable(Unavailable::NotReportedByPeer);
            if let Some(s) = &stats {
                d.capture_backend = Metric::Available(s.capture_backend.clone());
                if !s.encoder.is_empty() {
                    d.encoder = Metric::Available(s.encoder.clone());
                    d.hardware_encoder = Metric::Available(s.hardware_encoder);
                }
                d.fps = Metric::Available(s.fps);
                if s.frames_encoded > 0 && s.encode_us_avg > 0 {
                    d.encode_latency_ms = Metric::Available(f64::from(s.encode_us_avg) / 1000.0);
                }
            }
            match &report {
                Some(r) => {
                    if r.frames_decoded > 0 {
                        d.decode_latency_ms =
                            Metric::Available(f64::from(r.decode_us_avg) / 1000.0);
                    }
                    d.dropped_frames = Metric::Available(dropped);
                }
                None => {
                    d.decode_latency_ms = Metric::Unavailable(Unavailable::NotReportedByPeer);
                    d.dropped_frames = Metric::Unavailable(Unavailable::NotReportedByPeer);
                }
            }
        });

        let path = match &self.link {
            Some(link) if self.live() => link.conn.path(),
            _ => return,
        };
        let rtt_ms = path.rtt.as_secs_f64() * 1000.0;
        let loss = self.loss.update(path.lost_packets, path.sent_packets);
        let sent = self.sent_bytes.load(Ordering::Relaxed);
        let kbps = sent.saturating_sub(self.last_sent_bytes) as f64 * 8.0 / 1000.0 / dt;
        self.last_sent_bytes = sent;
        let fresh = if self.fresh_report { report } else { None };
        self.fresh_report = false;
        let sample = NetworkSample {
            rtt_ms,
            loss_ratio: loss.unwrap_or(0.0),
            in_flight_frames: self.in_flight.len(),
            oldest_in_flight_ms: self.in_flight.oldest_age().as_secs_f64() * 1000.0,
            sent_kbps: kbps,
            encode_ms: stats
                .as_ref()
                .map(|s| f64::from(s.encode_us_avg) / 1000.0)
                .unwrap_or(0.0),
            decode_ms: fresh
                .as_ref()
                .map(|r| f64::from(r.decode_us_avg) / 1000.0)
                .unwrap_or(0.0),
            decode_queue: fresh.as_ref().map(|r| r.decode_queue).unwrap_or(0),
            viewer_dropped: fresh.as_ref().map(|r| r.frames_dropped).unwrap_or(0),
        };
        let mut congested = false;
        if let Some(c) = &mut self.controller {
            let d = c.update(now, &sample);
            congested = c.condition() == Condition::Congested;
            if self.last_decision != Some(d) {
                self.last_decision = Some(d);
                if let Some(p) = &self.pipeline {
                    p.command(MediaCommand::Apply(d));
                }
            }
        }
        if congested {
            self.congested_ticks += 1;
            self.good_ticks = 0;
        } else {
            self.good_ticks += 1;
            self.congested_ticks = 0;
        }
        match self.cell.state() {
            SessionState::Connected if self.congested_ticks >= DEGRADE_AFTER => {
                self.cell.apply(SessionEvent::Degrade);
            }
            SessionState::Degraded if self.good_ticks >= RECOVER_AFTER => {
                self.cell.apply(SessionEvent::Recover);
            }
            _ => {}
        }

        let decision = self.last_decision.unwrap_or(Decision {
            bitrate_kbps: 0,
            fps: 0,
            max_pixels: 0,
            max_in_flight: 0,
        });
        let s = stats.unwrap_or_default();
        if let Some(link) = &self.link {
            link.try_send(H::SenderReport(pb::SenderReport {
                fps: s.fps.round() as u32,
                target_fps: decision.fps,
                bitrate_kbps: kbps.round() as u32,
                target_bitrate_kbps: decision.bitrate_kbps,
                capture_us_avg: s.capture_us_avg,
                encode_us_avg: s.encode_us_avg,
                encoder_name: s.encoder,
                hardware_encoder: s.hardware_encoder,
                width: s.width,
                height: s.height,
                frames_skipped: s.frames_skipped as u32,
                cpu_percent: 0,
                rtt_ms: rtt_ms.round() as u32,
                lost_packets_permille: loss.map(|l| (l * 1000.0).round() as u32).unwrap_or(0),
                capture_backend: s.capture_backend,
                congested,
            }));
        }
        self.cell.update_diagnostics(|d| {
            d.transport = Metric::Available(path.kind);
            d.rtt_ms = Metric::Available(rtt_ms);
            d.bitrate_kbps = Metric::Available(kbps);
            d.packet_loss = Metric::from_option(loss, Unavailable::NotMeasuredYet);
        });
    }

    async fn poll_clipboard(&mut self) {
        if !self.live() || !self.permissions().clipboard {
            return;
        }
        if let Some(text) = self.clipboard.poll().await
            && let Some(link) = &self.link
        {
            link.send(H::Clipboard(pb::ClipboardData { text })).await;
        }
    }

    async fn teardown(mut self, exit: Exit) {
        let (goodbye_msg, code, reason) = match exit {
            Exit::Rejected => (None, CLOSE_REJECTED, "rejected"),
            Exit::Ended(g) => {
                self.cell.apply(SessionEvent::End);
                (g, CLOSE_NORMAL, "session ended")
            }
            Exit::Failed(e) => {
                warn!(error = %e, "session failed");
                self.cell.apply(SessionEvent::Fail(e.clone()));
                (
                    Some(goodbye(pb::GoodbyeReason::HostClosed, &e)),
                    CLOSE_NORMAL,
                    "session failed",
                )
            }
        };
        self.set_input_enabled(false);
        if let Some(p) = self.pipeline.take() {
            let _ = tokio::task::spawn_blocking(move || p.stop()).await;
        }
        if let Some(link) = self.link.take() {
            if let Some(g) = goodbye_msg {
                link.send(H::Goodbye(g)).await;
            }
            link.close(code, reason).await;
        }
        self.cell.finish();
        info!(state = self.cell.state().as_str(), "host session finished");
    }
}

async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

fn quality_mode(mode: i32) -> QualityMode {
    match pb::QualityMode::try_from(mode).unwrap_or(pb::QualityMode::Automatic) {
        pb::QualityMode::Automatic => QualityMode::Automatic,
        pb::QualityMode::PreferSpeed => QualityMode::PreferSpeed,
        pb::QualityMode::Balanced => QualityMode::Balanced,
        pb::QualityMode::PreferQuality => QualityMode::PreferQuality,
        pb::QualityMode::Original => QualityMode::Original,
    }
}
