//! Connecting side of the unified Aionix Connect app ("Acessar outro computador").
//!
//! sign in once (rotating refresh token kept DPAPI-protected, so the login survives
//! restarts and updates) → workstation enrollment bound to the user → QUIC endpoint on
//! the workstation key → organization devices with live presence → session request
//! through `ControlPlaneAuthority` → Remote Core technician session: H.264 decode,
//! viewer, input, clipboard, diagnostics.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    admin_side::AdminHandle,
    admin_views::{self, PasswordForm, SheetAction},
    common, keymap, theme,
    video::{FrameSlot, VideoSink},
    views,
};
use aionix_client::{
    ApiClient, ClientError,
    authority::{ControlPlaneAuthority, UserSession},
    enroll::{DeviceSession, Enrollment, ensure_enrolled},
    identity::{FileIdentityStore, Protection},
};
use aionix_control_api::{
    claims::SessionPermission,
    rest::{
        Customer, DeviceGroup, DeviceInfo, DeviceSummary, Member, OrgRole, PresenceState,
        SessionEventKind, SessionEventReport, UpdateDeviceRequest, UserProfile,
    },
};
use aionix_protocol::pb;
use aionix_remote_core::session::{
    DeviceLookup, Password, TechnicianConfig, TechnicianEvent, TechnicianIdentity,
    TechnicianServices, TechnicianSession, TechnicianSessionHandle,
    state::SessionState,
    windows_media::{MfDecoderFactory, SystemClipboard},
};
use aionix_shared::{FriendlyId, TransportKind};
use aionix_transport::Transport;
use egui::{Color32, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

/// What the connecting side needs to start.
#[derive(Debug, Clone, Default)]
pub struct TechOptions {
    pub server: String,
    /// Folder of the signed-in user's workstation identity and stored login
    /// (`…\Aionix Connect\technician`).
    pub dir: PathBuf,
    /// Pre-filled e-mail (development launcher).
    pub email: Option<String>,
    /// Development launcher only: password pre-fill used with `auto_login`.
    pub password: String,
    /// Development launcher: sign in right away with `email` + `password`.
    pub auto_login: bool,
    /// Development/validation: request a session to this ID right after signing in.
    pub auto_connect: Option<FriendlyId>,
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
    /// Local pointer over the remote image while controlling. The cursor is drawn
    /// here immediately (with the remote shape) instead of waiting a round trip for
    /// the host to report its new position — on a 80-100 ms link that wait is what
    /// makes the pointer feel like rubber.
    local_pointer: Option<Pos2>,
    mods: egui::Modifiers,
    last_key_unmapped: bool,
}

enum Screen {
    Home,
    Session(Box<ActiveSession>),
}

/// The connecting side of the app: sign-in (kept across restarts), the
/// organization's devices and the remote session viewer.
pub struct TechSide {
    opts: TechOptions,
    rt: tokio::runtime::Handle,
    screen: Screen,
    /// Screen to switch to after the current frame (set by handlers).
    next: Option<Screen>,
    email: String,
    password: String,
    login: Arc<Mutex<LoginState>>,
    pending_ws: Arc<Mutex<Option<Workspace>>>,
    ws: Option<Arc<Workspace>>,
    /// The sign-in form is open in the connect panel.
    show_login: bool,
    /// ID asked for while signed out; the session starts right after sign-in.
    connect_after_login: Option<FriendlyId>,
    connect_id: String,
    connect_error: Option<String>,
    filter: String,
    local_host: String,
    /// This computer's own ID (from the receiving side), to recognise self-connections.
    own_id: Option<FriendlyId>,
    /// Signed in with a temporary password: the login waits here until a new password
    /// is chosen (the server refuses the workstation until then).
    pending_change: Arc<Mutex<Option<UserSession>>>,
    /// The temporary password just typed, used as the current one for the change.
    change_current: String,
    password_sheet: Option<PasswordSheet>,
    /// The device password being asked for before an unattended connection (ADR-0015).
    unattended: Option<UnattendedPrompt>,
    /// Customers, groups and technicians for the management sheets.
    catalog: Arc<Mutex<Catalog>>,
    /// "Editar computador", while open.
    device_edit: Option<DeviceEdit>,
    /// "Clientes e grupos" is open (it replaces the computer sheet while shown).
    organize_open: bool,
    organize_work: Arc<Mutex<Work>>,
    new_customer: String,
    new_group: String,
    password_form: PasswordForm,
    password_task: Arc<Mutex<PasswordTask>>,
    /// Stops the background retries of the stored login (manual sign-in, sign-out).
    resume_cancel: Arc<AtomicBool>,
}

/// A computer whose owner allowed unattended access, waiting for the device password.
struct UnattendedPrompt {
    id: FriendlyId,
    name: String,
    hostname: String,
    password: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasswordSheet {
    /// After signing in with a temporary password.
    Mandatory,
    /// "Alterar senha" from the account row.
    Voluntary,
}

enum PasswordTask {
    Idle,
    Working,
    Failed(String),
    Done,
}

impl TechSide {
    pub fn new(rt: &tokio::runtime::Handle, opts: TechOptions, ctx: &egui::Context) -> Self {
        let mut me = Self {
            email: opts.email.clone().unwrap_or_default(),
            password: opts.password.clone(),
            opts,
            rt: rt.clone(),
            screen: Screen::Home,
            next: None,
            login: Arc::new(Mutex::new(LoginState::Idle)),
            pending_ws: Arc::new(Mutex::new(None)),
            ws: None,
            show_login: false,
            connect_after_login: None,
            connect_id: String::new(),
            connect_error: None,
            filter: String::new(),
            local_host: aionix_platform::windows::sysinfo::hostname(),
            own_id: None,
            pending_change: Arc::default(),
            change_current: String::new(),
            password_sheet: None,
            unattended: None,
            catalog: Arc::default(),
            device_edit: None,
            organize_open: false,
            organize_work: Arc::default(),
            new_customer: String::new(),
            new_group: String::new(),
            password_form: PasswordForm::default(),
            password_task: Arc::new(Mutex::new(PasswordTask::Idle)),
            resume_cancel: Arc::new(AtomicBool::new(false)),
        };
        if me.opts.auto_login && !me.email.is_empty() && !me.password.is_empty() {
            me.begin_login(ctx);
        } else {
            me.resume_saved(ctx);
        }
        me
    }
}

// ------------------------------------------------------------------ stored login

/// Refresh token of the signed-in user, DPAPI-protected for this Windows user. It is
/// rewritten on every rotation: presenting an old (rotated) token revokes the login.
fn login_file(dir: &std::path::Path) -> PathBuf {
    dir.join("login.bin")
}

fn save_refresh(path: &std::path::Path, token: &str) {
    use aionix_platform::{DataScope, windows::dpapi::protect};
    let Ok(sealed) = protect(token.as_bytes(), DataScope::User) else {
        warn!("could not protect the stored login");
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, sealed).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn load_refresh(path: &std::path::Path) -> Option<String> {
    let sealed = std::fs::read(path).ok()?;
    let plain = aionix_platform::windows::dpapi::unprotect(&sealed).ok()?;
    String::from_utf8(plain).ok().filter(|t| !t.is_empty())
}

fn forget_login(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

// ------------------------------------------------------------------ login

async fn login_with_password(
    server: &str,
    email: &str,
    password: &str,
) -> Result<UserSession, String> {
    let api = ApiClient::new(server).map_err(|e| e.to_string())?;
    UserSession::login(api, email.trim(), password)
        .await
        .map_err(|e| common::client_error_text(&e))
}

/// Opens the workspace (workstation identity, transport) for an authenticated user,
/// keeping the stored login in sync with every token rotation.
async fn open_workspace(
    server: String,
    dir: PathBuf,
    user: UserSession,
    status: Arc<Mutex<LoginState>>,
    ctx: egui::Context,
) -> Result<Workspace, String> {
    let set = |s: &'static str| {
        *status.lock().unwrap() = LoginState::Working(s);
        ctx.request_repaint();
    };
    let api = ApiClient::new(&server).map_err(|e| e.to_string())?;
    let file = login_file(&dir);
    let user = Arc::new(user.persist_with(move |t| save_refresh(&file, t)).await);
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

fn spawn_device_refresh(rt: &tokio::runtime::Handle, ws: Arc<Workspace>, ctx: egui::Context) {
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

impl TechSide {
    fn start_session(
        &mut self,
        ctx: &egui::Context,
        target: FriendlyId,
        name: String,
        hostname: &str,
    ) {
        self.start_session_with(ctx, target, name, hostname, None);
    }

    /// `unattended`: the device password, for a connection without the owner's click.
    fn start_session_with(
        &mut self,
        ctx: &egui::Context,
        target: FriendlyId,
        name: String,
        hostname: &str,
        unattended: Option<Password>,
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
        config.unattended_password = unattended;
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
        let local_machine = !hostname.is_empty() && hostname.eq_ignore_ascii_case(&self.local_host);
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
async fn report_lifecycle(handle: TechnicianSessionHandle, ws: Arc<Workspace>, ctx: egui::Context) {
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

impl TechSide {
    /// This computer's own ID, so connecting to it is treated as a local test.
    pub fn set_own_id(&mut self, id: Option<FriendlyId>) {
        self.own_id = id;
    }

    /// An outbound session view is open (full window).
    pub fn in_session(&self) -> bool {
        matches!(self.screen, Screen::Session(_)) || matches!(self.next, Some(Screen::Session(_)))
    }

    /// Typing in the connect panel or signing in: an update must not restart now.
    pub fn busy(&self) -> bool {
        self.in_session()
            || matches!(*self.login.lock().unwrap(), LoginState::Working(_))
            || self.show_login
            || self.password_sheet.is_some()
            || !self.connect_id.trim().is_empty()
    }

    /// Per-frame bookkeeping; call once per frame before drawing.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if self.password_sheet.is_none() && self.pending_change.lock().unwrap().is_some() {
            // Signed in with a temporary password: choose a new one first.
            self.change_current = std::mem::take(&mut self.password);
            self.password_form = PasswordForm::default();
            *self.password_task.lock().unwrap() = PasswordTask::Idle;
            self.password_sheet = Some(PasswordSheet::Mandatory);
            self.show_login = false;
        }
        let signed_in = self.pending_ws.lock().unwrap().take();
        if let Some(ws) = signed_in {
            let ws = Arc::new(ws);
            spawn_device_refresh(&self.rt, ws.clone(), ctx.clone());
            self.ws = Some(ws);
            *self.login.lock().unwrap() = LoginState::Idle;
            self.password.clear();
            self.change_current.clear();
            if self.password_sheet == Some(PasswordSheet::Mandatory) {
                self.password_sheet = None;
            }
            self.show_login = false;
            if let Some(id) = self
                .opts
                .auto_connect
                .take()
                .or(self.connect_after_login.take())
            {
                self.connect_now(ctx, id, format!("ID {id}"));
            }
        }
        if let Some(next) = self.next.take() {
            self.screen = next;
        }
    }

    /// Full-window remote session; call while `in_session()`.
    pub fn session_view(&mut self, ui: &mut egui::Ui) {
        let mut screen = std::mem::replace(&mut self.screen, Screen::Home);
        if let Screen::Session(s) = &mut screen
            && !self.session_screen(ui, s)
        {
            screen = Screen::Home;
        }
        self.screen = self.next.take().unwrap_or(screen);
    }

    /// App exit: ends an outbound session. The login is kept on purpose.
    pub fn shutdown(&mut self) {
        if let Screen::Session(s) = &self.screen {
            s.handle.end();
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    /// Signed-in user's name and organization (sidebar).
    pub fn account(&self) -> Option<(String, String)> {
        self.ws.as_ref().map(|ws| {
            (
                ws.profile.display_name.clone(),
                ws.org
                    .as_ref()
                    .map(|o| o.1.clone())
                    .unwrap_or_else(|| "Sem organização".into()),
            )
        })
    }

    /// Signing in (by hand or with the stored login).
    pub fn signing_in(&self) -> bool {
        matches!(*self.login.lock().unwrap(), LoginState::Working(_))
    }

    /// Computers of the organization online now (signed in).
    pub fn online_count(&self) -> Option<usize> {
        self.ws.as_ref().map(|ws| {
            ws.devices
                .lock()
                .unwrap()
                .devices
                .iter()
                .filter(|d| d.presence == PresenceState::Online)
                .count()
        })
    }

    /// Opens the sign-in sheet.
    pub fn open_login(&mut self) {
        self.show_login = true;
    }

    /// The sign-in sheet is open.
    pub fn login_open(&self) -> bool {
        self.ws.is_none() && (self.show_login || self.connect_after_login.is_some())
    }

    /// Signs out on this computer (the stored login is deleted).
    pub fn logout(&mut self) {
        self.resume_cancel.store(true, Ordering::SeqCst);
        self.pending_change.lock().unwrap().take();
        self.password_sheet = None;
        if let Some(ws) = self.ws.take() {
            let user = ws.user.clone();
            self.rt.spawn(async move {
                let _ = user.logout().await;
            });
        }
        forget_login(&login_file(&self.opts.dir));
        self.show_login = false;
        self.connect_after_login = None;
        *self.login.lock().unwrap() = LoginState::Idle;
    }

    /// Início, "Acessar outro computador": connect by ID (signing in first when
    /// needed) and, signed in, the computers online now. Returns a page to open.
    pub fn connect_section(&mut self, ui: &mut egui::Ui) -> Option<views::Page> {
        let ws = self.ws.clone();
        let (quick_src, total, stale): (Vec<DeviceSummary>, usize, bool) = match &ws {
            Some(ws) => {
                let st = ws.devices.lock().unwrap();
                (
                    st.devices
                        .iter()
                        .filter(|d| d.presence == PresenceState::Online)
                        .take(5)
                        .cloned()
                        .collect(),
                    st.devices.len(),
                    st.error.is_some(),
                )
            }
            None => (Vec::new(), 0, false),
        };
        let quick: Vec<_> = quick_src
            .iter()
            .map(|d| device_model(d, &self.local_host))
            .collect();
        let m = views::ConnectModel {
            signed_in: ws.is_some(),
            signing_in: self.signing_in(),
            error: self.connect_error.as_deref(),
            quick: &quick,
            total,
            stale,
        };
        match views::connect_section(ui, &m, &mut self.connect_id) {
            Some(views::ConnectAction::Connect(digits)) => {
                if let Ok(id) = digits.parse::<FriendlyId>() {
                    self.connect_error = None;
                    if ws.is_some() {
                        self.connect_id.clear();
                        self.connect_now(ui.ctx(), id, format!("ID {id}"));
                    } else {
                        self.connect_after_login = Some(id);
                        self.show_login = true;
                    }
                }
            }
            Some(views::ConnectAction::SignIn) => self.show_login = true,
            Some(views::ConnectAction::Device(i)) => {
                if let Some(d) = quick_src.get(i) {
                    self.connect_error = None;
                    let (id, name, host) = (d.friendly_id, device_name(d), d.hostname.clone());
                    self.request_device(ui.ctx(), id, name, host, unattended_ready(d));
                }
            }
            Some(views::ConnectAction::ShowAll) => return Some(views::Page::Computers),
            None => {}
        }
        None
    }

    /// "Computadores": the organization's computers with live presence.
    pub fn computers_page(&mut self, ui: &mut egui::Ui) {
        let Some(ws) = self.ws.clone() else { return };
        let can_manage = self.can_manage();
        let picked = {
            let st = ws.devices.lock().unwrap();
            let filter = self.filter.trim().to_lowercase();
            let digits: String = filter.chars().filter(char::is_ascii_digit).collect();
            let shown: Vec<&DeviceSummary> = st
                .devices
                .iter()
                .filter(|d| {
                    filter.is_empty()
                        || d.display_name.to_lowercase().contains(&filter)
                        || d.hostname.to_lowercase().contains(&filter)
                        || d.customer_name
                            .as_ref()
                            .is_some_and(|c| c.to_lowercase().contains(&filter))
                        || (!digits.is_empty() && d.friendly_id.compact().contains(&digits))
                })
                .collect();
            let models: Vec<_> = shown
                .iter()
                .map(|d| device_model(d, &self.local_host))
                .collect();
            let m = views::ComputersModel {
                devices: &models,
                total: st.devices.len(),
                online: st
                    .devices
                    .iter()
                    .filter(|d| d.presence == PresenceState::Online)
                    .count(),
                loaded: st.loaded,
                has_org: ws.org.is_some(),
                error: st.error.as_deref(),
                can_manage,
            };
            views::computers_page(ui, &m, &mut self.filter).and_then(|a| match a {
                views::ComputersAction::Connect(i) => shown.get(i).map(|d| {
                    Pick::Connect(
                        d.friendly_id,
                        device_name(d),
                        d.hostname.clone(),
                        unattended_ready(d),
                    )
                }),
                views::ComputersAction::Edit(i) => {
                    shown.get(i).map(|d| Pick::Edit(Box::new((*d).clone())))
                }
                views::ComputersAction::Organize => Some(Pick::Organize),
            })
        };
        match picked {
            Some(Pick::Connect(id, name, hostname, unattended)) => {
                self.connect_error = None;
                self.request_device(ui.ctx(), id, name, hostname, unattended);
            }
            Some(Pick::Edit(d)) => self.open_device_edit(ui.ctx(), &d),
            Some(Pick::Organize) => self.open_organize(ui.ctx()),
            None => {}
        }
    }

    /// Connects to a computer, asking for its unattended-access password first when the
    /// owner has allowed unattended access there.
    fn request_device(
        &mut self,
        ctx: &egui::Context,
        id: FriendlyId,
        name: String,
        hostname: String,
        unattended: bool,
    ) {
        if unattended {
            self.unattended = Some(UnattendedPrompt {
                id,
                name,
                hostname,
                password: String::new(),
            });
        } else {
            self.start_session(ctx, id, name, &hostname);
        }
    }

    /// The unattended-access password sheet, while it is open.
    pub fn unattended_sheet(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.unattended.as_mut() else {
            views::reset_unattended_sheet(ctx);
            return;
        };
        let m = views::UnattendedModel {
            name: &prompt.name,
            error: None,
        };
        let action = views::unattended_sheet(ctx, &m, &mut prompt.password);
        let Some(action) = action else { return };
        let Some(prompt) = self.unattended.take() else {
            return;
        };
        match action {
            views::UnattendedAction::Connect => {
                let password = Password::new(prompt.password);
                self.start_session_with(
                    ctx,
                    prompt.id,
                    prompt.name,
                    &prompt.hostname,
                    Some(password),
                );
            }
            views::UnattendedAction::AskPermission => {
                self.start_session(ctx, prompt.id, prompt.name, &prompt.hostname);
            }
            views::UnattendedAction::Cancel => {}
        }
    }

    /// The sign-in sheet, while it is open.
    pub fn login_sheet(&mut self, ctx: &egui::Context) {
        if self.ws.is_some() || !(self.show_login || self.connect_after_login.is_some()) {
            views::reset_login_sheet(ctx);
            return;
        }
        let (working, error) = match &*self.login.lock().unwrap() {
            LoginState::Working(s) => (Some(*s), None),
            LoginState::Failed(e) => (None, Some(e.clone())),
            LoginState::Idle => (None, None),
        };
        let m = views::LoginModel {
            working,
            error: error.as_deref(),
            then_connect: self.connect_after_login.map(|id| id.to_string()),
        };
        match views::login_sheet(ctx, &m, &mut self.email, &mut self.password) {
            Some(views::LoginAction::Submit) => self.begin_login(ctx),
            Some(views::LoginAction::Cancel) => {
                self.show_login = false;
                self.connect_after_login = None;
                *self.login.lock().unwrap() = LoginState::Idle;
            }
            None => {}
        }
    }

    /// Starts a session by ID; this computer's own ID is a local test (control off).
    fn connect_now(&mut self, ctx: &egui::Context, id: FriendlyId, name: String) {
        let host = if self.own_id == Some(id) {
            self.local_host.clone()
        } else {
            String::new()
        };
        self.start_session(ctx, id, name, &host);
    }

    fn begin_login(&mut self, ctx: &egui::Context) {
        let (server, dir, email, password) = (
            self.opts.server.clone(),
            self.opts.dir.clone(),
            self.email.clone(),
            self.password.clone(),
        );
        let (status, pending, ctx) = (self.login.clone(), self.pending_ws.clone(), ctx.clone());
        *status.lock().unwrap() = LoginState::Working("Entrando…");
        self.resume_cancel.store(true, Ordering::SeqCst);
        let change = self.pending_change.clone();
        self.rt.spawn(async move {
            let result = match login_with_password(&server, &email, &password).await {
                Ok(user) if user.profile().await.must_change_password => {
                    *change.lock().unwrap() = Some(user);
                    *status.lock().unwrap() = LoginState::Idle;
                    ctx.request_repaint();
                    return;
                }
                Ok(user) => open_workspace(server, dir, user, status.clone(), ctx.clone()).await,
                Err(e) => Err(e),
            };
            match result {
                Ok(ws) => *pending.lock().unwrap() = Some(ws),
                Err(e) => {
                    warn!(error = %e, "sign-in failed");
                    *status.lock().unwrap() = LoginState::Failed(e);
                }
            }
            ctx.request_repaint();
        });
    }

    /// Signs in silently with the stored login, if any. While the server cannot be
    /// reached (the app often starts with Windows before the network is up) it keeps
    /// trying in the background; the "Entrar" button stays available meanwhile.
    fn resume_saved(&mut self, ctx: &egui::Context) {
        let file = login_file(&self.opts.dir);
        let Some(token) = load_refresh(&file) else {
            return;
        };
        let (server, dir) = (self.opts.server.clone(), self.opts.dir.clone());
        let (status, pending, ctx) = (self.login.clone(), self.pending_ws.clone(), ctx.clone());
        let cancel = self.resume_cancel.clone();
        cancel.store(false, Ordering::SeqCst);
        *status.lock().unwrap() = LoginState::Working("Entrando…");
        self.rt.spawn(async move {
            let api = match ApiClient::new(&server) {
                Ok(a) => a,
                Err(e) => {
                    *status.lock().unwrap() = LoginState::Failed(e.to_string());
                    return;
                }
            };
            let mut wait = Duration::from_secs(3);
            let user = loop {
                match UserSession::resume(api.clone(), &token).await {
                    Ok(user) => break user,
                    Err(ClientError::Network(e)) => {
                        info!(error = %e, "stored login: server unreachable, retrying");
                        if cancel.load(Ordering::SeqCst) {
                            return;
                        }
                        if matches!(*status.lock().unwrap(), LoginState::Working(_)) {
                            *status.lock().unwrap() = LoginState::Idle;
                            ctx.request_repaint();
                        }
                        tokio::time::sleep(wait).await;
                        wait = (wait * 2).min(Duration::from_secs(60));
                        if cancel.load(Ordering::SeqCst) {
                            return;
                        }
                    }
                    Err(e) => {
                        // A revoked or expired login is gone for good.
                        if matches!(
                            e.code(),
                            Some(aionix_control_api::ErrorCode::Unauthorized)
                                | Some(aionix_control_api::ErrorCode::Forbidden)
                        ) {
                            forget_login(&file);
                        }
                        info!(error = %common::client_error_text(&e), "stored login not resumed");
                        *status.lock().unwrap() = LoginState::Idle;
                        ctx.request_repaint();
                        return;
                    }
                }
            };
            *status.lock().unwrap() = LoginState::Working("Entrando…");
            match open_workspace(server, dir, user, status.clone(), ctx.clone()).await {
                Ok(ws) if !cancel.load(Ordering::SeqCst) => *pending.lock().unwrap() = Some(ws),
                Ok(_) => {}
                Err(e) => {
                    info!(error = %e, "stored login not resumed");
                    *status.lock().unwrap() = LoginState::Idle;
                }
            }
            ctx.request_repaint();
        });
    }

    /// The administration panel's handle, when this account manages its organization.
    pub fn admin_handle(&self) -> Option<AdminHandle> {
        let ws = self.ws.as_ref()?;
        let m = ws
            .profile
            .memberships
            .iter()
            .filter(|m| m.role.can_manage())
            .max_by_key(|m| m.role)?;
        Some(AdminHandle {
            api: ws.api.clone(),
            user: ws.user.clone(),
            me: ws.profile.id,
            org: m.org_id,
            org_name: m.org_name.clone(),
            role: m.role,
        })
    }

    /// "Alterar senha" from the account row.
    pub fn open_password_change(&mut self) {
        if self.ws.is_some() {
            self.password_form = PasswordForm::default();
            *self.password_task.lock().unwrap() = PasswordTask::Idle;
            self.password_sheet = Some(PasswordSheet::Voluntary);
        }
    }

    /// The password sheet, while open (mandatory after a temporary password, or
    /// "Alterar senha"). Call every frame.
    pub fn password_sheet(&mut self, ctx: &egui::Context) {
        let Some(mode) = self.password_sheet else {
            views::reset_sheet(ctx, "password");
            return;
        };
        let (working, error, done) = match &*self.password_task.lock().unwrap() {
            PasswordTask::Working => (true, None, false),
            PasswordTask::Failed(e) => (false, Some(e.clone()), false),
            PasswordTask::Done => (false, None, true),
            PasswordTask::Idle => (false, None, false),
        };
        if done && mode == PasswordSheet::Voluntary {
            self.password_sheet = None;
            self.password_form = PasswordForm::default();
            return;
        }
        let mandatory = mode == PasswordSheet::Mandatory;
        match admin_views::password_sheet(
            ctx,
            &mut self.password_form,
            mandatory,
            working,
            error.as_deref(),
        ) {
            Some(SheetAction::Submit) => {
                *self.password_task.lock().unwrap() = PasswordTask::Working;
                let new = self.password_form.new.clone();
                let task = self.password_task.clone();
                let ctx2 = ctx.clone();
                if mandatory {
                    let Some(user) = self.pending_change.lock().unwrap().take() else {
                        return;
                    };
                    let current = self.change_current.clone();
                    let (server, dir) = (self.opts.server.clone(), self.opts.dir.clone());
                    let (status, pending, change) = (
                        self.login.clone(),
                        self.pending_ws.clone(),
                        self.pending_change.clone(),
                    );
                    self.rt.spawn(async move {
                        let changed = match user.access_token().await {
                            Ok(access) => user.api().change_password(&access, &current, &new).await,
                            Err(e) => Err(e),
                        };
                        match changed {
                            Ok(()) => {
                                *task.lock().unwrap() = PasswordTask::Done;
                                *status.lock().unwrap() = LoginState::Working("Entrando…");
                                match open_workspace(
                                    server,
                                    dir,
                                    user,
                                    status.clone(),
                                    ctx2.clone(),
                                )
                                .await
                                {
                                    Ok(ws) => *pending.lock().unwrap() = Some(ws),
                                    Err(e) => *status.lock().unwrap() = LoginState::Failed(e),
                                }
                            }
                            Err(e) => {
                                *task.lock().unwrap() = PasswordTask::Failed(password_error(&e));
                                *change.lock().unwrap() = Some(user);
                            }
                        }
                        ctx2.request_repaint();
                    });
                } else if let Some(ws) = self.ws.clone() {
                    let current = self.password_form.current.clone();
                    self.rt.spawn(async move {
                        let result = match ws.user.access_token().await {
                            Ok(access) => ws.api.change_password(&access, &current, &new).await,
                            Err(e) => Err(e),
                        };
                        *task.lock().unwrap() = match result {
                            Ok(()) => PasswordTask::Done,
                            Err(e) => PasswordTask::Failed(password_error(&e)),
                        };
                        ctx2.request_repaint();
                    });
                }
            }
            Some(SheetAction::Cancel) => {
                self.password_sheet = None;
                self.password_form = PasswordForm::default();
                if mandatory {
                    // "Sair": the login with the temporary password is dropped.
                    if let Some(user) = self.pending_change.lock().unwrap().take() {
                        self.rt.spawn(async move {
                            let _ = user.logout().await;
                        });
                    }
                    self.change_current.clear();
                }
            }
            None => {}
        }
    }
}

impl TechSide {
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

        // ---- toolbar (its own strip: clicks there never reach the remote computer)
        let fullscreen = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        let title = s.host_name.clone().unwrap_or_else(|| s.name.clone());
        let id_text = s.target.to_string();
        let metrics = live
            .then(|| {
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
                parts.join("  ·  ")
            })
            .filter(|m| !m.is_empty());
        let tone = session_tone(status.state);
        egui::Panel::top("session-bar")
            .exact_size(views::TOOLBAR_HEIGHT)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let m = views::ToolbarModel {
                    title: &title,
                    id: &id_text,
                    state: common::state_text(status.state),
                    tone,
                    pulse: !terminal && !matches!(status.state, SessionState::Connected),
                    metrics,
                    terminal,
                    can_control: can_control && live,
                    fullscreen,
                };
                let before = s.control;
                match views::session_toolbar(ui, &m, &mut s.control, &mut s.show_diag) {
                    Some(views::ToolbarAction::Leave) => leave = true,
                    Some(views::ToolbarAction::End) => s.handle.end(),
                    Some(views::ToolbarAction::Retry) => retry = true,
                    Some(views::ToolbarAction::Fullscreen) => ui
                        .ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen)),
                    Some(views::ToolbarAction::ControlOff) | None => {}
                }
                if before && !s.control {
                    release_all(s);
                }
            });

        if s.local_machine && !terminal {
            egui::Panel::top("local-notice")
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    views::session_notice(
                        ui,
                        "Teste local: este é o próprio computador. O controle começa desligado para o cursor não se perseguir; ligue-o pelo botão do ponteiro se quiser testar.",
                    );
                });
        }

        if s.show_diag {
            egui::Panel::right("diag")
                .exact_size(300.0)
                .frame(
                    egui::Frame::new()
                        .fill(p.sidebar)
                        .stroke(Stroke::new(1.0, p.hairline))
                        .inner_margin(egui::Margin::same(18)),
                )
                .show(ui, |ui| diagnostics_panel(ui, s, &diag, &status));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(if live {
                Color32::from_rgb(11, 11, 12)
            } else {
                p.bg
            }))
            .show(ui, |ui| {
                let area = ui.available_rect_before_wrap();
                match (
                    &s.texture,
                    live || status.state == SessionState::Reconnecting,
                ) {
                    (Some(tex), true) => {
                        let img_rect = fit_rect(area, tex.size_vec2());
                        ui.painter().image(
                            tex.id(),
                            img_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                        self.viewer_input(ui, s, img_rect, can_control && live);
                        draw_cursor(ui, s, img_rect);
                        if status.state == SessionState::Reconnecting {
                            views::session_overlay(
                                ui,
                                area,
                                &views::OverlayModel {
                                    headline: "Reconectando…",
                                    detail: "A conexão caiu. Tentando retomar a sessão.",
                                    busy: true,
                                    tone: views::Tone::Warning,
                                },
                            );
                        }
                    }
                    _ => {
                        let (headline, detail, spin) = state_message(&status, s);
                        views::session_overlay(
                            ui,
                            area,
                            &views::OverlayModel {
                                headline: &headline,
                                detail: &detail,
                                busy: spin,
                                tone,
                            },
                        );
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
            s.input.local_pointer = None;
            if s.input.focused {
                release_all(s);
            }
            return;
        }
        s.input.local_pointer = if resp.hovered() || resp.is_pointer_button_down_on() {
            ui.input(|i| i.pointer.hover_pos())
                .map(|p| p.clamp(img.min, img.max))
        } else {
            None
        };
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
        // Hide the OS pointer only when the remote cursor can be drawn in its place.
        let have_shape = s
            .cursor
            .as_ref()
            .is_some_and(|c| s.cursors.contains_key(&c.shape_id));
        if have_shape && (resp.hovered() || resp.is_pointer_button_down_on()) {
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
    let pos = s.input.local_pointer.unwrap_or_else(|| {
        Pos2::new(
            img.min.x + c.x as f32 * scale,
            img.min.y + c.y as f32 * (img.height() / sh),
        )
    });
    let size = shape.size * scale.clamp(0.6, 1.5);
    let min = pos - shape.hotspot * (size.x / shape.size.x);
    ui.painter().image(
        shape.tex.id(),
        Rect::from_min_size(min, size),
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
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

fn password_error(e: &ClientError) -> String {
    match e.code() {
        Some(aionix_control_api::ErrorCode::InvalidCredentials) => {
            "A senha atual está incorreta.".into()
        }
        Some(aionix_control_api::ErrorCode::BadRequest) => {
            "A nova senha precisa ter pelo menos 12 caracteres.".into()
        }
        _ => common::client_error_text(e),
    }
}

/// Name shown for a computer: its display name, or the hostname.
fn device_name(d: &DeviceSummary) -> String {
    if d.display_name.is_empty() {
        d.hostname.clone()
    } else {
        d.display_name.clone()
    }
}

/// Whether the owner allows unattended access right now and a password is set, so the
/// technician can connect without the person at the computer clicking "Permitir".
fn unattended_ready(d: &DeviceSummary) -> bool {
    d.unattended_enabled && d.unattended_password_set
}

fn device_model(d: &DeviceSummary, local_host: &str) -> views::DeviceModel {
    views::DeviceModel {
        id: d.friendly_id.to_string(),
        name: device_name(d),
        system: d.os_name.clone(),
        customer: d.customer_name.clone().filter(|c| !c.is_empty()),
        presence: match d.presence {
            PresenceState::Online => views::Presence::Online,
            PresenceState::Connecting => views::Presence::Connecting,
            PresenceState::Offline => views::Presence::Offline,
        },
        this_computer: d.hostname.eq_ignore_ascii_case(local_host),
        seen: d.last_seen_at.map(seen_text),
    }
}

/// "visto há 3 h" from the last time the server heard from a computer.
fn seen_text(at: time::OffsetDateTime) -> String {
    let secs = (time::OffsetDateTime::now_utc() - at)
        .whole_seconds()
        .max(0);
    match secs {
        0..60 => "visto agora".into(),
        60..3_600 => format!("visto há {} min", secs / 60),
        3_600..86_400 => format!("visto há {} h", secs / 3_600),
        86_400..172_800 => "visto ontem".into(),
        172_800..2_592_000 => format!("visto há {} dias", secs / 86_400),
        _ => "visto há mais de um mês".into(),
    }
}

fn transport_text(t: TransportKind) -> &'static str {
    match t {
        TransportKind::Lan => "Rede local",
        TransportKind::Direct => "Direto",
        TransportKind::Relay => "Via relay",
        TransportKind::Unknown => "Determinando",
    }
}

fn session_tone(s: SessionState) -> views::Tone {
    match s {
        SessionState::Connected => views::Tone::Success,
        SessionState::Degraded | SessionState::Reconnecting | SessionState::AwaitingApproval => {
            views::Tone::Warning
        }
        SessionState::Failed | SessionState::Rejected => views::Tone::Danger,
        SessionState::Ended | SessionState::Ending => views::Tone::Neutral,
        _ => views::Tone::Accent,
    }
}

// ---------------------------------------------- organize computers (ADR-0015)

impl TechSide {
    /// Whether this account may organize computers and set their unattended password.
    fn can_manage(&self) -> bool {
        let (Some(ws), Some((org, _))) = (
            self.ws.as_ref(),
            self.ws.as_ref().and_then(|w| w.org.clone()),
        ) else {
            return false;
        };
        ws.profile
            .memberships
            .iter()
            .any(|m| m.org_id == org && matches!(m.role, OrgRole::Admin | OrgRole::Owner))
    }

    /// Loads customers, groups and technicians for the management sheets.
    fn refresh_catalog(&self, ctx: &egui::Context) {
        let Some(ws) = self.ws.clone() else { return };
        let Some((org, _)) = ws.org.clone() else {
            return;
        };
        let catalog = self.catalog.clone();
        let ctx = ctx.clone();
        self.rt.spawn(async move {
            let loaded = load_catalog(&ws, org).await;
            store_catalog(&catalog, loaded);
            ctx.request_repaint();
        });
    }

    /// "Editar computador" for one computer, starting from its current values.
    fn open_device_edit(&mut self, ctx: &egui::Context, d: &DeviceSummary) {
        self.device_edit = Some(DeviceEdit {
            device: d.id,
            id_text: common::friendly(d.friendly_id),
            fields: views::DeviceFields {
                name: d.display_name.clone(),
                description: d.description.clone(),
                customer: d.customer_id,
                group: d.group_id,
                assignee: d.assigned_user_id,
                favorite: d.favorite,
                password: String::new(),
            },
            work: Arc::new(Mutex::new(Work::Idle)),
        });
        self.refresh_catalog(ctx);
        ctx.request_repaint();
    }

    fn open_organize(&mut self, ctx: &egui::Context) {
        self.organize_open = true;
        *self.organize_work.lock().unwrap() = Work::Idle;
        self.refresh_catalog(ctx);
    }

    /// The management sheets: "Clientes e grupos" when open, otherwise "Editar computador".
    pub fn management_sheets(&mut self, ctx: &egui::Context) {
        if self.organize_open {
            self.organize_sheet(ctx);
            return;
        }
        self.device_sheet(ctx);
    }

    fn device_sheet(&mut self, ctx: &egui::Context) {
        let Some(ws) = self.ws.clone() else { return };
        let Some(edit) = self.device_edit.as_mut() else {
            views::reset_device_edit_sheet(ctx);
            return;
        };
        // Consent and password status come from the live device list.
        let live = ws
            .devices
            .lock()
            .unwrap()
            .devices
            .iter()
            .find(|d| d.id == edit.device)
            .cloned();
        let Some(live) = live else {
            self.device_edit = None;
            views::reset_device_edit_sheet(ctx);
            return;
        };
        let (customers, groups, members) = {
            let c = self.catalog.lock().unwrap();
            (
                c.customers
                    .iter()
                    .map(|c| (c.id, c.name.clone()))
                    .collect::<Vec<_>>(),
                c.groups
                    .iter()
                    .map(|g| (g.id, g.name.clone()))
                    .collect::<Vec<_>>(),
                c.members
                    .iter()
                    .filter(|m| !m.disabled)
                    .map(|m| (m.user_id, m.display_name.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        let work = edit.work.lock().unwrap().clone();
        let (working, error, notice) = work.texts();
        let m = views::DeviceEditModel {
            id_text: &edit.id_text,
            customers: &customers,
            groups: &groups,
            members: &members,
            password_set: live.unattended_password_set,
            unattended_enabled: live.unattended_enabled,
            working,
            error,
            notice,
        };
        let action = views::device_edit_sheet(ctx, &m, &mut edit.fields);
        match action {
            Some(views::DeviceEditAction::Save) => self.save_device(ctx),
            Some(views::DeviceEditAction::SetPassword) => self.device_password(ctx, true),
            Some(views::DeviceEditAction::ClearPassword) => self.device_password(ctx, false),
            Some(views::DeviceEditAction::Organize) => self.organize_open = true,
            Some(views::DeviceEditAction::Close) => {
                self.device_edit = None;
                views::reset_device_edit_sheet(ctx);
            }
            None => {}
        }
    }

    fn save_device(&mut self, ctx: &egui::Context) {
        let (Some(ws), Some(edit)) = (self.ws.clone(), self.device_edit.as_ref()) else {
            return;
        };
        let Some((org, _)) = ws.org.clone() else {
            return;
        };
        let req = UpdateDeviceRequest {
            display_name: Some(edit.fields.name.trim().to_owned()),
            description: Some(edit.fields.description.trim().to_owned()),
            customer_id: Some(edit.fields.customer),
            group_id: Some(edit.fields.group),
            assigned_user_id: Some(edit.fields.assignee),
            favorite: Some(edit.fields.favorite),
        };
        let (device, work) = (edit.device, edit.work.clone());
        *work.lock().unwrap() = Work::Working("Salvando…");
        let ctx = ctx.clone();
        self.rt.spawn(async move {
            let result = match ws.user.access_token().await {
                Ok(access) => ws.api.update_device(&access, org, device, &req).await,
                Err(e) => Err(e),
            };
            *work.lock().unwrap() = match result {
                Ok(summary) => {
                    replace_device(&ws, summary);
                    Work::Done("Alterações salvas.")
                }
                Err(e) => Work::Failed(common::client_error_text(&e)),
            };
            ctx.request_repaint();
        });
    }

    /// Sets (`set`) or removes the unattended-access password of the open computer.
    fn device_password(&mut self, ctx: &egui::Context, set: bool) {
        let (Some(ws), Some(edit)) = (self.ws.clone(), self.device_edit.as_mut()) else {
            return;
        };
        let Some((org, _)) = ws.org.clone() else {
            return;
        };
        let password = std::mem::take(&mut edit.fields.password);
        let (device, work) = (edit.device, edit.work.clone());
        *work.lock().unwrap() = Work::Working(if set {
            "Definindo senha…"
        } else {
            "Removendo senha…"
        });
        let ctx = ctx.clone();
        self.rt.spawn(async move {
            let result = match ws.user.access_token().await {
                Ok(access) if set => {
                    ws.api
                        .set_unattended_password(&access, org, device, &password)
                        .await
                }
                Ok(access) => ws.api.clear_unattended_password(&access, org, device).await,
                Err(e) => Err(e),
            };
            *work.lock().unwrap() = match result {
                Ok(()) => {
                    set_password_flag(&ws, device, set);
                    Work::Done(if set {
                        "Senha definida. Ela vale enquanto o dono permitir o acesso."
                    } else {
                        "Senha removida. O acesso sem aceitação foi desligado."
                    })
                }
                Err(e) => Work::Failed(common::client_error_text(&e)),
            };
            ctx.request_repaint();
        });
    }

    /// "Clientes e grupos": create and delete customers and groups.
    fn organize_sheet(&mut self, ctx: &egui::Context) {
        let Some(ws) = self.ws.clone() else { return };
        let (customers, groups) = {
            let c = self.catalog.lock().unwrap();
            (
                c.customers
                    .iter()
                    .map(|c| (c.id, c.name.clone(), c.device_count))
                    .collect::<Vec<_>>(),
                c.groups
                    .iter()
                    .map(|g| (g.id, g.name.clone(), g.device_count))
                    .collect::<Vec<_>>(),
            )
        };
        let work = self.organize_work.lock().unwrap().clone();
        let (working, error, _) = work.texts();
        let m = views::OrganizeModel {
            customers: &customers,
            groups: &groups,
            working,
            error,
        };
        let action = views::organize_sheet(ctx, &m, &mut self.new_customer, &mut self.new_group);
        let Some(action) = action else { return };
        let op = match action {
            views::OrganizeAction::AddCustomer => {
                let name = std::mem::take(&mut self.new_customer).trim().to_owned();
                if name.is_empty() {
                    return;
                }
                OrgOp::CreateCustomer(name)
            }
            views::OrganizeAction::AddGroup => {
                let name = std::mem::take(&mut self.new_group).trim().to_owned();
                if name.is_empty() {
                    return;
                }
                OrgOp::CreateGroup(name)
            }
            views::OrganizeAction::DeleteCustomer(id) => OrgOp::DeleteCustomer(id),
            views::OrganizeAction::DeleteGroup(id) => OrgOp::DeleteGroup(id),
            views::OrganizeAction::Close => {
                self.organize_open = false;
                views::reset_organize_sheet(ctx);
                return;
            }
        };
        self.organize_task(ctx, ws, op);
    }

    /// Runs one customer/group change, then reloads the catalog and the computer list.
    fn organize_task(&self, ctx: &egui::Context, ws: Arc<Workspace>, op: OrgOp) {
        let Some((org, _)) = ws.org.clone() else {
            return;
        };
        let (work, catalog) = (self.organize_work.clone(), self.catalog.clone());
        *work.lock().unwrap() = Work::Working(op.label());
        let ctx = ctx.clone();
        self.rt.spawn(async move {
            let result = run_org_op(&ws, org, &op).await;
            *work.lock().unwrap() = match result {
                Ok(done) => {
                    op.unlink_devices(&ws);
                    Work::Done(done)
                }
                Err(e) => Work::Failed(common::client_error_text(&e)),
            };
            store_catalog(&catalog, load_catalog(&ws, org).await);
            ctx.request_repaint();
        });
    }
}

/// A customer or group change from the "Clientes e grupos" sheet.
enum OrgOp {
    CreateCustomer(String),
    CreateGroup(String),
    DeleteCustomer(Uuid),
    DeleteGroup(Uuid),
}

impl OrgOp {
    fn label(&self) -> &'static str {
        match self {
            Self::CreateCustomer(_) => "Cadastrando cliente…",
            Self::CreateGroup(_) => "Criando grupo…",
            Self::DeleteCustomer(_) => "Excluindo cliente…",
            Self::DeleteGroup(_) => "Excluindo grupo…",
        }
    }

    /// After a delete, the computers of that customer or group are shown without it.
    fn unlink_devices(&self, ws: &Workspace) {
        let mut st = ws.devices.lock().unwrap();
        for d in st.devices.iter_mut() {
            match self {
                Self::DeleteCustomer(id) if d.customer_id == Some(*id) => {
                    d.customer_id = None;
                    d.customer_name = None;
                }
                Self::DeleteGroup(id) if d.group_id == Some(*id) => {
                    d.group_id = None;
                    d.group_name = None;
                }
                _ => {}
            }
        }
    }
}

async fn run_org_op(ws: &Workspace, org: Uuid, op: &OrgOp) -> Result<&'static str, ClientError> {
    let access = ws.user.access_token().await?;
    match op {
        OrgOp::CreateCustomer(name) => {
            ws.api.create_customer(&access, org, name).await?;
            Ok("Cliente cadastrado.")
        }
        OrgOp::CreateGroup(name) => {
            ws.api.create_group(&access, org, name).await?;
            Ok("Grupo criado.")
        }
        OrgOp::DeleteCustomer(id) => {
            ws.api.delete_customer(&access, org, *id).await?;
            Ok("Cliente excluído. Os computadores continuam cadastrados.")
        }
        OrgOp::DeleteGroup(id) => {
            ws.api.delete_group(&access, org, *id).await?;
            Ok("Grupo excluído. Os computadores continuam cadastrados.")
        }
    }
}

/// Organization data for the management sheets, loaded from the Control Plane.
#[derive(Default)]
struct Catalog {
    customers: Vec<Customer>,
    groups: Vec<DeviceGroup>,
    members: Vec<Member>,
    error: Option<String>,
}

/// The last change requested from an open sheet.
#[derive(Default, Clone)]
enum Work {
    #[default]
    Idle,
    Working(&'static str),
    Failed(String),
    Done(&'static str),
}

impl Work {
    /// (working line, error, notice) for the sheet's footer.
    fn texts(&self) -> (Option<&str>, Option<&str>, Option<&str>) {
        match self {
            Self::Idle => (None, None, None),
            Self::Working(s) => (Some(s), None, None),
            Self::Failed(e) => (None, Some(e.as_str()), None),
            Self::Done(s) => (None, None, Some(s)),
        }
    }
}

/// "Editar computador" for one computer.
struct DeviceEdit {
    device: Uuid,
    id_text: String,
    fields: views::DeviceFields,
    work: Arc<Mutex<Work>>,
}

/// Customers, groups and technicians of an organization.
type Loaded = (Vec<Customer>, Vec<DeviceGroup>, Vec<Member>);

async fn load_catalog(ws: &Workspace, org: Uuid) -> Result<Loaded, ClientError> {
    let access = ws.user.access_token().await?;
    let customers = ws.api.customers(&access, org).await?.customers;
    let groups = ws.api.groups(&access, org).await?.groups;
    let members = ws.api.org_members(&access, org).await?.members;
    Ok((customers, groups, members))
}

fn store_catalog(catalog: &Mutex<Catalog>, loaded: Result<Loaded, ClientError>) {
    let mut c = catalog.lock().unwrap();
    match loaded {
        Ok((customers, groups, members)) => {
            c.customers = customers;
            c.groups = groups;
            c.members = members;
            c.error = None;
        }
        Err(e) => c.error = Some(common::client_error_text(&e)),
    }
}

/// Puts a computer returned by the server in the list, in place of the old one.
fn replace_device(ws: &Workspace, summary: DeviceSummary) {
    let mut st = ws.devices.lock().unwrap();
    if let Some(d) = st.devices.iter_mut().find(|d| d.id == summary.id) {
        *d = summary;
    }
}

fn set_password_flag(ws: &Workspace, device: Uuid, set: bool) {
    let mut st = ws.devices.lock().unwrap();
    if let Some(d) = st.devices.iter_mut().find(|d| d.id == device) {
        d.unattended_password_set = set;
        if !set {
            d.unattended_enabled = false;
        }
    }
}

/// What a click on the computers list asks for.
enum Pick {
    Connect(FriendlyId, String, String, bool),
    Edit(Box<DeviceSummary>),
    Organize,
}
