//! Permanent signaling link of a device, with automatic reconnection.
//!
//! The link state is truthful: `Connecting` until the server sent `Ready` and our
//! reachability was published, `Online` afterwards, `Connecting` again while
//! reconnecting after a loss, `Stopped` for terminal conditions (revoked identity,
//! replaced by another instance of the same device, explicit shutdown).

use std::{sync::Arc, time::Duration};

use aionix_control_api::{
    rest::SessionEventKind,
    signal::{
        ClientMessage, EndpointAddrDto, MAX_RECONNECT_BACKOFF_SECS, OfferRejectReason, OfferViewer,
        ServerMessage, close_code,
    },
};
use aionix_shared::{AccessMode, TransportKind};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest, http::HeaderValue};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::enroll::DeviceSession;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Online,
    /// Terminal: the link will not be re-established.
    Stopped(StopReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Revoked,
    Superseded,
    Shutdown,
}

/// Reachability and local policy published to the server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresenceInfo {
    pub addr: EndpointAddrDto,
    pub busy: bool,
    pub unattended_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct SessionOffer {
    pub session_id: Uuid,
    pub ticket: String,
    pub mode: AccessMode,
    pub viewer: OfferViewer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferDecision {
    pub accept: bool,
    pub reason: Option<OfferRejectReason>,
    pub addr: EndpointAddrDto,
}

/// Application callbacks (host side).
#[async_trait]
pub trait SignalHandler: Send + Sync + 'static {
    async fn on_offer(&self, offer: SessionOffer) -> OfferDecision;
    async fn on_session_revoked(&self, _session_id: Uuid) {}
}

#[derive(Debug, Clone)]
pub struct SignalingOptions {
    pub url: String,
    pub min_backoff: Duration,
    pub max_backoff: Duration,
}

impl SignalingOptions {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            min_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(MAX_RECONNECT_BACKOFF_SECS),
        }
    }
}

enum Command {
    Send(ClientMessage),
    Shutdown,
}

#[derive(Clone)]
pub struct SignalingClient {
    state: watch::Receiver<LinkState>,
    cmds: mpsc::UnboundedSender<Command>,
}

impl SignalingClient {
    pub fn spawn(
        options: SignalingOptions,
        session: Arc<DeviceSession>,
        presence: watch::Receiver<PresenceInfo>,
        handler: Arc<dyn SignalHandler>,
    ) -> Self {
        let (state_tx, state) = watch::channel(LinkState::Connecting);
        let (cmds, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run(options, session, presence, handler, state_tx, cmd_rx));
        Self { state, cmds }
    }

    pub fn state(&self) -> LinkState {
        self.state.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<LinkState> {
        self.state.clone()
    }

    pub async fn wait_for(&self, timeout: Duration, pred: impl Fn(&LinkState) -> bool) -> bool {
        let mut rx = self.state.clone();
        tokio::time::timeout(timeout, rx.wait_for(|s| pred(s)))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// Host-side lifecycle report (approval decision, connected, ended…). Metadata only.
    pub fn report(
        &self,
        session_id: Uuid,
        kind: SessionEventKind,
        transport: Option<TransportKind>,
        detail: &str,
    ) {
        let _ = self.cmds.send(Command::Send(ClientMessage::SessionEvent {
            session_id,
            kind,
            transport,
            detail: detail.chars().take(300).collect(),
        }));
    }

    /// Sends `Goodbye` (offline at once) and stops reconnecting.
    pub fn shutdown(&self) {
        let _ = self.cmds.send(Command::Shutdown);
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

enum LinkEnd {
    Reconnect { reset_backoff: bool },
    ReloginAndReconnect,
    Stop(StopReason),
}

fn jitter(d: Duration) -> Duration {
    let ms = d.as_millis() as u64;
    Duration::from_millis(ms / 2 + rand::random_range(0..=ms / 2 + 1))
}

async fn run(
    options: SignalingOptions,
    session: Arc<DeviceSession>,
    mut presence: watch::Receiver<PresenceInfo>,
    handler: Arc<dyn SignalHandler>,
    state: watch::Sender<LinkState>,
    mut cmds: mpsc::UnboundedReceiver<Command>,
) {
    let mut backoff = options.min_backoff;
    loop {
        state.send_replace(LinkState::Connecting);
        let end = match connect(&options.url, &session).await {
            Ok(ws) => link(ws, &session, &mut presence, &handler, &state, &mut cmds).await,
            Err(ConnectError::Revoked) => LinkEnd::Stop(StopReason::Revoked),
            Err(ConnectError::Unauthorized) => LinkEnd::ReloginAndReconnect,
            Err(ConnectError::Other(e)) => {
                debug!(error = %e, "signaling connect failed");
                LinkEnd::Reconnect {
                    reset_backoff: false,
                }
            }
        };
        match end {
            LinkEnd::Stop(reason) => {
                info!(?reason, "signaling stopped");
                state.send_replace(LinkState::Stopped(reason));
                return;
            }
            LinkEnd::ReloginAndReconnect => session.invalidate().await,
            LinkEnd::Reconnect {
                reset_backoff: true,
            } => backoff = options.min_backoff,
            LinkEnd::Reconnect {
                reset_backoff: false,
            } => {}
        }
        state.send_replace(LinkState::Connecting);
        let wait = jitter(backoff);
        backoff = (backoff * 2).min(options.max_backoff);
        // Shutdown requests are honoured while waiting.
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            cmd = cmds.recv() => {
                if matches!(cmd, Some(Command::Shutdown) | None) {
                    state.send_replace(LinkState::Stopped(StopReason::Shutdown));
                    return;
                }
            }
        }
    }
}

enum ConnectError {
    Revoked,
    Unauthorized,
    Other(String),
}

async fn connect(url: &str, session: &DeviceSession) -> Result<Ws, ConnectError> {
    let token = session.token().await.map_err(|e| match e.code() {
        Some(aionix_control_api::ErrorCode::DeviceRevoked) => ConnectError::Revoked,
        _ => ConnectError::Other(e.to_string()),
    })?;
    let mut req = url
        .into_client_request()
        .map_err(|e| ConnectError::Other(e.to_string()))?;
    req.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| ConnectError::Other(e.to_string()))?,
    );
    match tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(req),
    )
    .await
    {
        Err(_) => Err(ConnectError::Other("connect timed out".into())),
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tungstenite::Error::Http(resp))) => match resp.status().as_u16() {
            401 => Err(ConnectError::Unauthorized),
            403 => Err(ConnectError::Revoked),
            s => Err(ConnectError::Other(format!("HTTP {s}"))),
        },
        Ok(Err(e)) => Err(ConnectError::Other(e.to_string())),
    }
}

async fn send(ws: &mut Ws, msg: &ClientMessage) -> bool {
    let text = serde_json::to_string(msg).expect("client messages serialize");
    ws.send(Message::Text(text.into())).await.is_ok()
}

fn presence_msg(p: &PresenceInfo) -> ClientMessage {
    ClientMessage::Presence {
        addr: p.addr.clone(),
        unattended_enabled: p.unattended_enabled,
        busy: p.busy,
    }
}

async fn link(
    mut ws: Ws,
    session: &DeviceSession,
    presence: &mut watch::Receiver<PresenceInfo>,
    handler: &Arc<dyn SignalHandler>,
    state: &watch::Sender<LinkState>,
    cmds: &mut mpsc::UnboundedReceiver<Command>,
) -> LinkEnd {
    let started = tokio::time::Instant::now();
    let (answers_tx, mut answers) = mpsc::unbounded_channel::<ClientMessage>();
    let stable = |started: tokio::time::Instant| started.elapsed() > Duration::from_secs(60);
    loop {
        tokio::select! {
            incoming = ws.next() => {
                let msg = match incoming {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        debug!(error = %e, "signaling link error");
                        return LinkEnd::Reconnect { reset_backoff: stable(started) };
                    }
                    None => return LinkEnd::Reconnect { reset_backoff: stable(started) },
                };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(frame) => {
                        let code = frame.map(|f| u16::from(f.code)).unwrap_or(1005);
                        return match code {
                            close_code::DEVICE_REVOKED => LinkEnd::Stop(StopReason::Revoked),
                            close_code::SUPERSEDED => LinkEnd::Stop(StopReason::Superseded),
                            close_code::TOKEN_EXPIRED => {
                                session.invalidate().await;
                                LinkEnd::Reconnect { reset_backoff: true }
                            }
                            _ => LinkEnd::Reconnect { reset_backoff: stable(started) },
                        };
                    }
                    _ => continue,
                };
                let Ok(server_msg) = serde_json::from_str::<ServerMessage>(text.as_str()) else {
                    warn!("unrecognized signaling message ignored");
                    continue;
                };
                match server_msg {
                    ServerMessage::Ready { friendly_id, .. } => {
                        debug!(%friendly_id, "signaling ready");
                        let p = presence.borrow_and_update().clone();
                        if !send(&mut ws, &presence_msg(&p)).await {
                            return LinkEnd::Reconnect { reset_backoff: false };
                        }
                        state.send_replace(LinkState::Online);
                    }
                    ServerMessage::Ping { id } => {
                        if !send(&mut ws, &ClientMessage::Pong { id }).await {
                            return LinkEnd::Reconnect { reset_backoff: false };
                        }
                    }
                    ServerMessage::SessionOffer { session_id, ticket, mode, viewer } => {
                        let h = handler.clone();
                        let tx = answers_tx.clone();
                        tokio::spawn(async move {
                            let d = h.on_offer(SessionOffer { session_id, ticket, mode, viewer: *viewer }).await;
                            let _ = tx.send(ClientMessage::SessionOfferResponse {
                                session_id,
                                accept: d.accept,
                                reason: d.reason,
                                addr: d.addr,
                            });
                        });
                    }
                    ServerMessage::SessionRevoked { session_id } => {
                        handler.on_session_revoked(session_id).await;
                    }
                    ServerMessage::DeviceRevoked => return LinkEnd::Stop(StopReason::Revoked),
                }
            }
            answer = answers.recv() => {
                if let Some(a) = answer && !send(&mut ws, &a).await {
                    return LinkEnd::Reconnect { reset_backoff: false };
                }
            }
            changed = presence.changed() => {
                if changed.is_ok() {
                    let p = presence.borrow_and_update().clone();
                    if !send(&mut ws, &presence_msg(&p)).await {
                        return LinkEnd::Reconnect { reset_backoff: false };
                    }
                }
            }
            cmd = cmds.recv() => match cmd {
                Some(Command::Send(m)) => {
                    if !send(&mut ws, &m).await {
                        return LinkEnd::Reconnect { reset_backoff: false };
                    }
                }
                Some(Command::Shutdown) | None => {
                    let _ = send(&mut ws, &ClientMessage::Goodbye).await;
                    let _ = ws.close(None).await;
                    return LinkEnd::Stop(StopReason::Shutdown);
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_bounds() {
        for _ in 0..100 {
            let j = jitter(Duration::from_secs(4));
            assert!(
                j >= Duration::from_secs(2) && j <= Duration::from_millis(4001),
                "{j:?}"
            );
        }
    }
}
