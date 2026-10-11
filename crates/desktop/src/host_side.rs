//! Receiving side of the unified Aionix Connect app ("Este computador").
//!
//! identity (DPAPI) → enrollment (agent token or quick support) → device login →
//! QUIC endpoint on the enrolled key + relay grants → signaling (presence Online,
//! offers) → HostService (ticket validation, consent window, capture/H.264, input,
//! clipboard) → lifecycle reports to the Control Plane.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{common, theme, views};
use aionix_client::{
    ApiClient,
    enroll::{DeviceSession, Enrollment, ensure_enrolled},
    host::HostOfferHandler,
    identity::{FileIdentityStore, Protection},
    signaling::{LinkState, PresenceInfo, SignalingClient, SignalingOptions, StopReason},
};
use aionix_control_api::rest::{DeviceInfo, SessionEventKind};
use aionix_remote_core::session::{
    ApprovalDecision, ApprovalPrompt, ApprovalRequest, HostConfig, HostService, HostServices,
    HostSessionHandle, TicketValidator,
    approval::WindowsApprovalPrompt,
    media::SystemInputFactory,
    state::SessionState,
    windows_media::{ScreenVideoSource, SystemClipboard},
};
use aionix_shared::FriendlyId;
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::watch;
use tracing::{info, warn};

/// What the receiving side needs to start.
#[derive(Debug, Clone)]
pub struct HostOptions {
    pub server: String,
    /// Signaling WebSocket URL when it is not served by the API address.
    pub signal_url: Option<String>,
    /// Folder of the receiving identity (`…\Aionix Connect\host`): the 9-digit ID.
    pub dir: PathBuf,
    /// Organization enrollment token (first start only): enrolls as a managed agent.
    /// Without it the device enrolls as Quick Support (reachable by its ID).
    pub enrollment_token: Option<String>,
    /// Seconds the consent prompt waits before refusing on its own.
    pub approval_timeout: u64,
}

#[derive(Clone, PartialEq)]
enum Phase {
    Starting(String),
    Online,
    Connecting,
    Stopped(String),
    Error(String),
}

#[derive(Clone)]
struct Technician {
    name: String,
    email: String,
    organization: Option<String>,
    control: bool,
    clipboard: bool,
}

struct HostUi {
    phase: Phase,
    friendly: Option<FriendlyId>,
    device_id: Option<uuid::Uuid>,
    kind: String,
    session: Option<(HostSessionHandle, Option<Technician>)>,
    pending: Option<Technician>,
    last_outcome: Option<String>,
    log_path: PathBuf,
    addr: String,
}

type Shared = Arc<Mutex<HostUi>>;

struct Runtime {
    signaling: Option<SignalingClient>,
}

/// Wraps the real consent window to show the request in the main window too.
struct ObservedPrompt {
    inner: WindowsApprovalPrompt,
    ui: Shared,
    ctx: egui::Context,
}

#[async_trait]
impl ApprovalPrompt for ObservedPrompt {
    async fn ask(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let tech = Technician {
            name: request.technician_name.clone(),
            email: request.technician_email.clone(),
            organization: request.organization.clone(),
            control: request.permissions.control,
            clipboard: request.permissions.clipboard,
        };
        self.ui.lock().unwrap().pending = Some(tech.clone());
        self.ctx.request_repaint();
        let decision = self.inner.ask(request).await;
        {
            let mut ui = self.ui.lock().unwrap();
            ui.pending = None;
            if decision == ApprovalDecision::Approved
                && let Some((_, t)) = ui.session.as_mut()
            {
                *t = Some(tech);
            }
        }
        self.ctx.request_repaint();
        decision
    }
}

fn device_info() -> DeviceInfo {
    let s = aionix_platform::windows::sysinfo::system_info();
    DeviceInfo {
        hostname: s.hostname,
        os_name: s.os_name,
        os_version: s.os_version,
        app_version: aionix_shared::VERSION.into(),
        username: s.username,
    }
}

fn set_phase(ui: &Shared, ctx: &egui::Context, phase: Phase) {
    ui.lock().unwrap().phase = phase;
    ctx.request_repaint();
}

async fn run(args: HostOptions, ui: Shared, runtime: Arc<Mutex<Runtime>>, ctx: egui::Context) {
    let dir = args.dir.clone();
    let mut wait = Duration::from_secs(2);
    let (session, reg) = loop {
        set_phase(
            &ui,
            &ctx,
            Phase::Starting("Conectando ao servidor...".into()),
        );
        match enroll(&args, &dir).await {
            Ok(v) => break v,
            Err(e) => {
                warn!(error = %e, "enrollment/login failed; retrying");
                set_phase(&ui, &ctx, Phase::Error(e));
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(30));
            }
        }
    };
    {
        let mut u = ui.lock().unwrap();
        u.friendly = Some(reg.friendly_id);
        u.device_id = Some(reg.device_id);
        u.kind = match reg.kind {
            aionix_shared::DeviceKind::Agent => "Computador gerenciado".into(),
            aionix_shared::DeviceKind::QuickSupport => "Suporte rápido".into(),
            other => format!("{other:?}"),
        };
    }
    set_phase(
        &ui,
        &ctx,
        Phase::Starting("Preparando conexão segura...".into()),
    );
    let transport = loop {
        match common::bind_transport(session.clone()).await {
            Ok(t) => break t,
            Err(e) => {
                warn!(error = %e, "transport bind failed; retrying");
                set_phase(&ui, &ctx, Phase::Error(format!("Rede indisponível: {e}")));
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    };
    let validator = Arc::new(TicketValidator::new(
        session.ticket_verifier(),
        reg.device_id,
        session.key().public(),
    ));
    let display_id = aionix_capture::windows::enumerate_displays()
        .ok()
        .and_then(|d| d.into_iter().find(|d| d.primary).map(|d| d.id))
        .unwrap_or(1);
    let host = HostService::start(
        transport.clone(),
        HostConfig {
            host_name: aionix_platform::windows::sysinfo::hostname(),
            display_id,
            approval_timeout: Duration::from_secs(args.approval_timeout),
            ..Default::default()
        },
        HostServices {
            validator: validator.clone(),
            approval: Arc::new(ObservedPrompt {
                inner: WindowsApprovalPrompt,
                ui: ui.clone(),
                ctx: ctx.clone(),
            }),
            video: Arc::new(ScreenVideoSource::default()),
            input: Arc::new(SystemInputFactory),
            clipboard: Arc::new(SystemClipboard),
        },
    );

    // Reachability published through signaling follows the endpoint's address.
    let (presence_tx, presence_rx) = watch::channel(PresenceInfo {
        addr: transport.addr(),
        busy: false,
        unattended_enabled: false,
    });
    ui.lock().unwrap().addr = describe_addr(&transport.addr());
    {
        let (tx, t, ui, ctx) = (
            presence_tx.clone(),
            transport.clone(),
            ui.clone(),
            ctx.clone(),
        );
        tokio::spawn(async move {
            let mut updates = Box::pin(t.addr_updates());
            while let Some(addr) = updates.next().await {
                ui.lock().unwrap().addr = describe_addr(&addr);
                ctx.request_repaint();
                tx.send_modify(|p| p.addr = addr);
            }
        });
    }
    let signal_url = args
        .signal_url
        .clone()
        .unwrap_or_else(|| session.api().signal_url());
    let signaling = SignalingClient::spawn(
        SignalingOptions::new(signal_url),
        session.clone(),
        presence_rx.clone(),
        Arc::new(HostOfferHandler::new(validator, presence_rx)),
    );
    runtime.lock().unwrap().signaling = Some(signaling.clone());
    {
        let (ui, ctx) = (ui.clone(), ctx.clone());
        let mut link = signaling.subscribe();
        tokio::spawn(async move {
            loop {
                let state = link.borrow_and_update().clone();
                let phase = match state {
                    LinkState::Online => Phase::Online,
                    LinkState::Connecting => Phase::Connecting,
                    LinkState::Stopped(StopReason::Revoked) => {
                        Phase::Stopped("Este dispositivo foi revogado pelo administrador.".into())
                    }
                    LinkState::Stopped(StopReason::Superseded) => {
                        Phase::Stopped("Este dispositivo foi aberto em outra janela.".into())
                    }
                    LinkState::Stopped(StopReason::Shutdown) => {
                        Phase::Stopped("Desconectado.".into())
                    }
                };
                set_phase(&ui, &ctx, phase);
                if link.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    // Sessions: mark busy while one runs and report its lifecycle (metadata only).
    while let Some(handle) = host.next_session().await {
        ui.lock().unwrap().session = Some((handle.clone(), None));
        ctx.request_repaint();
        presence_tx.send_modify(|p| p.busy = true);
        follow_session(handle, &signaling, &ui, &ctx).await;
        presence_tx.send_modify(|p| p.busy = false);
    }
}

async fn follow_session(
    handle: HostSessionHandle,
    signaling: &SignalingClient,
    ui: &Shared,
    ctx: &egui::Context,
) {
    let mut rx = handle.subscribe();
    let mut last = SessionState::Idle;
    loop {
        let status = rx.borrow_and_update().clone();
        if status.state != last {
            let sid = handle.diagnostics().session_id.value().copied();
            let transport = handle.diagnostics().transport.value().copied();
            if let Some(sid) = sid {
                let kind = match status.state {
                    SessionState::Establishing => Some(SessionEventKind::AuthGranted),
                    SessionState::Connected if last == SessionState::Reconnecting => {
                        Some(SessionEventKind::Reconnected)
                    }
                    SessionState::Connected if last == SessionState::Establishing => {
                        Some(SessionEventKind::Connected)
                    }
                    SessionState::Reconnecting => Some(SessionEventKind::Reconnecting),
                    SessionState::Rejected => Some(SessionEventKind::AuthDenied),
                    SessionState::Ended => Some(SessionEventKind::Ended),
                    SessionState::Failed => Some(SessionEventKind::Failed),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let detail = status.failure.clone().unwrap_or_default();
                    signaling.report(sid, kind, transport, &detail);
                }
            }
            last = status.state;
            ctx.request_repaint();
        }
        if status.state.is_terminal() {
            let mut u = ui.lock().unwrap();
            let at = time::OffsetDateTime::now_local()
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
            let hhmm = format!("{:02}:{:02}", at.hour(), at.minute());
            u.last_outcome = Some(match status.state {
                SessionState::Rejected => format!("Solicitação recusada às {hhmm}."),
                SessionState::Failed => format!("Sessão interrompida às {hhmm}."),
                _ => format!("Última sessão encerrada às {hhmm}."),
            });
            u.session = None;
            drop(u);
            ctx.request_repaint();
            return;
        }
        if rx.changed().await.is_err() {
            ui.lock().unwrap().session = None;
            return;
        }
    }
}

async fn enroll(
    args: &HostOptions,
    dir: &std::path::Path,
) -> Result<(Arc<DeviceSession>, aionix_client::identity::Registration), String> {
    let api = ApiClient::new(&args.server).map_err(|e| e.to_string())?;
    let store = FileIdentityStore::new(dir.join("identity.json"), Protection::DpapiUser);
    let enrollment = match args.enrollment_token.as_deref() {
        Some(t) if !t.is_empty() => Enrollment::Agent {
            enrollment_token: t,
        },
        _ => Enrollment::QuickSupport,
    };
    let info = device_info();
    let (key, reg) = ensure_enrolled(&api, &store, enrollment, info.clone())
        .await
        .map_err(|e| common::client_error_text(&e))?;
    info!(device_id = %reg.device_id, friendly = %reg.friendly_id, "enrolled");
    let session = Arc::new(DeviceSession::new(api, key, reg.clone(), Some(info)));
    // Proves the identity is still accepted (revoked devices stop here).
    match session.token().await {
        Ok(_) => Ok((session, reg)),
        // The server does not know this identity (its database was replaced): start
        // over with a new identity instead of failing forever. Revoked identities get
        // `DeviceRevoked` instead and are never replaced automatically.
        Err(e) if e.code() == Some(aionix_control_api::ErrorCode::Unauthorized) => {
            warn!(device_id = %reg.device_id, "identity unknown to this server; enrolling again");
            let stale = dir.join("identity.json");
            let _ = std::fs::rename(&stale, dir.join("identity.unknown-to-server.json"));
            Err(
                "Cadastro deste computador não encontrado no servidor; registrando novamente..."
                    .into(),
            )
        }
        Err(e) => Err(common::client_error_text(&e)),
    }
}

fn describe_addr(a: &aionix_control_api::signal::EndpointAddrDto) -> String {
    format!(
        "{} endereço(s) direto(s), {} relay(s)",
        a.direct_addrs.len(),
        a.relay_urls.len()
    )
}

/// The receiving side of the unified app: enrollment (the 9-digit ID), signaling,
/// the consent prompt and inbound sessions. Runs from start to exit, whatever the user
/// does in the rest of the window.
pub struct HostSide {
    ui: Shared,
    runtime: Arc<Mutex<Runtime>>,
    rt: tokio::runtime::Handle,
    copied_at: Option<Instant>,
}

impl HostSide {
    pub fn start(
        rt: &tokio::runtime::Handle,
        opts: HostOptions,
        ctx: egui::Context,
        log_path: PathBuf,
    ) -> Self {
        info!(server = %opts.server, dir = %opts.dir.display(), "receiving side starting");
        let ui = Arc::new(Mutex::new(HostUi {
            phase: Phase::Starting("Iniciando...".into()),
            friendly: None,
            device_id: None,
            kind: String::new(),
            session: None,
            pending: None,
            last_outcome: None,
            log_path,
            addr: String::new(),
        }));
        let runtime = Arc::new(Mutex::new(Runtime { signaling: None }));
        rt.spawn(run(opts, ui.clone(), runtime.clone(), ctx));
        Self {
            ui,
            runtime,
            rt: rt.clone(),
            copied_at: None,
        }
    }

    /// This computer's ID once enrolled.
    pub fn friendly(&self) -> Option<FriendlyId> {
        self.ui.lock().unwrap().friendly
    }

    /// Someone is connected to, or asking to connect to, this computer.
    pub fn busy(&self) -> bool {
        let u = self.ui.lock().unwrap();
        u.session.is_some() || u.pending.is_some()
    }

    /// Ends an inbound session and leaves signaling cleanly (app exit).
    pub fn shutdown(&self) {
        let signaling = self.runtime.lock().unwrap().signaling.clone();
        let session = self
            .ui
            .lock()
            .unwrap()
            .session
            .as_ref()
            .map(|s| s.0.clone());
        if let Some(h) = session {
            h.end();
        }
        if let Some(s) = signaling {
            s.shutdown();
            let _ = self.rt.block_on(async {
                s.wait_for(Duration::from_secs(2), |l| {
                    matches!(l, LinkState::Stopped(_))
                })
                .await
            });
        }
    }

    /// "Este computador" for the views: the ID, its status, and any inbound request or
    /// session.
    pub fn model(&self) -> views::HostModel {
        let u = self.ui.lock().unwrap();
        let in_session = u.session.is_some();
        let (status, text) = match &u.phase {
            Phase::Online if in_session => (views::HostStatus::Online, "Em sessão".to_owned()),
            Phase::Online => (
                views::HostStatus::Online,
                "Pronto para receber suporte".to_owned(),
            ),
            Phase::Connecting => (
                views::HostStatus::Connecting,
                "Conectando ao servidor…".to_owned(),
            ),
            Phase::Starting(s) => (views::HostStatus::Starting, s.replace("...", "…")),
            Phase::Stopped(s) => (views::HostStatus::Offline, s.clone()),
            Phase::Error(_) => (
                views::HostStatus::Offline,
                "Sem conexão com o servidor".to_owned(),
            ),
        };
        let detail = match &u.phase {
            Phase::Error(e) => Some(format!("{e} Tentando novamente…")),
            _ => None,
        };
        let inbound = u.session.as_ref().map(|(handle, tech)| {
            let status = handle.status();
            let mut can = vec!["ver a tela"];
            if let Some(t) = tech {
                if t.control {
                    can.push("usar mouse e teclado");
                }
                if t.clipboard {
                    can.push("usar a área de transferência");
                }
            }
            let permissions = match can.split_last() {
                Some((last, rest)) if !rest.is_empty() => format!("{} e {last}", rest.join(", ")),
                _ => can.join(""),
            };
            let duration = handle.diagnostics().duration.value().map(|d| {
                let s = d.as_secs();
                format!("{:02}:{:02}", s / 60, s % 60)
            });
            let state = (status.state != SessionState::Connected).then(|| {
                (
                    common::state_text(status.state).to_owned(),
                    match status.state {
                        SessionState::Failed | SessionState::Rejected => views::Tone::Danger,
                        SessionState::Ended | SessionState::Ending => views::Tone::Neutral,
                        _ => views::Tone::Warning,
                    },
                )
            });
            views::InboundModel {
                email: tech
                    .as_ref()
                    .map(|t| t.email.clone())
                    .filter(|e| !e.is_empty()),
                name: tech
                    .as_ref()
                    .map(display_name)
                    .unwrap_or_else(|| "Um técnico".into()),
                permissions,
                duration,
                state,
                can_end: !status.state.is_terminal(),
            }
        });
        views::HostModel {
            id: u.friendly.map(|id| (common::friendly(id), id.compact())),
            status,
            status_text: text,
            status_detail: detail,
            pending: u.pending.as_ref().map(display_name),
            inbound,
            last_outcome: u.last_outcome.clone(),
            copied: self
                .copied_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2)),
        }
    }

    /// Acts on what the user asked for in "Este computador".
    pub fn act(&mut self, ctx: &egui::Context, action: views::HostAction) {
        match action {
            views::HostAction::Copy(compact) => {
                ctx.copy_text(compact);
                self.copied_at = Some(Instant::now());
                // Back to "Copiar" when the confirmation expires.
                ctx.request_repaint_after(Duration::from_millis(2050));
            }
            views::HostAction::EndInbound => {
                let session = self.ui.lock().unwrap().session.clone();
                if let Some((handle, _)) = session {
                    handle.end();
                }
            }
        }
    }

    /// Collapsible technical details (support and diagnostics).
    pub fn details(&self, ui: &mut egui::Ui) {
        let (session, kind, device_id, log_path, addr) = {
            let u = self.ui.lock().unwrap();
            (
                u.session.clone(),
                u.kind.clone(),
                u.device_id,
                u.log_path.clone(),
                u.addr.clone(),
            )
        };
        views::disclosure(ui, "Detalhes técnicos", |ui| {
                if let Some(id) = device_id {
                    theme::kv(ui, "Dispositivo", &id.to_string());
                }
                if !kind.is_empty() {
                    theme::kv(ui, "Tipo", &kind);
                }
                theme::kv(ui, "Rede", &addr);
                theme::kv(ui, "Registro", &log_path.display().to_string());
                if let Some((h, _)) = &session {
                    let d = h.diagnostics();
                    let na = || "—".to_string();
                    theme::kv(
                        ui,
                        "Captura",
                        d.capture_backend.value().map(String::as_str).unwrap_or("—"),
                    );
                    theme::kv(
                        ui,
                        "Codificador",
                        d.encoder.value().map(String::as_str).unwrap_or("—"),
                    );
                    theme::kv(
                        ui,
                        "Quadros/s",
                        &d.fps.value().map(|v| format!("{v:.0}")).unwrap_or_else(na),
                    );
                    theme::kv(
                        ui,
                        "Taxa",
                        &d.bitrate_kbps
                            .value()
                            .map(|v| format!("{v:.0} kbit/s"))
                            .unwrap_or_else(na),
                    );
                    theme::kv(
                        ui,
                        "Transporte",
                        &d.transport
                            .value()
                            .map(|t| format!("{t:?}"))
                            .unwrap_or_else(na),
                    );
                }
            });
    }
}

fn display_name(t: &Technician) -> String {
    match &t.organization {
        Some(o) if !o.is_empty() => format!("{} ({o})", t.name),
        _ => t.name.clone(),
    }
}
