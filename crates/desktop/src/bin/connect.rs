//! Aionix Connect — one app for both sides of remote support.
//!
//! "Este computador" (left): this computer's 9-digit ID, always reachable; nobody gets
//! in without clicking Permitir in the consent window. "Acessar outro computador"
//! (right): connect by ID after signing in once. An outbound session takes the whole
//! window. One instance per user; a second launch brings the existing window forward.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("aionix-connect runs on Windows only");
}

#[cfg(windows)]
fn main() {
    app::main();
}

#[cfg(windows)]
mod app {
    use std::{path::PathBuf, time::Duration, time::Instant};

    use aionix_desktop::{
        admin_side::AdminSide,
        common,
        host_side::{HostOptions, HostSide},
        icon,
        tech_side::{TechOptions, TechSide},
        theme, updater, views,
    };
    use aionix_shared::FriendlyId;
    use clap::Parser;

    use tracing::{error, info, warn};

    #[derive(Parser, Debug, Clone)]
    #[command(name = "aionix-connect", about = "Aionix Connect — suporte remoto")]
    struct Args {
        /// Server URL (API, signaling and relay behind one address). Default: the
        /// server compiled into this build, or `server` in aionix-host.json next to the
        /// executable.
        #[arg(long, env = "AIONIX_SERVER")]
        server: Option<String>,
        /// Signaling WebSocket URL when it is not served by the API address.
        #[arg(long, env = "AIONIX_SIGNAL_URL")]
        signal_url: Option<String>,
        /// Root data folder (default `%LOCALAPPDATA%\Aionix Connect`); the receiving
        /// identity lives in `host\`, the signed-in user's in `technician\`.
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Organization enrollment token (first start only): this computer enrolls as
        /// a managed agent instead of Quick Support.
        #[arg(long, env = "AIONIX_ENROLLMENT_TOKEN")]
        enrollment_token: Option<String>,
        /// Banner text identifying a local validation build.
        #[arg(long)]
        environment_label: Option<String>,
        /// Seconds the consent prompt waits before refusing on its own.
        #[arg(long, default_value_t = 60)]
        approval_timeout: u64,
        /// Pre-filled e-mail (development launcher).
        #[arg(long, env = "AIONIX_DEV_EMAIL")]
        email: Option<String>,
        /// Sign in at start with the pre-filled e-mail and AIONIX_DEV_PASSWORD.
        #[arg(long)]
        auto_login: bool,
        /// Development/validation: request a session to this ID right after signing in.
        #[arg(long, hide = true)]
        auto_connect: Option<FriendlyId>,
        /// Allow a second instance (development launcher running two data folders).
        #[arg(long, hide = true)]
        multi_instance: bool,
        /// Start minimized to the taskbar (the Windows startup entry uses this).
        #[arg(long)]
        minimized: bool,
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

    const WINDOW_TITLE: &str = "Aionix Connect";

    pub fn main() {
        let mut args = Args::parse();
        let file = file_config();
        let server = args
            .server
            .clone()
            .or(file.server)
            .unwrap_or_else(|| common::DEFAULT_SERVER.into());
        if args.signal_url.is_none() {
            args.signal_url = file.signal_url;
        }
        if args.environment_label.is_none() {
            args.environment_label = file.environment_label;
        }
        // One instance per user: two copies would fight over the same identity.
        let _instance = if args.multi_instance {
            None
        } else {
            match single_instance::acquire() {
                Some(guard) => Some(guard),
                None => {
                    single_instance::focus_existing(WINDOW_TITLE);
                    return;
                }
            }
        };

        aionix_platform::windows::init_dpi_awareness();
        let root = args
            .data_dir
            .clone()
            .unwrap_or_else(|| common::data_dir("", None));
        let log_path = common::init_logging(&root, "aionix-connect");
        info!(server = %server, version = common::VERSION, "Aionix Connect starting");
        let password = std::env::var("AIONIX_DEV_PASSWORD").unwrap_or_default();
        // SAFETY: removing our own process variable before any other thread reads it.
        unsafe { std::env::remove_var("AIONIX_DEV_PASSWORD") };

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(WINDOW_TITLE)
                .with_inner_size([1040.0, 680.0])
                // Started with Windows: hidden until the first frame minimizes it.
                .with_visible(!args.minimized)
                .with_min_inner_size([760.0, 560.0])
                .with_icon(icon::app_icon()),
            ..Default::default()
        };
        let handle = rt.handle().clone();
        let result = eframe::run_native(
            "aionix-connect",
            options,
            Box::new(move |cc| {
                theme::install(&cc.egui_ctx);
                let host = HostSide::start(
                    &handle,
                    HostOptions {
                        server: server.clone(),
                        signal_url: args.signal_url.clone(),
                        dir: root.join("host"),
                        enrollment_token: args.enrollment_token.clone(),
                        approval_timeout: args.approval_timeout,
                    },
                    cc.egui_ctx.clone(),
                    log_path,
                );
                let tech = TechSide::new(
                    &handle,
                    TechOptions {
                        server: server.clone(),
                        dir: root.join("technician"),
                        email: args.email.clone(),
                        password,
                        auto_login: args.auto_login,
                        auto_connect: args.auto_connect,
                    },
                    &cc.egui_ctx,
                );
                let updater = updater::Updater::start(
                    &handle,
                    aionix_update::Product::App,
                    cc.egui_ctx.clone(),
                );
                Ok(Box::new(App {
                    host,
                    tech,
                    updater,
                    label: args.environment_label.clone(),
                    last_input: Instant::now(),
                    applying_update: false,
                    page: views::Page::Home,
                    computers_after_login: false,
                    start_minimized: if args.minimized { MINIMIZE_ATTEMPTS } else { 0 },
                    admin: AdminSide::new(&handle),
                }))
            }),
        );
        if let Err(e) = result {
            error!(error = %e, "window failed");
        }
        rt.shutdown_timeout(Duration::from_secs(3));
    }

    struct App {
        host: HostSide,
        tech: TechSide,
        updater: updater::Updater,
        label: Option<String>,
        last_input: Instant,
        applying_update: bool,
        page: views::Page,
        /// "Computadores" was clicked while signed out: open it after signing in.
        computers_after_login: bool,
        /// Attempts left in the minimized start (0: done).
        start_minimized: u8,
        admin: AdminSide,
    }

    /// Frames (~50 ms apart) spent getting a `--minimized` start minimized.
    const MINIMIZE_ATTEMPTS: u8 = 60;

    /// A verified update installs itself only after this long without the user touching
    /// the app, and never during a session (either direction) or a consent prompt.
    const IDLE_BEFORE_UPDATE: Duration = Duration::from_secs(120);

    impl eframe::App for App {
        /// Runs before every frame and, unlike `ui`, also while the window is minimized
        /// (how the app starts with Windows; eframe then skips `ui`): work that must not
        /// wait for the window to be shown.
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            // Started with Windows: show on the first frame, then ask to minimize until the
            // window reports it is minimized (Windows ignores minimizing a window that is
            // not shown yet, and how soon it is shown varies). Gives up after ~3 s. Here
            // and not in `ui`, which never sees the minimized window: the attempts would
            // stay pending and minimize the window again when the user opens it.
            if self.start_minimized > 0 {
                let minimized = ctx.input(|i| i.viewport().minimized) == Some(true);
                if minimized {
                    self.start_minimized = 0;
                } else {
                    if self.start_minimized == MINIMIZE_ATTEMPTS {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    self.start_minimized -= 1;
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
            }
            self.tech.set_own_id(self.host.friendly());
            self.tech.poll(ctx);
            self.apply_update_when_idle(ctx);
            ctx.request_repaint_after(Duration::from_millis(500));
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
                self.last_input = Instant::now();
            }
            if self.tech.in_session() {
                self.tech.session_view(ui);
            } else {
                self.home(ui);
            }
        }

        fn on_exit(&mut self) {
            self.tech.shutdown();
            self.host.shutdown();
        }
    }

    impl App {
        fn apply_update_when_idle(&mut self, ctx: &egui::Context) {
            if self.applying_update
                || self.updater.ready().is_none()
                || self.host.busy()
                || self.tech.busy()
                || self.admin.busy()
                || self.last_input.elapsed() < IDLE_BEFORE_UPDATE
            {
                return;
            }
            self.applying_update = true;
            let minimized = ctx.input(|i| i.viewport().minimized) == Some(true);
            match self.updater.apply(minimized) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => warn!(error = %e, "update not applied"),
            }
        }

        fn home(&mut self, ui: &mut egui::Ui) {
            let ctx = ui.ctx().clone();
            let account = self.tech.account();
            let admin = self.tech.admin_handle();
            if account.is_none() || (self.page == views::Page::Admin && admin.is_none()) {
                self.page = views::Page::Home;
            } else if std::mem::take(&mut self.computers_after_login) {
                self.page = views::Page::Computers;
            }
            let phase = self.updater.phase();
            let version_text = match &phase {
                updater::Phase::Downloading { version, .. } => version.to_string(),
                updater::Phase::Ready { manifest, .. } => manifest.version.to_string(),
                _ => String::new(),
            };
            let can_restart = !self.host.busy() && !self.tech.in_session();
            let update = match &phase {
                updater::Phase::Disabled => views::UpdateModel::Disabled,
                updater::Phase::Idle => views::UpdateModel::Idle,
                updater::Phase::Checking => views::UpdateModel::Checking,
                updater::Phase::UpToDate => views::UpdateModel::UpToDate,
                updater::Phase::Downloading { fraction, .. } => views::UpdateModel::Downloading {
                    version: &version_text,
                    fraction: *fraction,
                },
                updater::Phase::Ready { .. } => views::UpdateModel::Ready {
                    version: &version_text,
                    can_restart,
                },
                updater::Phase::Failed(e) => views::UpdateModel::Failed { error: e },
            };
            let side = views::SidebarModel {
                page: self.page,
                account: account
                    .as_ref()
                    .map(|(name, org)| views::AccountModel { name, org }),
                signing_in: self.tech.signing_in(),
                online: self.tech.online_count(),
                env_label: self.label.as_deref(),
                version: common::VERSION,
                update,
                admin: admin.is_some(),
            };
            let host_model = self.host.model();
            let mut host_action = None;
            let mut go = None;
            let page = self.page;
            let (tech, host, admin_side) = (&mut self.tech, &self.host, &mut self.admin);
            let action = views::shell(ui, &side, |ui| match page {
                views::Page::Home => {
                    host_action = views::home_page(
                        ui,
                        &host_model,
                        |ui| go = tech.connect_section(ui),
                        |ui| host.details(ui),
                    );
                }
                views::Page::Computers => tech.computers_page(ui),
                views::Page::Admin => {
                    if let Some(h) = &admin {
                        admin_side.page(ui, h);
                    }
                }
            });
            match action {
                Some(views::SidebarAction::Go(views::Page::Computers)) if account.is_none() => {
                    self.computers_after_login = true;
                    self.tech.open_login();
                }
                Some(views::SidebarAction::Go(p)) => self.page = p,
                Some(views::SidebarAction::SignIn) => self.tech.open_login(),
                Some(views::SidebarAction::SignOut) => {
                    self.tech.logout();
                    self.admin.clear();
                    self.page = views::Page::Home;
                }
                Some(views::SidebarAction::ChangePassword) => self.tech.open_password_change(),
                Some(views::SidebarAction::CheckUpdates) => self.updater.check_now(),
                Some(views::SidebarAction::RestartToUpdate) => match self.updater.apply(false) {
                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(e) => warn!(error = %e, "update not applied"),
                },
                None => {}
            }
            if let Some(p) = go {
                self.page = p;
            }
            if let Some(a) = host_action {
                self.host.act(&ctx, a);
            }
            if !self.tech.login_open() {
                self.computers_after_login = false;
            }
            self.tech.login_sheet(&ctx);
            self.tech.password_sheet(&ctx);
            if let Some(h) = &admin {
                self.admin.sheets(&ctx, h);
            }
        }
    }

    /// Named mutex per user session; the window of a running copy is brought forward.
    mod single_instance {
        use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
        };
        use windows::core::{HSTRING, PCWSTR};

        pub struct Guard(HANDLE);
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }

        pub fn acquire() -> Option<Guard> {
            unsafe {
                let h =
                    CreateMutexW(None, true, &HSTRING::from("Local\\AionixConnect.App")).ok()?;
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    let _ = CloseHandle(h);
                    return None;
                }
                Some(Guard(h))
            }
        }

        pub fn focus_existing(title: &str) {
            unsafe {
                if let Ok(w) = FindWindowW(PCWSTR::null(), &HSTRING::from(title)) {
                    if IsIconic(w).as_bool() {
                        let _ = ShowWindow(w, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(w);
                }
            }
        }
    }
}
