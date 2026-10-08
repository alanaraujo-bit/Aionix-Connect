//! Aionix Connect — the computer that receives support.
//!
//! identity (DPAPI) → enrollment (agent token or quick support) → device login →
//! QUIC endpoint on the enrolled key + relay grants → signaling (presence Online,
//! offers) → HostService (ticket validation, consent window, capture/H.264, input,
//! clipboard) → lifecycle reports to the Control Plane.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("aionix-host runs on Windows only");
}

#[cfg(windows)]
fn main() {
    app::main();
}

#[cfg(windows)]
mod app {
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use aionix_client::{
        ApiClient,
        enroll::{DeviceSession, Enrollment, ensure_enrolled},
        host::HostOfferHandler,
        identity::{FileIdentityStore, Protection},
        signaling::{LinkState, PresenceInfo, SignalingClient, SignalingOptions, StopReason},
    };
    use aionix_control_api::rest::{DeviceInfo, SessionEventKind};
    use aionix_desktop::{common, icon, theme};
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
    use clap::Parser;
    use egui::{Align, Layout, RichText, Stroke};
    use futures::StreamExt;
    use tokio::sync::watch;
    use tracing::{error, info, warn};

    #[derive(Parser, Debug, Clone)]
    #[command(
        name = "aionix-host",
        about = "Aionix Connect — receber suporte remoto"
    )]
    struct Args {
        /// Public server URL (API, signaling and relay behind one address). Default:
        /// server in ionix-host.json next to the executable, else the local staging.
        #[arg(long, env = "AIONIX_SERVER")]
        server: Option<String>,
        /// Signaling WebSocket URL when it is not served by the API address.
        #[arg(long, env = "AIONIX_SIGNAL_URL")]
        signal_url: Option<String>,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Organization enrollment token (first start only): enrolls as a managed agent.
        /// Without it the device enrolls as Quick Support (reachable by its ID).
        #[arg(long, env = "AIONIX_ENROLLMENT_TOKEN")]
        enrollment_token: Option<String>,
        /// Banner text identifying a local validation build.
        #[arg(long)]
        environment_label: Option<String>,
        /// Seconds the consent prompt waits before refusing on its own.
        #[arg(long, default_value_t = 60)]
        approval_timeout: u64,
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

    /// Settings shipped next to the executable (a package made for one server).
    #[derive(serde::Deserialize, Default)]
    struct FileConfig {
        server: Option<String>,
        signal_url: Option<String>,
        environment_label: Option<String>,
    }

    fn file_config() -> FileConfig {
        std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::read_to_string(p.with_file_name("aionix-host.json")).ok())
            .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default()
    }

    pub fn main() {
        let mut args = Args::parse();
        let file = file_config();
        if args.server.is_none() {
            args.server = Some(
                file.server
                    .unwrap_or_else(|| "http://127.0.0.1:8080".into()),
            );
        }
        if args.signal_url.is_none() {
            args.signal_url = file.signal_url;
        }
        if args.environment_label.is_none() {
            args.environment_label = file.environment_label;
        }
        aionix_platform::windows::init_dpi_awareness();
        let dir = common::data_dir("host", args.data_dir.clone());
        let log_path = common::init_logging(&dir, "aionix-host");
        info!(server = %server(&args), dir = %dir.display(), "Aionix Connect host starting");
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
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
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(aionix_brand::brand().product_name.clone())
                .with_inner_size([440.0, 660.0])
                .with_min_inner_size([400.0, 560.0])
                .with_icon(icon::brand_icon(64, [0x4f, 0x7c, 0xff])),
            ..Default::default()
        };
        let label = args.environment_label.clone();
        let (ui2, rt_handle, runtime2) = (ui.clone(), rt.handle().clone(), runtime.clone());
        let result = eframe::run_native(
            "aionix-host",
            options,
            Box::new(move |cc| {
                theme::install(&cc.egui_ctx);
                rt_handle.spawn(run(
                    args,
                    ui2.clone(),
                    runtime2.clone(),
                    cc.egui_ctx.clone(),
                    dir,
                ));
                Ok(Box::new(HostApp {
                    ui: ui2,
                    runtime: runtime2,
                    rt: rt_handle,
                    label,
                    copied_at: None,
                }))
            }),
        );
        if let Err(e) = result {
            error!(error = %e, "window failed");
        }
        rt.shutdown_timeout(Duration::from_secs(3));
    }

    fn server(args: &Args) -> &str {
        args.server.as_deref().unwrap_or("http://127.0.0.1:8080")
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

    async fn run(
        args: Args,
        ui: Shared,
        runtime: Arc<Mutex<Runtime>>,
        ctx: egui::Context,
        dir: PathBuf,
    ) {
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
                        LinkState::Stopped(StopReason::Revoked) => Phase::Stopped(
                            "Este dispositivo foi revogado pelo administrador.".into(),
                        ),
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
        args: &Args,
        dir: &std::path::Path,
    ) -> Result<(Arc<DeviceSession>, aionix_client::identity::Registration), String> {
        let api = ApiClient::new(server(&args)).map_err(|e| e.to_string())?;
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
                Err("Cadastro deste computador não encontrado no servidor; registrando novamente...".into())
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

    struct HostApp {
        ui: Shared,
        runtime: Arc<Mutex<Runtime>>,
        rt: tokio::runtime::Handle,
        label: Option<String>,
        copied_at: Option<Instant>,
    }

    impl eframe::App for HostApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let p = theme::palette(ui);
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(p.bg)
                        .inner_margin(egui::Margin::same(22)),
                )
                .show(ui, |ui| self.body(ui));
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }

        fn on_exit(&mut self) {
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
    }

    impl HostApp {
        fn body(&mut self, ui: &mut egui::Ui) {
            let p = theme::palette(ui);
            let state = {
                let u = self.ui.lock().unwrap();
                (
                    u.phase.clone(),
                    u.friendly,
                    u.session.clone(),
                    u.pending.clone(),
                    u.last_outcome.clone(),
                    u.kind.clone(),
                    u.device_id,
                    u.log_path.clone(),
                    u.addr.clone(),
                )
            };
            let (phase, friendly, session, pending, last_outcome, kind, device_id, log_path, addr) =
                state;

            theme::brand_header(ui, "Suporte remoto");
            if let Some(label) = &self.label {
                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(p.warning.gamma_multiply(0.12))
                    .stroke(Stroke::new(1.0, p.warning.gamma_multiply(0.4)))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.label(RichText::new(label).size(12.5).color(p.warning));
                    });
            }
            ui.add_space(14.0);

            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Seu ID").size(13.0).color(p.muted));
                match friendly {
                    Some(id) => {
                        ui.horizontal(|ui| {
                            ui.label(theme::title(common::friendly(id), 34.0));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let copied = self
                                    .copied_at
                                    .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                                let text = if copied { "Copiado" } else { "Copiar" };
                                if theme::secondary_button(ui, text, true, Some(86.0)).clicked() {
                                    ui.ctx().copy_text(id.compact());
                                    self.copied_at = Some(Instant::now());
                                }
                            });
                        });
                    }
                    None => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(22.0));
                            ui.label(theme::title("— — —", 30.0).color(p.muted));
                        });
                    }
                }
                ui.add_space(4.0);
                let (text, color) = match &phase {
                    Phase::Online if session.is_some() => ("Em sessão", p.accent),
                    Phase::Online => ("Pronto para receber suporte", p.success),
                    Phase::Connecting => ("Conectando ao servidor...", p.warning),
                    Phase::Starting(s) => (s.as_str(), p.warning),
                    Phase::Stopped(s) => (s.as_str(), p.danger),
                    Phase::Error(_) => ("Sem conexão com o servidor", p.danger),
                };
                theme::status_pill(ui, text, color);
                if let Phase::Error(e) = &phase {
                    ui.label(RichText::new(format!("{e} Tentando novamente...")).size(12.5).color(p.muted));
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Informe este ID ao técnico. Ninguém acessa este computador sem a sua permissão.",
                    )
                    .size(13.0)
                    .color(p.muted),
                );
            });

            ui.add_space(12.0);
            if let Some(t) = &pending {
                theme::card(ui)
                    .stroke(Stroke::new(1.5, p.warning))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(theme::title("Solicitação de acesso", 16.0));
                        ui.label(format!("{} está pedindo acesso.", display_name(t)));
                        ui.label(
                            RichText::new(
                                "Responda na janela de confirmação: Permitir ou Recusar.",
                            )
                            .size(13.0)
                            .color(p.muted),
                        );
                    });
                ui.add_space(12.0);
            } else if let Some((handle, tech)) = &session {
                let status = handle.status();
                let d = handle.diagnostics();
                theme::card(ui)
                    .stroke(Stroke::new(1.5, p.accent))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(theme::title("Sessão ativa", 16.0));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                theme::status_pill(
                                    ui,
                                    common::state_text(status.state),
                                    state_color(&p, status.state),
                                );
                            });
                        });
                        if let Some(t) = tech {
                            ui.label(format!("Técnico: {}", display_name(t)));
                            if !t.email.is_empty() {
                                ui.label(RichText::new(&t.email).size(13.0).color(p.muted));
                            }
                            let mut can = vec!["ver a tela"];
                            if t.control {
                                can.push("usar mouse e teclado");
                            }
                            if t.clipboard {
                                can.push("área de transferência");
                            }
                            ui.label(
                                RichText::new(format!("Permissões: {}", can.join(", ")))
                                    .size(13.0)
                                    .color(p.muted),
                            );
                        }
                        if let Some(dur) = d.duration.value() {
                            let s = dur.as_secs();
                            ui.label(
                                RichText::new(format!("Duração: {:02}:{:02}", s / 60, s % 60))
                                    .size(13.0)
                                    .color(p.muted),
                            );
                        }
                        ui.add_space(6.0);
                        if theme::danger_button(
                            ui,
                            "Encerrar sessão",
                            !status.state.is_terminal(),
                            Some(ui.available_width()),
                        )
                        .clicked()
                        {
                            handle.end();
                        }
                    });
                ui.add_space(12.0);
            } else if let Some(o) = &last_outcome {
                ui.label(RichText::new(o).size(13.0).color(p.muted));
                ui.add_space(8.0);
            }

            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                ui.label(
                    RichText::new(format!("Registro: {}", log_path.display()))
                        .size(11.5)
                        .color(p.muted),
                );
                egui::CollapsingHeader::new(
                    RichText::new("Detalhes técnicos").size(12.5).color(p.muted),
                )
                .show(ui, |ui| {
                    if let Some(id) = device_id {
                        theme::kv(ui, "Dispositivo", &id.to_string());
                    }
                    if !kind.is_empty() {
                        theme::kv(ui, "Tipo", &kind);
                    }
                    theme::kv(ui, "Rede", &addr);
                    if let Some((h, _)) = &session {
                        let d = h.diagnostics();
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
                            &d.fps
                                .value()
                                .map(|v| format!("{v:.0}"))
                                .unwrap_or("—".into()),
                        );
                        theme::kv(
                            ui,
                            "Taxa",
                            &d.bitrate_kbps
                                .value()
                                .map(|v| format!("{v:.0} kbit/s"))
                                .unwrap_or("—".into()),
                        );
                        theme::kv(
                            ui,
                            "Transporte",
                            &d.transport
                                .value()
                                .map(|t| format!("{t:?}"))
                                .unwrap_or("—".into()),
                        );
                    }
                });
            });
        }
    }

    fn display_name(t: &Technician) -> String {
        match &t.organization {
            Some(o) if !o.is_empty() => format!("{} ({o})", t.name),
            _ => t.name.clone(),
        }
    }

    pub(crate) fn state_color(p: &theme::Palette, s: SessionState) -> egui::Color32 {
        match s {
            SessionState::Connected => p.success,
            SessionState::Degraded
            | SessionState::Reconnecting
            | SessionState::AwaitingApproval => p.warning,
            SessionState::Failed | SessionState::Rejected => p.danger,
            SessionState::Ended | SessionState::Ending => p.muted,
            _ => p.info,
        }
    }
}
