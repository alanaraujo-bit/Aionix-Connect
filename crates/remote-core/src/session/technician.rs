//! Technician-side session coordinator.
//!
//! ```text
//! request ticket (authority) ─▶ connect to the host key ─▶ Hello(ticket)
//!   ─▶ HelloAck(PENDING) ─▶ host shows the prompt ─▶ AuthStatus(GRANTED | DENIED)
//!   ─▶ video streams ─▶ decoder thread ─▶ FrameConsumer
//!   ─▶ input / clipboard / receiver reports ─▶ host
//!   ─▶ (link lost ─▶ Reconnecting ─▶ bounded resume attempts ─▶ Connected | Failed)
//!   ─▶ Goodbye ─▶ Ended
//! ```

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aionix_control_api::claims::SessionPermission;
use aionix_protocol::{ALPN, PROTOCOL_VERSION, pb};
use aionix_shared::AccessMode;
use aionix_transport::{PeerConnection, RecvStream, SendStream, Transport};
use tokio::{
    sync::{Notify, mpsc, watch},
    task::JoinHandle,
};
use tracing::{debug, info, warn};

use super::{
    StateCell,
    authority::{
        AuthorityError, AuthorizationProvider, DeviceLookup, Password, SessionGrant,
        SessionRequest, TechnicianIdentity,
    },
    diagnostics::{LossMeter, Metric, Resolution, SessionDiagnostics, SessionRole, Unavailable},
    link::{
        CLOSE_FAULT, CLOSE_NORMAL, ControlWriter, LinkEvent, MAX_VIDEO_STREAM, decode_video_frame,
        goodbye, spawn_reader, viewer_msg, write_input,
    },
    media::{ClipboardAccess, ClipboardChannel, DecodeWorker, FrameConsumer, VideoDecoderFactory},
    reconnect::ReconnectPolicy,
    state::{RejectReason, SessionEvent, SessionState, SessionStatus},
    ticket::Permissions,
    wait_for_status,
};

use pb::{host_to_viewer::Msg as H, viewer_to_host::Msg as V};

#[derive(Debug, Clone)]
pub struct TechnicianConfig {
    pub identity: TechnicianIdentity,
    pub target: DeviceLookup,
    /// Requested permissions; the ticket may grant fewer.
    pub permissions: Vec<SessionPermission>,
    /// Set for unattended access (ADR-0015): the device password. Without it the
    /// session is assisted and the person at the computer must click "Permitir".
    pub unattended_password: Option<Password>,
    pub reconnect: ReconnectPolicy,
    pub connect_timeout: Duration,
    /// Longest wait for the host user's answer (the host refuses on its own earlier).
    pub approval_wait: Duration,
    /// Time from approval until the first stream must have started.
    pub establish_timeout: Duration,
    pub report_interval: Duration,
    pub clipboard_poll: Duration,
    pub capabilities: pb::Capabilities,
}

impl TechnicianConfig {
    pub fn new(identity: TechnicianIdentity, target: DeviceLookup) -> Self {
        Self {
            identity,
            target,
            permissions: Permissions::all().to_claims(),
            unattended_password: None,
            reconnect: ReconnectPolicy::default(),
            connect_timeout: Duration::from_secs(20),
            approval_wait: Duration::from_secs(150),
            establish_timeout: Duration::from_secs(20),
            report_interval: Duration::from_secs(1),
            clipboard_poll: Duration::from_millis(500),
            capabilities: pb::Capabilities {
                decoders: vec![pb::VideoCodec::H264 as i32],
                clipboard_text: true,
                max_width: 3840,
                max_height: 2160,
                max_fps: 60,
            },
        }
    }
}

pub struct TechnicianServices {
    pub authority: Arc<dyn AuthorizationProvider>,
    pub decoder: Arc<dyn VideoDecoderFactory>,
    pub frames: Box<dyn FrameConsumer>,
    pub clipboard: Arc<dyn ClipboardAccess>,
}

/// Session events for the technician UI.
#[derive(Debug, Clone)]
pub enum TechnicianEvent {
    HostName(String),
    StreamStarted(pb::StreamStarted),
    CursorShape(pb::CursorShape),
    CursorPosition(pb::CursorPosition),
    Notice(pb::Notice),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InputRejected {
    #[error("the session does not grant control")]
    NotPermitted,
    #[error("the session is not connected")]
    NotConnected,
    #[error("input queue full")]
    Backpressure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TechCommand {
    End,
    InjectTransportFailure,
}

/// Observes and controls a technician session.
#[derive(Clone)]
pub struct TechnicianSessionHandle {
    cell: Arc<StateCell>,
    cmds: mpsc::UnboundedSender<TechCommand>,
    input: mpsc::Sender<pb::InputEvent>,
    granted: Arc<Mutex<Option<Permissions>>>,
    events: Arc<Mutex<Option<mpsc::Receiver<TechnicianEvent>>>>,
}

impl TechnicianSessionHandle {
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

    pub fn diagnostics(&self) -> SessionDiagnostics {
        self.cell.diagnostics()
    }

    /// Permissions granted by the ticket (known once the authority answered).
    pub fn permissions(&self) -> Option<Permissions> {
        *self.granted.lock().unwrap()
    }

    /// Sends one input event. Only allowed while connected and if `control` was granted.
    pub fn send_input(&self, event: pb::InputEvent) -> Result<(), InputRejected> {
        if !self.permissions().is_some_and(|p| p.control) {
            return Err(InputRejected::NotPermitted);
        }
        if !self.cell.state().is_live() {
            return Err(InputRejected::NotConnected);
        }
        self.input
            .try_send(event)
            .map_err(|_| InputRejected::Backpressure)
    }

    /// Takes the event stream (once).
    pub fn take_events(&self) -> Option<mpsc::Receiver<TechnicianEvent>> {
        self.events.lock().unwrap().take()
    }

    pub fn end(&self) {
        let _ = self.cmds.send(TechCommand::End);
    }

    /// Diagnostics/testing: drops the current connection abruptly, as a network
    /// failure would; the session then tries to resume.
    pub fn inject_transport_failure(&self) {
        let _ = self.cmds.send(TechCommand::InjectTransportFailure);
    }
}

pub struct TechnicianSession;

impl TechnicianSession {
    /// Starts the session; follow it through the returned handle.
    pub fn start(
        transport: Transport,
        config: TechnicianConfig,
        services: TechnicianServices,
    ) -> TechnicianSessionHandle {
        let cell = StateCell::new(SessionRole::Technician);
        let (cmds, cmd_rx) = mpsc::unbounded_channel();
        let (input, input_rx) = mpsc::channel(512);
        let (events_out, events_rx) = mpsc::channel(256);
        let granted = Arc::new(Mutex::new(None));
        let handle = TechnicianSessionHandle {
            cell: cell.clone(),
            cmds,
            input,
            granted: granted.clone(),
            events: Arc::new(Mutex::new(Some(events_rx))),
        };
        let (events_tx, link_events) = mpsc::channel(256);
        let tech = Tech {
            transport,
            clipboard: ClipboardChannel::new(services.clipboard.clone()),
            authority: services.authority,
            decoder_factory: services.decoder,
            consumer: Some(services.frames),
            config,
            cell,
            events_out,
            events_tx,
            generation: 0,
            link: None,
            grant: None,
            granted,
            resume_token: None,
            decode: None,
            keyframe_needed: Arc::new(Notify::new()),
            input_rx: Arc::new(tokio::sync::Mutex::new(input_rx)),
            sender_report: None,
            loss: LossMeter::default(),
            last_report_at: Instant::now(),
            last_keyframe_request: None,
            cmds_open: true,
        };
        tokio::spawn(tech.run(link_events, cmd_rx));
        handle
    }
}

/// One connection to the host.
struct TechLink {
    conn: PeerConnection,
    control: Option<ControlWriter<pb::ViewerToHost>>,
    tasks: Vec<JoinHandle<()>>,
}

impl TechLink {
    fn new(
        generation: u64,
        conn: PeerConnection,
        send: SendStream,
        recv: RecvStream,
        events: mpsc::Sender<LinkEvent<pb::HostToViewer>>,
    ) -> Self {
        Self {
            control: Some(ControlWriter::spawn(send)),
            tasks: vec![spawn_reader(generation, recv, events)],
            conn,
        }
    }

    async fn send(&self, msg: V) -> bool {
        match &self.control {
            Some(c) => c.send(viewer_msg(msg)).await,
            None => false,
        }
    }

    fn try_send(&self, msg: V) -> bool {
        self.control
            .as_ref()
            .is_some_and(|c| c.try_send(viewer_msg(msg)))
    }

    async fn close(mut self, code: u32, reason: &str) {
        for t in &self.tasks {
            t.abort();
        }
        if let Some(c) = self.control.take() {
            c.close().await;
        }
        self.conn.close(code, reason);
    }
}

impl Drop for TechLink {
    fn drop(&mut self) {
        // Covers links abandoned mid-handshake (e.g. a resume attempt timing out).
        for t in &self.tasks {
            t.abort();
        }
        if self.control.is_some() {
            self.conn.close(CLOSE_NORMAL, "abandoned");
        }
    }
}

enum Exit {
    Rejected,
    Ended(Option<pb::Goodbye>),
    Failed(String),
}

enum Resume {
    Resumed,
    Refused,
}

struct Tech {
    transport: Transport,
    config: TechnicianConfig,
    authority: Arc<dyn AuthorizationProvider>,
    decoder_factory: Arc<dyn VideoDecoderFactory>,
    consumer: Option<Box<dyn FrameConsumer>>,
    clipboard: ClipboardChannel,
    cell: Arc<StateCell>,
    events_out: mpsc::Sender<TechnicianEvent>,
    events_tx: mpsc::Sender<LinkEvent<pb::HostToViewer>>,
    generation: u64,
    link: Option<TechLink>,
    grant: Option<SessionGrant>,
    granted: Arc<Mutex<Option<Permissions>>>,
    resume_token: Option<String>,
    decode: Option<DecodeWorker>,
    keyframe_needed: Arc<Notify>,
    input_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<pb::InputEvent>>>,
    sender_report: Option<pb::SenderReport>,
    loss: LossMeter,
    last_report_at: Instant,
    last_keyframe_request: Option<Instant>,
    cmds_open: bool,
}

const KEYFRAME_MIN_INTERVAL: Duration = Duration::from_millis(300);
const MAX_INPUT_BATCH_EVENTS: usize = 64;

impl Tech {
    fn permissions(&self) -> Permissions {
        self.granted.lock().unwrap().unwrap_or_default()
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn live(&self) -> bool {
        self.cell.state().is_live()
    }

    fn hello(&self, ticket: String, resume_token: String) -> V {
        V::Hello(pb::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_version: aionix_shared::VERSION.into(),
            session_ticket: ticket,
            capabilities: Some(self.config.capabilities.clone()),
            resume_token,
        })
    }

    async fn run(
        mut self,
        mut events: mpsc::Receiver<LinkEvent<pb::HostToViewer>>,
        mut cmds: mpsc::UnboundedReceiver<TechCommand>,
    ) {
        let exit = self.drive(&mut events, &mut cmds).await;
        self.teardown(exit).await;
    }

    async fn drive(
        &mut self,
        events: &mut mpsc::Receiver<LinkEvent<pb::HostToViewer>>,
        cmds: &mut mpsc::UnboundedReceiver<TechCommand>,
    ) -> Exit {
        // 1. Ask the authority for a ticket.
        self.cell.apply(SessionEvent::Start);
        let request = SessionRequest {
            technician: self.config.identity.clone(),
            target: self.config.target,
            mode: if self.config.unattended_password.is_some() {
                AccessMode::Unattended
            } else {
                AccessMode::Assisted
            },
            permissions: self.config.permissions.clone(),
            unattended_password: self.config.unattended_password.clone(),
        };
        let authority = self.authority.clone();
        let grant = tokio::select! {
            g = authority.request_session(request) => g,
            Some(TechCommand::End) = cmds.recv() => return Exit::Ended(None),
        };
        let grant = match grant {
            Ok(g) => g,
            Err(AuthorityError::Forbidden(reason)) => {
                self.cell
                    .apply(SessionEvent::Reject(RejectReason::Policy(reason)));
                return Exit::Rejected;
            }
            Err(e) => return Exit::Failed(format!("authorization: {e}")),
        };
        let sid = grant.session_id;
        self.cell
            .update_diagnostics(|d| d.session_id = Metric::Available(sid));
        *self.granted.lock().unwrap() = Some(Permissions::from_claims(&grant.permissions));

        // 2. Reach the host; the transport authenticates its key.
        let connect = self
            .transport
            .connect(&grant.host_key, &grant.host_addr, ALPN);
        let conn = match tokio::time::timeout(self.config.connect_timeout, connect).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return Exit::Failed(format!("connect: {e}")),
            Err(_) => return Exit::Failed("connect: timed out".into()),
        };
        if conn.remote_key() != grant.host_key {
            conn.close(CLOSE_NORMAL, "unexpected peer");
            return Exit::Failed("connected peer is not the authorized host".into());
        }
        let (send, recv) = match conn.open_bi().await {
            Ok(s) => s,
            Err(e) => return Exit::Failed(format!("control stream: {e}")),
        };
        let generation = self.next_generation();
        let link = TechLink::new(generation, conn, send, recv, self.events_tx.clone());
        link.send(self.hello(grant.ticket.clone(), String::new()))
            .await;
        self.link = Some(link);
        self.grant = Some(grant);
        self.cell.apply(SessionEvent::Authenticate);

        // 3. Wait for authorization and the local user's decision on the host.
        let deadline = tokio::time::Instant::now() + self.config.approval_wait;
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Some(LinkEvent::Message(g, m)) if g == generation => match m.msg {
                        Some(H::HelloAck(ack)) => {
                            if !ack.host_name.is_empty() {
                                let _ = self.events_out.try_send(TechnicianEvent::HostName(ack.host_name));
                            }
                            if ack.auth_state == pb::AuthState::Pending as i32
                                && self.cell.state() == SessionState::Authenticating
                            {
                                self.cell.apply(SessionEvent::AwaitApproval);
                            }
                        }
                        Some(H::AuthStatus(s)) if s.state == pb::AuthState::Granted as i32 => {
                            if self.cell.state() == SessionState::Authenticating {
                                self.cell.apply(SessionEvent::AwaitApproval);
                            }
                            self.resume_token = (!s.resume_token.is_empty()).then_some(s.resume_token);
                            self.cell.apply(SessionEvent::Approved);
                            break;
                        }
                        Some(H::AuthStatus(s)) if s.state == pb::AuthState::Denied as i32 => {
                            let reason = deny_reason(s.reason);
                            info!(?reason, "session refused by the host");
                            self.cell.apply(SessionEvent::Reject(reason));
                            return Exit::Rejected;
                        }
                        Some(H::Goodbye(g)) => {
                            return Exit::Failed(format!("host closed the request: {}", g.detail));
                        }
                        _ => {}
                    },
                    Some(LinkEvent::Lost(g, e)) if g == generation => {
                        return Exit::Failed(format!("connection lost before authorization: {e}"));
                    }
                    _ => {}
                },
                cmd = cmds.recv(), if self.cmds_open => match cmd {
                    Some(TechCommand::End) => {
                        return Exit::Ended(Some(goodbye(pb::GoodbyeReason::ViewerClosed, "request cancelled")));
                    }
                    Some(TechCommand::InjectTransportFailure) => self.inject_fault(),
                    None => self.cmds_open = false,
                },
                _ = tokio::time::sleep_until(deadline) => {
                    return Exit::Failed("no answer from the host".into());
                }
            }
        }

        // 4. Media: decoder thread and per-link stream tasks.
        let Some(consumer) = self.consumer.take() else {
            return Exit::Failed("frame consumer missing".into());
        };
        match DecodeWorker::start(self.decoder_factory.clone(), consumer) {
            Ok(w) => {
                self.keyframe_needed = w.keyframe_needed();
                self.decode = Some(w);
            }
            Err(e) => return Exit::Failed(format!("decoder: {e}")),
        }
        self.start_link_media();
        self.main_loop(events, cmds).await
    }

    fn start_link_media(&mut self) {
        let control = self.permissions().control;
        let Some(link) = &mut self.link else { return };
        if let Some(decode) = &self.decode {
            link.tasks
                .push(tokio::spawn(video_task(link.conn.clone(), decode.sender())));
        }
        if control {
            link.tasks.push(tokio::spawn(input_task(
                link.conn.clone(),
                self.input_rx.clone(),
            )));
        }
    }

    async fn main_loop(
        &mut self,
        events: &mut mpsc::Receiver<LinkEvent<pb::HostToViewer>>,
        cmds: &mut mpsc::UnboundedReceiver<TechCommand>,
    ) -> Exit {
        let mut report_tick = tokio::time::interval(self.config.report_interval);
        let mut clip_tick = tokio::time::interval(self.config.clipboard_poll);
        report_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        clip_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let establish_deadline = tokio::time::Instant::now() + self.config.establish_timeout;
        loop {
            let establishing = self.cell.state() == SessionState::Establishing;
            let keyframe = self.keyframe_needed.clone();
            tokio::select! {
                ev = events.recv() => match ev {
                    Some(LinkEvent::Message(g, m)) if g == self.generation => {
                        if let Some(exit) = self.on_message(m).await {
                            return exit;
                        }
                    }
                    Some(LinkEvent::Lost(g, reason)) if g == self.generation => {
                        if establishing {
                            return Exit::Failed(format!("connection lost while establishing: {reason}"));
                        }
                        if self.live() {
                            warn!(reason, "connection lost; resuming");
                            if let Some(exit) = self.reconnect(events, cmds).await {
                                return exit;
                            }
                        }
                    }
                    _ => {}
                },
                cmd = cmds.recv(), if self.cmds_open => match cmd {
                    Some(TechCommand::End) => {
                        return Exit::Ended(Some(goodbye(pb::GoodbyeReason::ViewerClosed, "ended by the technician")));
                    }
                    Some(TechCommand::InjectTransportFailure) => self.inject_fault(),
                    None => self.cmds_open = false,
                },
                _ = keyframe.notified() => self.request_keyframe(),
                _ = report_tick.tick() => self.on_report_tick(),
                _ = clip_tick.tick() => self.poll_clipboard().await,
                _ = tokio::time::sleep_until(establish_deadline), if establishing => {
                    return Exit::Failed("the host did not start a video stream".into());
                }
            }
        }
    }

    async fn on_message(&mut self, m: pb::HostToViewer) -> Option<Exit> {
        match m.msg? {
            H::StreamStarted(s) => {
                let res = Resolution {
                    width: s.width,
                    height: s.height,
                };
                self.cell
                    .update_diagnostics(|d| d.resolution = Metric::Available(res));
                let _ = self.events_out.try_send(TechnicianEvent::StreamStarted(s));
                if self.cell.state() == SessionState::Establishing {
                    self.cell.apply(SessionEvent::Established);
                }
            }
            H::CursorShape(c) => {
                let _ = self.events_out.try_send(TechnicianEvent::CursorShape(c));
            }
            H::CursorPosition(p) => {
                let _ = self.events_out.try_send(TechnicianEvent::CursorPosition(p));
            }
            H::Notice(n) => {
                let _ = self.events_out.try_send(TechnicianEvent::Notice(n));
            }
            H::Clipboard(c) => {
                if self.permissions().clipboard
                    && let Err(e) = self.clipboard.apply_remote(c.text).await
                {
                    debug!(error = %e, "clipboard text not applied");
                }
            }
            H::SenderReport(r) => {
                match self.cell.state() {
                    SessionState::Connected if r.congested => {
                        self.cell.apply(SessionEvent::Degrade);
                    }
                    SessionState::Degraded if !r.congested => {
                        self.cell.apply(SessionEvent::Recover);
                    }
                    _ => {}
                }
                self.sender_report = Some(r);
            }
            H::Ping(p) => {
                if let Some(link) = &self.link {
                    link.try_send(V::Pong(pb::Pong {
                        id: p.id,
                        echo_sent_us: p.sent_us,
                    }));
                }
            }
            H::Goodbye(g) => {
                info!(reason = g.reason, detail = %g.detail, "host ended the session");
                return Some(Exit::Ended(None));
            }
            H::HelloAck(_) | H::AuthStatus(_) | H::Displays(_) | H::Pong(_) | H::SystemInfo(_) => {}
        }
        None
    }

    fn inject_fault(&self) {
        if let Some(link) = &self.link {
            warn!("injecting transport failure");
            link.conn.close(CLOSE_FAULT, "transport failure injected");
        }
    }

    fn request_keyframe(&mut self) {
        if !self.live()
            || self
                .last_keyframe_request
                .is_some_and(|t| t.elapsed() < KEYFRAME_MIN_INTERVAL)
        {
            return;
        }
        self.last_keyframe_request = Some(Instant::now());
        if let Some(link) = &self.link {
            link.try_send(V::KeyframeRequest(pb::KeyframeRequest {}));
        }
    }

    /// Bounded resumption after an unexpected loss. `None` = resumed.
    async fn reconnect(
        &mut self,
        events: &mut mpsc::Receiver<LinkEvent<pb::HostToViewer>>,
        cmds: &mut mpsc::UnboundedReceiver<TechCommand>,
    ) -> Option<Exit> {
        self.cell.apply(SessionEvent::ConnectionLost);
        if let Some(link) = self.link.take() {
            link.close(CLOSE_FAULT, "connection lost").await;
        }
        // Input typed while disconnected must not be replayed later.
        if let Ok(mut rx) = self.input_rx.try_lock() {
            while rx.try_recv().is_ok() {}
        }
        self.loss.reset();
        let (Some(grant), Some(token)) = (self.grant.clone(), self.resume_token.clone()) else {
            return Some(Exit::Failed("session cannot be resumed".into()));
        };
        let policy = self.config.reconnect;
        for attempt in 1..=policy.max_attempts {
            let delay = tokio::time::sleep(policy.delay(attempt));
            tokio::pin!(delay);
            loop {
                tokio::select! {
                    _ = &mut delay => break,
                    cmd = cmds.recv(), if self.cmds_open => match cmd {
                        Some(TechCommand::End) => return Some(Exit::Ended(None)),
                        Some(TechCommand::InjectTransportFailure) => {}
                        None => self.cmds_open = false,
                    },
                }
            }
            self.cell.record_reconnect_attempt();
            info!(attempt, max = policy.max_attempts, "resuming session");
            let attempt_result = tokio::time::timeout(
                policy.attempt_timeout,
                self.try_resume(&grant, &token, events),
            )
            .await;
            match attempt_result {
                Ok(Ok(Resume::Resumed)) => {
                    self.cell.apply(SessionEvent::Reconnected);
                    info!(attempt, "session resumed");
                    return None;
                }
                Ok(Ok(Resume::Refused)) => {
                    return Some(Exit::Failed(
                        "the host refused to resume the session".into(),
                    ));
                }
                Ok(Err(e)) => debug!(attempt, error = %e, "resume attempt failed"),
                Err(_) => debug!(attempt, "resume attempt timed out"),
            }
        }
        Some(Exit::Failed(format!(
            "could not resume the session after {} attempts",
            policy.max_attempts
        )))
    }

    async fn try_resume(
        &mut self,
        grant: &SessionGrant,
        token: &str,
        events: &mut mpsc::Receiver<LinkEvent<pb::HostToViewer>>,
    ) -> Result<Resume, String> {
        let conn = self
            .transport
            .connect(&grant.host_key, &grant.host_addr, ALPN)
            .await
            .map_err(|e| e.to_string())?;
        let (send, recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        let generation = self.next_generation();
        let link = TechLink::new(generation, conn, send, recv, self.events_tx.clone());
        link.send(self.hello(String::new(), token.to_owned())).await;
        loop {
            match events.recv().await {
                Some(LinkEvent::Message(g, m)) if g == generation => match m.msg {
                    Some(H::AuthStatus(s)) if s.state == pb::AuthState::Granted as i32 => {
                        self.resume_token = Some(s.resume_token);
                        self.link = Some(link);
                        self.start_link_media();
                        return Ok(Resume::Resumed);
                    }
                    Some(H::AuthStatus(s)) if s.state == pb::AuthState::Denied as i32 => {
                        link.close(CLOSE_NORMAL, "resume refused").await;
                        return Ok(Resume::Refused);
                    }
                    _ => {}
                },
                Some(LinkEvent::Lost(g, e)) if g == generation => return Err(e),
                Some(_) => {} // Late events of an older connection.
                None => return Err("event channel closed".into()),
            }
        }
    }

    fn on_report_tick(&mut self) {
        let Some(decode) = &self.decode else { return };
        let secs = self.last_report_at.elapsed().as_secs_f64().max(1e-3);
        self.last_report_at = Instant::now();
        let s = decode.take_interval();
        let queue = decode.queue_len();
        let sender = self.sender_report.clone();
        let path = match &self.link {
            Some(link) if self.live() => Some(link.conn.path()),
            _ => None,
        };
        let loss = path
            .as_ref()
            .and_then(|p| self.loss.update(p.lost_packets, p.sent_packets));
        self.cell.update_diagnostics(|d| {
            if let Some(name) = &s.decoder {
                d.decoder = Metric::Available(name.clone());
            }
            d.capture_backend = Metric::Unavailable(Unavailable::NotReportedByPeer);
            d.encoder = Metric::Unavailable(Unavailable::NotReportedByPeer);
            d.hardware_encoder = Metric::Unavailable(Unavailable::NotReportedByPeer);
            d.encode_latency_ms = Metric::Unavailable(Unavailable::NotReportedByPeer);
            if let Some(r) = &sender {
                if !r.capture_backend.is_empty() {
                    d.capture_backend = Metric::Available(r.capture_backend.clone());
                }
                if !r.encoder_name.is_empty() {
                    d.encoder = Metric::Available(r.encoder_name.clone());
                    d.hardware_encoder = Metric::Available(r.hardware_encoder);
                }
                if r.encode_us_avg > 0 {
                    d.encode_latency_ms = Metric::Available(f64::from(r.encode_us_avg) / 1000.0);
                }
            }
            if s.width > 0 {
                d.resolution = Metric::Available(Resolution {
                    width: s.width,
                    height: s.height,
                });
            }
            d.fps = Metric::Available(f64::from(s.frames_decoded) / secs);
            d.bitrate_kbps = Metric::Available(s.bytes_received as f64 * 8.0 / 1000.0 / secs);
            if s.frames_decoded > 0 {
                d.decode_latency_ms = Metric::Available(
                    s.decode_us_total as f64 / f64::from(s.frames_decoded) / 1000.0,
                );
            }
            d.dropped_frames = Metric::Available(s.total_dropped);
            if let Some(p) = &path {
                d.transport = Metric::Available(p.kind);
                d.rtt_ms = Metric::Available(p.rtt.as_secs_f64() * 1000.0);
                d.packet_loss = Metric::from_option(loss, Unavailable::NotMeasuredYet);
            }
        });
        if let Some(link) = &self.link
            && path.is_some()
        {
            link.try_send(V::ReceiverReport(pb::ReceiverReport {
                stream_epoch: s.last_epoch,
                last_frame_seq: s.last_seq,
                frames_received: s.frames_received,
                frames_decoded: s.frames_decoded,
                frames_dropped: s.frames_dropped,
                decode_us_avg: (s.decode_us_total / u64::from(s.frames_decoded.max(1))) as u32,
                render_us_avg: 0,
                decode_queue: queue,
                bytes_received: s.bytes_received,
                jitter_us: 0,
                interval_ms: (secs * 1000.0) as u32,
            }));
        }
    }

    async fn poll_clipboard(&mut self) {
        if !self.live() || !self.permissions().clipboard {
            return;
        }
        if let Some(text) = self.clipboard.poll().await
            && let Some(link) = &self.link
        {
            link.send(V::Clipboard(pb::ClipboardData { text })).await;
        }
    }

    async fn teardown(mut self, exit: Exit) {
        let goodbye_msg = match exit {
            Exit::Rejected => None,
            Exit::Ended(g) => {
                self.cell.apply(SessionEvent::End);
                g
            }
            Exit::Failed(e) => {
                warn!(error = %e, "session failed");
                self.cell.apply(SessionEvent::Fail(e.clone()));
                Some(goodbye(pb::GoodbyeReason::ViewerClosed, &e))
            }
        };
        if let Some(link) = self.link.take() {
            if let Some(g) = goodbye_msg {
                link.send(V::Goodbye(g)).await;
            }
            link.close(CLOSE_NORMAL, "session ended").await;
        }
        if let Some(decode) = self.decode.take() {
            let _ = tokio::task::spawn_blocking(move || decode.stop()).await;
        }
        self.cell.finish();
        info!(
            state = self.cell.state().as_str(),
            "technician session finished"
        );
    }
}

fn deny_reason(reason: i32) -> RejectReason {
    match pb::DenyReason::try_from(reason).unwrap_or(pb::DenyReason::Unspecified) {
        pb::DenyReason::UserRejected => RejectReason::UserRejected,
        pb::DenyReason::Timeout => RejectReason::ApprovalTimeout,
        pb::DenyReason::InvalidTicket => RejectReason::InvalidTicket("refused by the host".into()),
        pb::DenyReason::Busy => RejectReason::Busy,
        other => RejectReason::Policy(format!("{other:?}")),
    }
}

async fn video_task(conn: PeerConnection, decode: super::media::DecodeSender) {
    loop {
        let Ok(mut stream) = conn.accept_uni().await else {
            return;
        };
        match stream.read_to_end(MAX_VIDEO_STREAM).await {
            Ok(bytes) => match decode_video_frame(bytes) {
                Ok(frame) => decode.push(frame),
                Err(e) => debug!(error = %e, "invalid video frame"),
            },
            Err(e) => debug!(error = %e, "video stream aborted"),
        }
    }
}

async fn input_task(
    conn: PeerConnection,
    input: Arc<tokio::sync::Mutex<mpsc::Receiver<pb::InputEvent>>>,
) {
    let Ok(mut send) = conn.open_uni().await else {
        return;
    };
    let mut rx = input.lock().await;
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while batch.len() < MAX_INPUT_BATCH_EVENTS {
            match rx.try_recv() {
                Ok(e) => batch.push(e),
                Err(_) => break,
            }
        }
        if write_input(&mut send, batch).await.is_err() {
            return;
        }
    }
}
