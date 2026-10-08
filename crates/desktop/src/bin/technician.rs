//! Aionix Connect Técnico.
//!
//! sign in (rotating refresh tokens) → workstation enrollment bound to the user
//! (DPAPI identity) → QUIC endpoint on the workstation key → organization devices with
//! live presence → session request through `ControlPlaneAuthority` (the Control Plane
//! checks role permissions, presence and signs the ticket) → Remote Core technician
//! session: H.264 decode, viewer, input, clipboard, diagnostics.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("aionix-technician runs on Windows only");
}

#[cfg(windows)]
fn main() {
    app::main();
}

#[cfg(windows)]
mod app {
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use aionix_client::{
        ApiClient,
        authority::{ControlPlaneAuthority, UserSession},
        enroll::{DeviceSession, Enrollment, ensure_enrolled},
        identity::{FileIdentityStore, Protection},
    };
    use aionix_control_api::{
        claims::SessionPermission,
        rest::{
            DeviceInfo, DeviceSummary, PresenceState, SessionEventKind, SessionEventReport,
            UserProfile,
        },
    };
    use aionix_desktop::{
        common, icon, keymap,
        theme::{self, Palette},
        video::{FrameSlot, VideoSink},
    };
    use aionix_protocol::pb;
    use aionix_remote_core::session::{
        DeviceLookup, TechnicianConfig, TechnicianEvent, TechnicianIdentity, TechnicianServices,
        TechnicianSession, TechnicianSessionHandle,
        state::SessionState,
        windows_media::{MfDecoderFactory, SystemClipboard},
    };
    use aionix_shared::{FriendlyId, TransportKind};
    use aionix_transport::Transport;
    use clap::Parser;
    use egui::{
        Align, Color32, CornerRadius, Layout, Pos2, Rect, RichText, Sense, Stroke, TextureHandle,
        TextureOptions, Vec2,
    };
    use tokio::sync::mpsc;
    use tracing::{error, info, warn};
    use uuid::Uuid;

    #[derive(Parser, Debug, Clone)]
    #[command(name = "aionix-technician", about = "Aionix Connect Técnico")]
    struct Args {
        #[arg(long, env = "AIONIX_SERVER", default_value = "http://127.0.0.1:8080")]
        server: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Pre-filled e-mail (development launcher).
        #[arg(long, env = "AIONIX_DEV_EMAIL")]
        email: Option<String>,
        /// Banner text identifying a local validation build.
        #[arg(long)]
        environment_label: Option<String>,
        /// Sign in at start with the pre-filled e-mail and password (development launcher).
        #[arg(long)]
        auto_login: bool,
        /// Development/validation: request a session to this ID right after signing in.
        #[arg(long, hide = true)]
        auto_connect: Option<FriendlyId>,
    }

    // ------------------------------------------------------------------ state

    struct Workspace {
        api: ApiClient,
        user: Arc<UserSession>,
        profile: UserProfile,
        org: Option<(Uuid, String)>,
        workstation: Arc<DeviceSession>,
        transport: Transport,
        devices: Arc<Mutex<DeviceListState>>,
    }

    #[derive(Default)]
    struct DeviceListState {
        devices: Vec<DeviceSummary>,
        loaded: bool,
        error: Option<String>,
    }

    enum LoginState {
        Idle,
        Working(&'static str),
        Failed(String),
    }

    struct CursorTex {
        tex: TextureHandle,
        hotspot: Vec2,
        size: Vec2,
    }

    struct ActiveSession {
        handle: TechnicianSessionHandle,
        target: FriendlyId,
        name: String,
        local_machine: bool,
        frames: Arc<FrameSlot>,
        texture: Option<TextureHandle>,
        events: Option<mpsc::Receiver<TechnicianEvent>>,
        stream: Option<pb::StreamStarted>,
        host_name: Option<String>,
        cursors: HashMap<u64, CursorTex>,
        cursor: Option<pb::CursorPosition>,
        control: bool,
        show_diag: bool,
        input: InputState,
        started: Instant,
    }

    #[derive(Default)]
    struct InputState {
        focused: bool,
        last_pos: Option<(i32, i32)>,
        mods: egui::Modifiers,
        last_key_unmapped: bool,
    }

    enum Screen {
        Login,
        Devices,
        Session(Box<ActiveSession>),
    }

    struct TechApp {
        args: Args,
        rt: tokio::runtime::Runtime,
        dir: PathBuf,
        log_path: PathBuf,
        screen: Screen,
        /// Screen to switch to after the current frame (set by handlers).
        next: Option<Screen>,
        email: String,
        password: String,
        login: Arc<Mutex<LoginState>>,
        pending_ws: Arc<Mutex<Option<Workspace>>>,
        ws: Option<Arc<Workspace>>,
        connect_id: String,
        connect_error: Option<String>,
        filter: String,
        local_host: String,
    }

    pub fn main() {
        let args = Args::parse();
        aionix_platform::windows::init_dpi_awareness();
        let dir = common::data_dir("technician", args.data_dir.clone());
        let log_path = common::init_logging(&dir, "aionix-technician");
        info!(server = %args.server, "Aionix Connect Technician starting");
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(
                    aionix_brand::brand()
                        .apps
                        .technician
                        .replace("Technician", "Técnico"),
                )
                .with_inner_size([1280.0, 800.0])
                .with_min_inner_size([900.0, 600.0])
                .with_icon(icon::brand_icon(64, [0x4f, 0x7c, 0xff])),
            ..Default::default()
        };
        let password = std::env::var("AIONIX_DEV_PASSWORD").unwrap_or_default();
        // The development password is only a pre-fill; it is not kept in the environment.
        // SAFETY-free: removing our own process variable before any threads read it.
        unsafe { std::env::remove_var("AIONIX_DEV_PASSWORD") };
        let result = eframe::run_native(
            "aionix-technician",
            options,
            Box::new(move |cc| {
                theme::install(&cc.egui_ctx);
                Ok(Box::new(TechApp {
                    email: args.email.clone().unwrap_or_default(),
                    password,
                    args,
                    rt,
                    dir,
                    log_path,
                    screen: Screen::Login,
                    next: None,
                    login: Arc::new(Mutex::new(LoginState::Idle)),
                    pending_ws: Arc::new(Mutex::new(None)),
                    ws: None,
                    connect_id: String::new(),
                    connect_error: None,
                    filter: String::new(),
                    local_host: aionix_platform::windows::sysinfo::hostname(),
                }))
            }),
        );
        if let Err(e) = result {
            error!(error = %e, "window failed");
        }
    }

    // ------------------------------------------------------------------ login

    async fn sign_in(
        server: String,
        dir: PathBuf,
        email: String,
        password: String,
        status: Arc<Mutex<LoginState>>,
        ctx: egui::Context,
    ) -> Result<Workspace, String> {
        let set = |s: &'static str| {
            *status.lock().unwrap() = LoginState::Working(s);
            ctx.request_repaint();
        };
        set("Autenticando...");
        let api = ApiClient::new(&server).map_err(|e| e.to_string())?;
        let user = Arc::new(
            UserSession::login(api.clone(), email.trim(), &password)
                .await
                .map_err(|e| common::client_error_text(&e))?,
        );
        let profile = user.profile().await;
        let org = profile
            .memberships
            .iter()
            .find(|m| m.role.can_connect())
            .or(profile.memberships.first())
            .map(|m| (m.org_id, m.org_name.clone()));
        set("Registrando esta estação de trabalho...");
        let access = user
            .access_token()
            .await
            .map_err(|e| common::client_error_text(&e))?;
        let store = FileIdentityStore::new(
            dir.join(format!("workstation-{}.json", profile.id.simple())),
            Protection::DpapiUser,
        );
        let s = aionix_platform::windows::sysinfo::system_info();
        let info = DeviceInfo {
            hostname: s.hostname,
            os_name: s.os_name,
            os_version: s.os_version,
            app_version: aionix_shared::VERSION.into(),
            username: s.username,
        };
        let (key, reg) = ensure_enrolled(
            &api,
            &store,
            Enrollment::Technician {
                access_token: &access,
            },
            info.clone(),
        )
        .await
        .map_err(|e| common::client_error_text(&e))?;
        let workstation = Arc::new(DeviceSession::new(api.clone(), key, reg, Some(info)));
        set("Preparando conexão segura...");
        let transport = common::bind_transport(workstation.clone())
            .await
            .map_err(|e| format!("Rede indisponível: {e}"))?;
        info!(user = %profile.email, "signed in");
        Ok(Workspace {
            api,
            user,
            profile,
            org,
            workstation,
            transport,
            devices: Arc::new(Mutex::new(DeviceListState::default())),
        })
    }

    fn spawn_device_refresh(rt: &tokio::runtime::Runtime, ws: Arc<Workspace>, ctx: egui::Context) {
        let weak = Arc::downgrade(&ws);
        drop(ws);
        rt.spawn(async move {
            loop {
                let Some(ws) = weak.upgrade() else { return };
                if let Some((org, _)) = ws.org {
                    let result = match ws.user.access_token().await {
                        Ok(access) => ws.api.org_devices(&access, org).await,
                        Err(e) => Err(e),
                    };
                    let mut st = ws.devices.lock().unwrap();
                    match result {
                        Ok(list) => {
                            let mut devices: Vec<_> = list
                                .devices
                                .into_iter()
                                .filter(|d| d.kind.can_host())
                                .collect();
                            devices.sort_by_key(|d| {
                                (presence_rank(d.presence), d.display_name.to_lowercase())
                            });
                            st.devices = devices;
                            st.loaded = true;
                            st.error = None;
                        }
                        Err(e) => st.error = Some(common::client_error_text(&e)),
                    }
                } else {
                    ws.devices.lock().unwrap().loaded = true;
                }
                drop(ws);
                ctx.request_repaint();
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    fn presence_rank(p: PresenceState) -> u8 {
        match p {
            PresenceState::Online => 0,
            PresenceState::Connecting => 1,
            PresenceState::Offline => 2,
        }
    }

    // ------------------------------------------------------------------ sessions

    impl TechApp {
        fn start_session(
            &mut self,
            ctx: &egui::Context,
            target: FriendlyId,
            name: String,
            hostname: &str,
        ) {
            let Some(ws) = self.ws.clone() else { return };
            let frames = FrameSlot::new(ctx.clone());
            let identity = TechnicianIdentity {
                user_id: ws.profile.id,
                device_id: ws.workstation.registration().device_id,
                key: ws.workstation.key().public(),
                display_name: ws.profile.display_name.clone(),
                email: ws.profile.email.clone(),
                org_name: ws.org.as_ref().map(|o| o.1.clone()),
            };
            let mut config = TechnicianConfig::new(identity, DeviceLookup::Friendly(target));
            config.permissions = vec![
                SessionPermission::View,
                SessionPermission::Control,
                SessionPermission::Clipboard,
            ];
            let authority = Arc::new(ControlPlaneAuthority::new(
                ws.user.clone(),
                ws.workstation.clone(),
            ));
            let handle = {
                let _guard = self.rt.enter();
                TechnicianSession::start(
                    ws.transport.clone(),
                    config,
                    TechnicianServices {
                        authority,
                        decoder: Arc::new(MfDecoderFactory),
                        frames: Box::new(VideoSink(frames.clone())),
                        clipboard: Arc::new(SystemClipboard),
                    },
                )
            };
            info!(target = %target, "session requested");
            self.rt
                .spawn(report_lifecycle(handle.clone(), ws.clone(), ctx.clone()));
            let local_machine =
                !hostname.is_empty() && hostname.eq_ignore_ascii_case(&self.local_host);
            let events = handle.take_events();
            self.next = Some(Screen::Session(Box::new(ActiveSession {
                handle,
                target,
                name,
                local_machine,
                frames,
                texture: None,
                events,
                stream: None,
                host_name: None,
                cursors: HashMap::new(),
                cursor: None,
                // Same machine: the injected cursor would chase the viewer. Off by default.
                control: !local_machine,
                show_diag: false,
                input: InputState::default(),
                started: Instant::now(),
            })));
        }
    }

    /// Lifecycle reports for the session history/audit (metadata only).
    async fn report_lifecycle(
        handle: TechnicianSessionHandle,
        ws: Arc<Workspace>,
        ctx: egui::Context,
    ) {
        let report = |kind: SessionEventKind, transport: Option<TransportKind>, detail: String| {
            let (ws, sid) = (ws.clone(), handle.diagnostics().session_id.value().copied());
            async move {
                let Some(sid) = sid else { return };
                let report = SessionEventReport {
                    kind,
                    transport,
                    detail: detail.chars().take(300).collect(),
                };
                match ws.user.access_token().await {
                    Ok(access) => {
                        if let Err(e) = ws.api.report_session_event(&access, sid, &report).await {
                            warn!(error = %e, "session event not recorded");
                        }
                    }
                    Err(e) => warn!(error = %e, "no access token for session event"),
                }
            }
        };
        let mut rx = handle.subscribe();
        let mut last = SessionState::Idle;
        let mut reported_transport: Option<TransportKind> = None;
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            let status = rx.borrow_and_update().clone();
            if status.state != last {
                ctx.request_repaint();
                let kind = match status.state {
                    SessionState::Connected if last == SessionState::Establishing => {
                        Some(SessionEventKind::Connected)
                    }
                    SessionState::Connected if last == SessionState::Reconnecting => {
                        Some(SessionEventKind::Reconnected)
                    }
                    SessionState::Reconnecting => Some(SessionEventKind::Reconnecting),
                    SessionState::Ended => Some(SessionEventKind::Ended),
                    SessionState::Failed => Some(SessionEventKind::Failed),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let t = handle.diagnostics().transport.value().copied();
                    report(kind, t, status.failure.clone().unwrap_or_default()).await;
                }
                last = status.state;
            }
            if status.state.is_terminal() {
                ctx.request_repaint();
                return;
            }
            tokio::select! {
                changed = rx.changed() => if changed.is_err() { return },
                _ = tick.tick() => {
                    // The path is measured after the session connects and may change
                    // (relay → direct); record it once known and on every change.
                    let t = handle.diagnostics().transport.value().copied();
                    if status.state.is_live() && t.is_some() && t != reported_transport && t != Some(TransportKind::Unknown) {
                        reported_transport = t;
                        report(SessionEventKind::TransportChanged, t, String::new()).await;
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ UI

    impl eframe::App for TechApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            if self.args.auto_login && !self.email.is_empty() && !self.password.is_empty() {
                self.args.auto_login = false;
                self.begin_login(ui.ctx());
            }
            let signed_in = self.pending_ws.lock().unwrap().take();
            if let Some(ws) = signed_in {
                let ws = Arc::new(ws);
                spawn_device_refresh(&self.rt, ws.clone(), ui.ctx().clone());
                self.ws = Some(ws);
                self.screen = Screen::Devices;
                *self.login.lock().unwrap() = LoginState::Idle;
                self.password.clear();
                if let Some(id) = self.args.auto_connect.take() {
                    self.start_session(ui.ctx(), id, format!("ID {id}"), "");
                    if let Some(next) = self.next.take() {
                        self.screen = next;
                    }
                }
            }
            let p = theme::palette(ui);
            let mut screen = std::mem::replace(&mut self.screen, Screen::Login);
            match &mut screen {
                Screen::Login => {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new().fill(p.bg))
                        .show(ui, |ui| self.login_screen(ui));
                }
                Screen::Devices => {
                    self.top_bar(ui);
                    egui::CentralPanel::default()
                        .frame(
                            egui::Frame::new()
                                .fill(p.bg)
                                .inner_margin(egui::Margin::same(24)),
                        )
                        .show(ui, |ui| self.devices_screen(ui));
                }
                Screen::Session(s) => {
                    if !self.session_screen(ui, s) {
                        screen = Screen::Devices;
                    }
                }
            }
            self.screen = self.next.take().unwrap_or(screen);
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }

        fn on_exit(&mut self) {
            if let Screen::Session(s) = &self.screen {
                s.handle.end();
                std::thread::sleep(Duration::from_millis(300));
            }
            if let Some(ws) = self.ws.take() {
                let user = ws.user.clone();
                let _ = self.rt.block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), user.logout()).await
                });
            }
        }
    }

    impl TechApp {
        fn banner(&self, ui: &mut egui::Ui, p: &Palette) {
            if let Some(label) = &self.args.environment_label {
                egui::Frame::new()
                    .fill(p.warning.gamma_multiply(0.12))
                    .stroke(Stroke::new(1.0, p.warning.gamma_multiply(0.4)))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.label(RichText::new(label).size(12.5).color(p.warning));
                    });
            }
        }

        fn login_screen(&mut self, ui: &mut egui::Ui) {
            let p = theme::palette(ui);
            let working = matches!(*self.login.lock().unwrap(), LoginState::Working(_));
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.5 - 250.0).max(24.0));
                ui.set_max_width(400.0);
                theme::brand_mark(ui, 56.0);
                ui.add_space(10.0);
                ui.label(theme::title("Aionix Connect Técnico", 24.0));
                ui.label(RichText::new("Entre para acessar os seus dispositivos").color(p.muted));
                ui.add_space(18.0);
                theme::card(ui).show(ui, |ui| {
                    ui.set_width(360.0);
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.label(RichText::new("E-mail").size(13.0).color(p.muted));
                        let email = ui.add_enabled(
                            !working,
                            egui::TextEdit::singleline(&mut self.email)
                                .hint_text("voce@empresa.com.br")
                                .desired_width(f32::INFINITY)
                                .margin(egui::Margin::symmetric(10, 8)),
                        );
                        ui.add_space(4.0);
                        ui.label(RichText::new("Senha").size(13.0).color(p.muted));
                        let pw = ui.add_enabled(
                            !working,
                            egui::TextEdit::singleline(&mut self.password)
                                .password(true)
                                .desired_width(f32::INFINITY)
                                .margin(egui::Margin::symmetric(10, 8)),
                        );
                        if self.email.is_empty() {
                            email.request_focus();
                        }
                        ui.add_space(10.0);
                        let enter = (pw.lost_focus() || email.lost_focus())
                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let can =
                            !working && !self.email.trim().is_empty() && !self.password.is_empty();
                        let label = match &*self.login.lock().unwrap() {
                            LoginState::Working(s) => *s,
                            _ => "Entrar",
                        };
                        if theme::primary_button(ui, label, can, Some(ui.available_width()))
                            .clicked()
                            || (enter && can)
                        {
                            self.begin_login(ui.ctx());
                        }
                        if working {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.label(RichText::new("Aguarde...").size(13.0).color(p.muted));
                            });
                        }
                        if let LoginState::Failed(e) = &*self.login.lock().unwrap() {
                            ui.label(RichText::new(e).size(13.0).color(p.danger));
                        }
                    });
                });
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("Servidor: {}", self.args.server))
                        .size(12.0)
                        .color(p.muted),
                );
                ui.label(
                    RichText::new(format!("Registro: {}", self.log_path.display()))
                        .size(11.0)
                        .color(p.muted),
                );
                self.banner(ui, &p);
            });
        }

        fn begin_login(&mut self, ctx: &egui::Context) {
            let (server, dir, email, password) = (
                self.args.server.clone(),
                self.dir.clone(),
                self.email.clone(),
                self.password.clone(),
            );
            let (status, pending, ctx) = (self.login.clone(), self.pending_ws.clone(), ctx.clone());
            *status.lock().unwrap() = LoginState::Working("Autenticando...");
            self.rt.spawn(async move {
                match sign_in(server, dir, email, password, status.clone(), ctx.clone()).await {
                    Ok(ws) => *pending.lock().unwrap() = Some(ws),
                    Err(e) => {
                        warn!(error = %e, "sign-in failed");
                        *status.lock().unwrap() = LoginState::Failed(e);
                    }
                }
                ctx.request_repaint();
            });
        }

        fn top_bar(&mut self, ui: &mut egui::Ui) {
            let p = theme::palette(ui);
            let mut logout = false;
            egui::Panel::top("top")
                .exact_size(60.0)
                .frame(
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, p.border))
                        .inner_margin(egui::Margin::symmetric(20, 10)),
                )
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        theme::brand_mark(ui, 30.0);
                        ui.label(theme::title("Aionix Connect", 16.0));
                        ui.label(RichText::new("Técnico").color(p.muted));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::secondary_button(ui, "Sair", true, None).clicked() {
                                logout = true;
                            }
                            if let Some(ws) = &self.ws {
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.label(RichText::new(&ws.profile.display_name).size(13.5));
                                    let org = ws
                                        .org
                                        .as_ref()
                                        .map(|o| o.1.as_str())
                                        .unwrap_or("sem organização");
                                    ui.label(RichText::new(org).size(12.0).color(p.muted));
                                });
                            }
                        });
                    });
                });
            if let Some(label) = &self.args.environment_label {
                egui::Panel::top("env-label")
                    .exact_size(26.0)
                    .frame(
                        egui::Frame::new()
                            .fill(p.warning.gamma_multiply(0.12))
                            .inner_margin(egui::Margin::symmetric(20, 4)),
                    )
                    .show(ui, |ui| {
                        ui.label(RichText::new(label).size(12.0).color(p.warning));
                    });
            }
            if logout {
                if let Some(ws) = self.ws.take() {
                    let user = ws.user.clone();
                    self.rt.spawn(async move {
                        let _ = user.logout().await;
                    });
                }
                self.next = Some(Screen::Login);
            }
        }

        fn devices_screen(&mut self, ui: &mut egui::Ui) {
            let p = theme::palette(ui);
            let Some(ws) = self.ws.clone() else { return };
            let mut start: Option<(FriendlyId, String, String)> = None;
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(theme::title("Dispositivos", 24.0));
                    ui.label(
                        RichText::new(
                            "Computadores da sua organização e o seu estado em tempo real.",
                        )
                        .color(p.muted),
                    );
                });
            });
            ui.add_space(14.0);
            // Connect by ID (quick support devices, or any device by its ID).
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(theme::title("Conectar por ID", 15.0));
                        ui.label(
                            RichText::new("Peça ao usuário o ID exibido no Aionix Connect.")
                                .size(13.0)
                                .color(p.muted),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let parsed = self.connect_id.parse::<FriendlyId>();
                        if theme::primary_button(ui, "Conectar", parsed.is_ok(), Some(120.0))
                            .clicked()
                            && let Ok(id) = parsed
                        {
                            start = Some((id, format!("ID {id}"), String::new()));
                        }
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.connect_id)
                                .hint_text("000 000 000")
                                .font(theme::semibold(17.0))
                                .desired_width(170.0)
                                .margin(egui::Margin::symmetric(10, 7)),
                        );
                        if r.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            && let Ok(id) = self.connect_id.parse::<FriendlyId>()
                        {
                            start = Some((id, format!("ID {id}"), String::new()));
                        }
                    });
                });
                if let Some(e) = &self.connect_error {
                    ui.label(RichText::new(e).size(13.0).color(p.danger));
                }
            });
            ui.add_space(14.0);

            let st = ws.devices.lock().unwrap();
            ui.horizontal(|ui| {
                let online = st
                    .devices
                    .iter()
                    .filter(|d| d.presence == PresenceState::Online)
                    .count();
                ui.label(theme::title(
                    format!("{} dispositivo(s)", st.devices.len()),
                    15.0,
                ));
                ui.label(RichText::new(format!("· {online} online")).color(p.muted));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("Buscar")
                            .desired_width(220.0)
                            .margin(egui::Margin::symmetric(10, 6)),
                    );
                });
            });
            if let Some(e) = &st.error {
                ui.label(
                    RichText::new(format!("Não foi possível atualizar a lista: {e}"))
                        .size(13.0)
                        .color(p.danger),
                );
            }
            ui.add_space(6.0);
            if !st.loaded {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new("Carregando dispositivos...").color(p.muted));
                });
            } else if ws.org.is_none() {
                ui.label(
                    RichText::new(
                        "Sua conta não pertence a nenhuma organização. Use Conectar por ID.",
                    )
                    .color(p.muted),
                );
            } else if st.devices.is_empty() {
                ui.label(RichText::new("Nenhum dispositivo cadastrado ainda.").color(p.muted));
            }
            let filter = self.filter.to_lowercase();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for d in st.devices.iter().filter(|d| {
                        filter.is_empty()
                            || d.display_name.to_lowercase().contains(&filter)
                            || d.hostname.to_lowercase().contains(&filter)
                            || d.friendly_id.compact().contains(&filter.replace(' ', ""))
                    }) {
                        if let Some(s) = device_row(ui, &p, d, &self.local_host) {
                            start = Some(s);
                        }
                        ui.add_space(8.0);
                    }
                });
            drop(st);
            if let Some((id, name, hostname)) = start {
                self.connect_error = None;
                self.start_session(ui.ctx(), id, name, &hostname);
            }
        }

        /// Returns false when the user leaves the session view.
        fn session_screen(&mut self, ui: &mut egui::Ui, s: &mut ActiveSession) -> bool {
            let p = theme::palette(ui);
            let status = s.handle.status();
            let diag = s.handle.diagnostics();
            let live = status.state.is_live();
            let terminal = status.state.is_terminal();
            // Drain session events.
            if let Some(rx) = s.events.as_mut() {
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        TechnicianEvent::HostName(n) => {
                            // Connected by ID to this very computer: the injected cursor
                            // would chase the viewer, so control starts off here too.
                            if !s.local_machine && n.eq_ignore_ascii_case(&self.local_host) {
                                s.local_machine = true;
                                s.control = false;
                            }
                            s.host_name = Some(n);
                        }
                        TechnicianEvent::StreamStarted(st) => s.stream = Some(st),
                        TechnicianEvent::CursorShape(c) => {
                            if c.width > 0
                                && c.height > 0
                                && c.rgba.len() == (c.width * c.height * 4) as usize
                            {
                                let img = egui::ColorImage::from_rgba_unmultiplied(
                                    [c.width as usize, c.height as usize],
                                    &c.rgba,
                                );
                                let tex = ui.ctx().load_texture(
                                    format!("cursor-{}", c.id),
                                    img,
                                    TextureOptions::LINEAR,
                                );
                                s.cursors.insert(
                                    c.id,
                                    CursorTex {
                                        tex,
                                        hotspot: Vec2::new(c.hotspot_x as f32, c.hotspot_y as f32),
                                        size: Vec2::new(c.width as f32, c.height as f32),
                                    },
                                );
                            }
                        }
                        TechnicianEvent::CursorPosition(c) => s.cursor = Some(c),
                        TechnicianEvent::Notice(_) => {}
                    }
                }
            }
            if let Some(img) = s.frames.take() {
                match &mut s.texture {
                    Some(t) => t.set(img, TextureOptions::LINEAR),
                    None => {
                        s.texture = Some(ui.ctx().load_texture(
                            "remote-screen",
                            img,
                            TextureOptions::LINEAR,
                        ))
                    }
                }
            }
            let mut leave = false;
            let mut retry = false;
            let can_control = s.handle.permissions().is_some_and(|x| x.control);

            // ---- toolbar
            egui::Panel::top("session-bar")
                .exact_size(56.0)
                .frame(
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, p.border))
                        .inner_margin(egui::Margin::symmetric(16, 8)),
                )
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        if terminal
                            && theme::secondary_button(ui, "‹  Dispositivos", true, None).clicked()
                        {
                            leave = true;
                        }
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let title = s.host_name.clone().unwrap_or_else(|| s.name.clone());
                            ui.label(theme::title(title, 15.0));
                            ui.label(
                                RichText::new(format!("ID {}", s.target))
                                    .size(12.0)
                                    .color(p.muted),
                            );
                        });
                        ui.add_space(8.0);
                        theme::status_pill(
                            ui,
                            common::state_text(status.state),
                            session_color(&p, status.state),
                        );
                        if live {
                            let mut parts = Vec::new();
                            if let Some(t) = diag.transport.value() {
                                parts.push(transport_text(*t).to_owned());
                            }
                            if let Some(r) = diag.rtt_ms.value() {
                                parts.push(format!("{r:.0} ms"));
                            }
                            if let Some(f) = diag.fps.value() {
                                parts.push(format!("{f:.0} fps"));
                            }
                            if let Some(res) = diag.resolution.value() {
                                parts.push(format!("{}×{}", res.width, res.height));
                            }
                            ui.label(RichText::new(parts.join("  ·  ")).size(12.5).color(p.muted));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if !terminal {
                                if theme::danger_button(ui, "Encerrar", true, None).clicked() {
                                    s.handle.end();
                                }
                            } else if theme::primary_button(ui, "Conectar novamente", true, None)
                                .clicked()
                            {
                                retry = true;
                            }
                            theme::toggle_chip(ui, &mut s.show_diag, "Diagnóstico", true);
                            let before = s.control;
                            theme::toggle_chip(ui, &mut s.control, "Controle", can_control && live)
                                .on_hover_text("Enviar mouse e teclado ao computador remoto");
                            if before && !s.control {
                                release_all(s);
                            }
                        });
                    });
                });

            if s.show_diag {
                egui::Panel::right("diag")
                    .exact_size(300.0)
                    .frame(
                        egui::Frame::new()
                            .fill(p.surface)
                            .stroke(Stroke::new(1.0, p.border))
                            .inner_margin(egui::Margin::same(16)),
                    )
                    .show(ui, |ui| diagnostics_panel(ui, s, &diag, &status));
            }

            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(if live { Color32::from_rgb(8, 9, 12) } else { p.bg }))
                .show(ui, |ui| {
                    let area = ui.available_rect_before_wrap();
                    if s.local_machine && !terminal {
                        let r = Rect::from_min_size(area.min + Vec2::new(12.0, 12.0), Vec2::new(area.width() - 24.0, 30.0));
                        ui.put(r, egui::Label::new(RichText::new("Validação local: o dispositivo é este mesmo computador. O controle fica desligado para o cursor não se perseguir; ligue-o em \"Controle\" se quiser testar.").size(12.5).color(p.warning)));
                    }
                    match (&s.texture, live || status.state == SessionState::Reconnecting) {
                        (Some(tex), true) => {
                            let img_rect = fit_rect(area.shrink(if s.local_machine { 44.0 } else { 0.0 }), tex.size_vec2());
                            ui.painter().image(tex.id(), img_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                            self.viewer_input(ui, s, img_rect, can_control && live);
                            draw_cursor(ui, s, img_rect);
                            if status.state == SessionState::Reconnecting {
                                overlay(ui, area, &p, "Reconectando...", "A conexão caiu. Tentando retomar a sessão.", true);
                            }
                        }
                        _ => {
                            let (headline, detail, spin) = state_message(&status, s);
                            overlay(ui, area, &p, &headline, &detail, spin);
                        }
                    }
                });
            if retry {
                let (id, name) = (s.target, s.name.clone());
                let host = if s.local_machine {
                    self.local_host.clone()
                } else {
                    String::new()
                };
                self.start_session(ui.ctx(), id, name, &host);
                return true;
            }
            if leave {
                return false;
            }
            true
        }

        fn viewer_input(&self, ui: &mut egui::Ui, s: &mut ActiveSession, img: Rect, allowed: bool) {
            let id = ui.id().with("remote-viewer");
            let resp = ui.interact(img, id, Sense::click_and_drag());
            if !(allowed && s.control) {
                if s.input.focused {
                    release_all(s);
                }
                return;
            }
            if resp.clicked() || resp.drag_started() || resp.is_pointer_button_down_on() {
                resp.request_focus();
            }
            let focused = resp.has_focus();
            if focused {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        id,
                        egui::EventFilter {
                            tab: true,
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        },
                    )
                });
            }
            if s.input.focused && !focused {
                release_all(s);
            }
            s.input.focused = focused;
            let (sw, sh) = s
                .stream
                .as_ref()
                .filter(|st| st.source_width > 0)
                .map(|st| (st.source_width as f32, st.source_height as f32))
                .or_else(|| {
                    s.texture
                        .as_ref()
                        .map(|t| (t.size_vec2().x, t.size_vec2().y))
                })
                .unwrap_or((1920.0, 1080.0));
            let map = |pos: Pos2| -> (i32, i32) {
                let x = ((pos.x - img.min.x) / img.width() * sw).clamp(0.0, sw - 1.0);
                let y = ((pos.y - img.min.y) / img.height() * sh).clamp(0.0, sh - 1.0);
                (x as i32, y as i32)
            };
            if resp.hovered() || resp.is_pointer_button_down_on() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            }
            let events = ui.input(|i| i.events.clone());
            let handle = s.handle.clone();
            let send = |e: pb::input_event::Event| {
                let _ = handle.send_input(pb::InputEvent { event: Some(e) });
            };
            for ev in events {
                match ev {
                    egui::Event::PointerMoved(pos)
                        if img.contains(pos) || resp.is_pointer_button_down_on() =>
                    {
                        let (x, y) = map(pos);
                        if s.input.last_pos != Some((x, y)) {
                            s.input.last_pos = Some((x, y));
                            send(pb::input_event::Event::MouseMove(pb::MouseMove { x, y }));
                        }
                    }
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        ..
                    } if img.contains(pos) || !pressed => {
                        if !pressed && !resp.has_focus() && !img.contains(pos) {
                            continue;
                        }
                        let kind = match button {
                            egui::PointerButton::Primary => pb::MouseButtonKind::MouseButtonLeft,
                            egui::PointerButton::Secondary => pb::MouseButtonKind::MouseButtonRight,
                            egui::PointerButton::Middle => pb::MouseButtonKind::MouseButtonMiddle,
                            egui::PointerButton::Extra1 => pb::MouseButtonKind::MouseButtonBack,
                            egui::PointerButton::Extra2 => pb::MouseButtonKind::MouseButtonForward,
                        };
                        let (x, y) = map(pos);
                        send(pb::input_event::Event::MouseButton(pb::MouseButton {
                            button: kind as i32,
                            down: pressed,
                            x,
                            y,
                        }));
                    }
                    egui::Event::MouseWheel { unit, delta, .. } if resp.hovered() => {
                        let scale = match unit {
                            egui::MouseWheelUnit::Line => 120.0,
                            egui::MouseWheelUnit::Point => 120.0 / 50.0,
                            egui::MouseWheelUnit::Page => 360.0,
                        };
                        let (dx, dy) = ((-delta.x * scale) as i32, (delta.y * scale) as i32);
                        if dx != 0 || dy != 0 {
                            send(pb::input_event::Event::MouseWheel(pb::MouseWheel {
                                delta_x: dx,
                                delta_y: dy,
                            }));
                        }
                    }
                    egui::Event::ModifiersChanged(m) if focused => sync_modifiers(s, m),
                    egui::Event::Key {
                        key,
                        physical_key,
                        pressed,
                        modifiers,
                        ..
                    } if focused => {
                        sync_modifiers(s, modifiers);
                        let k = physical_key.unwrap_or(key);
                        if keymap::is_modifier(k) {
                            continue;
                        }
                        match keymap::scancode(k) {
                            Some((code, extended)) => {
                                if pressed {
                                    s.input.last_key_unmapped = false;
                                }
                                send(pb::input_event::Event::Key(pb::KeyEvent {
                                    scancode: code,
                                    extended,
                                    down: pressed,
                                }));
                            }
                            None => {
                                if pressed {
                                    s.input.last_key_unmapped = true;
                                }
                            }
                        }
                    }
                    egui::Event::Text(t) if focused && s.input.last_key_unmapped => {
                        s.input.last_key_unmapped = false;
                        send(pb::input_event::Event::Text(pb::TextInput { text: t }));
                    }
                    egui::Event::Ime(egui::ImeEvent::Commit(t)) if focused => {
                        send(pb::input_event::Event::Text(pb::TextInput { text: t }));
                    }
                    egui::Event::WindowFocused(false) => release_all(s),
                    _ => {}
                }
            }
            if focused {
                let m = ui.input(|i| i.modifiers);
                sync_modifiers(s, m);
            }
        }
    }

    fn sync_modifiers(s: &mut ActiveSession, m: egui::Modifiers) {
        let old = s.input.mods;
        for (was, now, key) in [
            (old.shift, m.shift, keymap::SHIFT),
            (old.ctrl, m.ctrl, keymap::CTRL),
            (old.alt, m.alt, keymap::ALT),
        ] {
            if was != now {
                let _ = s.handle.send_input(pb::InputEvent {
                    event: Some(pb::input_event::Event::Key(pb::KeyEvent {
                        scancode: key.0,
                        extended: key.1,
                        down: now,
                    })),
                });
            }
        }
        s.input.mods = m;
    }

    fn release_all(s: &mut ActiveSession) {
        let _ = s.handle.send_input(pb::InputEvent {
            event: Some(pb::input_event::Event::ReleaseAll(pb::ReleaseAll {})),
        });
        s.input = InputState::default();
    }

    fn fit_rect(area: Rect, size: Vec2) -> Rect {
        if size.x <= 0.0 || size.y <= 0.0 {
            return area;
        }
        let scale = (area.width() / size.x).min(area.height() / size.y);
        Rect::from_center_size(area.center(), size * scale)
    }

    fn draw_cursor(ui: &egui::Ui, s: &ActiveSession, img: Rect) {
        let Some(c) = &s.cursor else { return };
        if !c.visible {
            return;
        }
        let Some(shape) = s.cursors.get(&c.shape_id) else {
            return;
        };
        let (sw, sh) = s
            .stream
            .as_ref()
            .filter(|st| st.source_width > 0)
            .map(|st| (st.source_width as f32, st.source_height as f32))
            .unwrap_or((img.width(), img.height()));
        let scale = img.width() / sw;
        let pos = Pos2::new(
            img.min.x + c.x as f32 * scale,
            img.min.y + c.y as f32 * (img.height() / sh),
        );
        let size = shape.size * scale.clamp(0.6, 1.5);
        let min = pos - shape.hotspot * (size.x / shape.size.x);
        ui.painter().image(
            shape.tex.id(),
            Rect::from_min_size(min, size),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    fn overlay(
        ui: &mut egui::Ui,
        area: Rect,
        p: &Palette,
        headline: &str,
        detail: &str,
        spin: bool,
    ) {
        let card = Rect::from_center_size(area.center(), Vec2::new(440.0, 170.0));
        ui.painter().rect(
            card,
            CornerRadius::same(16),
            p.surface,
            Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(card.shrink(24.0))
                .layout(Layout::top_down(Align::Center)),
        );
        child.add_space(6.0);
        if spin {
            child.add(egui::Spinner::new().size(26.0).color(p.accent));
        }
        child.add_space(6.0);
        child.label(theme::title(headline, 19.0));
        child.label(RichText::new(detail).size(13.5).color(p.muted));
    }

    fn state_message(
        status: &aionix_remote_core::session::state::SessionStatus,
        s: &ActiveSession,
    ) -> (String, String, bool) {
        let name = s.host_name.clone().unwrap_or_else(|| s.name.clone());
        match status.state {
            SessionState::Idle | SessionState::Requesting => (
                "Conectando...".into(),
                "Verificando permissões e localizando o dispositivo.".into(),
                true,
            ),
            SessionState::Authenticating => (
                "Autenticando...".into(),
                "Apresentando a autorização assinada ao dispositivo.".into(),
                true,
            ),
            SessionState::AwaitingApproval => (
                "Aguardando autorização...".into(),
                format!("O usuário de {name} precisa clicar em Permitir."),
                true,
            ),
            SessionState::Establishing => (
                "Estabelecendo conexão segura...".into(),
                "Iniciando vídeo criptografado de ponta a ponta.".into(),
                true,
            ),
            SessionState::Connected | SessionState::Degraded => (
                "Conectado".into(),
                "Aguardando a primeira imagem...".into(),
                true,
            ),
            SessionState::Reconnecting => (
                "Reconectando...".into(),
                "A conexão caiu. Tentando retomar a sessão.".into(),
                true,
            ),
            SessionState::Ending => ("Encerrando...".into(), String::new(), true),
            SessionState::Ended => (
                "Sessão encerrada".into(),
                format!("Duração: {}", fmt_duration(s.started.elapsed())),
                false,
            ),
            SessionState::Rejected => (
                "Acesso recusado".into(),
                common::outcome_text(status),
                false,
            ),
            SessionState::Failed => {
                let text = common::outcome_text(status);
                let head = if text.contains("offline") {
                    "Dispositivo offline"
                } else {
                    "Erro ao conectar"
                };
                (head.into(), text, false)
            }
        }
    }

    fn fmt_duration(d: Duration) -> String {
        let s = d.as_secs();
        format!("{:02}:{:02}", s / 60, s % 60)
    }

    fn diagnostics_panel(
        ui: &mut egui::Ui,
        s: &ActiveSession,
        d: &aionix_remote_core::session::diagnostics::SessionDiagnostics,
        status: &aionix_remote_core::session::state::SessionStatus,
    ) {
        use aionix_remote_core::session::diagnostics::Metric;
        fn m<T>(v: &Metric<T>, f: impl Fn(&T) -> String) -> String {
            v.value().map(f).unwrap_or_else(|| "—".into())
        }
        ui.label(theme::title("Diagnóstico da sessão", 15.0));
        ui.add_space(6.0);
        theme::kv(ui, "Estado", common::state_text(status.state));
        theme::kv(
            ui,
            "Sessão",
            &m(&d.session_id, |v| v.to_string()[..8].to_owned()),
        );
        theme::kv(
            ui,
            "Transporte",
            &m(&d.transport, |t| transport_text(*t).into()),
        );
        theme::kv(
            ui,
            "Latência (RTT)",
            &m(&d.rtt_ms, |v| format!("{v:.1} ms")),
        );
        theme::kv(
            ui,
            "Perda de pacotes",
            &m(&d.packet_loss, |v| format!("{:.2} %", v * 100.0)),
        );
        theme::kv(
            ui,
            "Resolução",
            &m(&d.resolution, |r| format!("{}×{}", r.width, r.height)),
        );
        theme::kv(ui, "Quadros/s", &m(&d.fps, |v| format!("{v:.0}")));
        theme::kv(
            ui,
            "Taxa de vídeo",
            &m(&d.bitrate_kbps, |v| format!("{v:.0} kbit/s")),
        );
        theme::kv(ui, "Captura (host)", &m(&d.capture_backend, |v| v.clone()));
        theme::kv(ui, "Codificador (host)", &m(&d.encoder, |v| v.clone()));
        theme::kv(
            ui,
            "Codificação",
            &m(&d.encode_latency_ms, |v| format!("{v:.1} ms")),
        );
        theme::kv(ui, "Decodificador", &m(&d.decoder, |v| v.clone()));
        theme::kv(
            ui,
            "Decodificação",
            &m(&d.decode_latency_ms, |v| format!("{v:.1} ms")),
        );
        theme::kv(
            ui,
            "Quadros descartados",
            &m(&d.dropped_frames, |v| v.to_string()),
        );
        let (rx, shown) = s.frames.counters();
        theme::kv(ui, "Quadros exibidos", &format!("{shown} de {rx}"));
        theme::kv(ui, "Reconexões", &status.reconnect_count.to_string());
        theme::kv(ui, "Duração", &m(&d.duration, |v| fmt_duration(*v)));
        if let Some(p) = s.handle.permissions() {
            let mut v = vec!["ver"];
            if p.control {
                v.push("controle");
            }
            if p.clipboard {
                v.push("área de transferência");
            }
            theme::kv(ui, "Permissões", &v.join(", "));
        }
        ui.add_space(10.0);
        if ui
            .small_button("Simular queda de conexão")
            .on_hover_text("Teste de reconexão: derruba a conexão atual")
            .clicked()
        {
            s.handle.inject_transport_failure();
        }
    }

    fn device_row(
        ui: &mut egui::Ui,
        p: &Palette,
        d: &DeviceSummary,
        local_host: &str,
    ) -> Option<(FriendlyId, String, String)> {
        let mut clicked = None;
        theme::card(ui)
            .inner_margin(egui::Margin::symmetric(18, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(10), p.surface_alt);
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "🖥",
                        egui::FontId::proportional(20.0),
                        p.muted,
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let name = if d.display_name.is_empty() {
                            &d.hostname
                        } else {
                            &d.display_name
                        };
                        ui.horizontal(|ui| {
                            ui.label(theme::title(name, 15.0));
                            if d.hostname.eq_ignore_ascii_case(local_host) {
                                ui.label(RichText::new("este computador").size(11.5).color(p.info));
                            }
                        });
                        let mut sub =
                            format!("ID {}  ·  {} {}", d.friendly_id, d.os_name, d.os_version);
                        if let Some(c) = &d.customer_name {
                            sub.push_str(&format!("  ·  {c}"));
                        }
                        ui.label(RichText::new(sub).size(12.5).color(p.muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let online = d.presence == PresenceState::Online;
                        if theme::primary_button(ui, "Conectar", online, Some(112.0)).clicked() {
                            let name = if d.display_name.is_empty() {
                                d.hostname.clone()
                            } else {
                                d.display_name.clone()
                            };
                            clicked = Some((d.friendly_id, name, d.hostname.clone()));
                        }
                        ui.add_space(8.0);
                        let (text, color) = match d.presence {
                            PresenceState::Online => ("Online", p.success),
                            PresenceState::Connecting => ("Conectando", p.warning),
                            PresenceState::Offline => ("Offline", p.muted),
                        };
                        theme::status_pill(ui, text, color);
                    });
                });
            });
        clicked
    }

    fn transport_text(t: TransportKind) -> &'static str {
        match t {
            TransportKind::Lan => "Rede local",
            TransportKind::Direct => "Direto",
            TransportKind::Relay => "Via relay",
            TransportKind::Unknown => "Determinando",
        }
    }

    fn session_color(p: &Palette, s: SessionState) -> Color32 {
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
