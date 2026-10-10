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
        common,
        host_side::{HostOptions, HostSide},
        icon,
        tech_side::{TechOptions, TechSide},
        theme, updater,
    };
    use aionix_shared::FriendlyId;
    use clap::Parser;
    use egui::{Align, Layout, RichText, Stroke};
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
                .with_min_inner_size([760.0, 560.0])
                .with_icon(icon::brand_icon(64, [0x5e, 0x8c, 0xff])),
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
    }

    /// A verified update installs itself only after this long without the user touching
    /// the app, and never during a session (either direction) or a consent prompt.
    const IDLE_BEFORE_UPDATE: Duration = Duration::from_secs(120);

    impl eframe::App for App {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
                self.last_input = Instant::now();
            }
            self.tech.set_own_id(self.host.friendly());
            self.tech.poll(&ctx);
            if self.tech.in_session() {
                self.tech.session_view(ui);
            } else {
                self.home(ui);
            }
            self.apply_update_when_idle(&ctx);
            ctx.request_repaint_after(Duration::from_millis(500));
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
                || self.last_input.elapsed() < IDLE_BEFORE_UPDATE
            {
                return;
            }
            self.applying_update = true;
            match self.updater.apply() {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => warn!(error = %e, "update not applied"),
            }
        }

        fn home(&mut self, ui: &mut egui::Ui) {
            let p = theme::palette(ui);
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
                        ui.label(theme::title(WINDOW_TITLE, 16.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            self.tech.account_chip(ui);
                        });
                    });
                });
            if let Some(label) = &self.label {
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
            egui::Panel::bottom("footer")
                .frame(
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, p.border))
                        .inner_margin(egui::Margin::symmetric(20, 8)),
                )
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        updater::footer(ui, &self.updater, &p);
                        if let Some(m) = self.updater.ready() {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let text = format!("Reiniciar e atualizar para {}", m.version);
                                if theme::primary_button(ui, &text, !self.host.busy(), None)
                                    .clicked()
                                {
                                    match self.updater.apply() {
                                        Ok(()) => {
                                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close)
                                        }
                                        Err(e) => warn!(error = %e, "update not applied"),
                                    }
                                }
                            });
                        }
                    });
                });
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(p.bg)
                        .inner_margin(egui::Margin::same(24)),
                )
                .show(ui, |ui| {
                    let wide = ui.available_width() >= 860.0;
                    if wide {
                        ui.columns(2, |cols| {
                            cols[0].vertical(|ui| {
                                self.host.card(ui);
                                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                                    self.host.details(ui);
                                });
                            });
                            cols[1].vertical(|ui| self.tech.connect_panel(ui));
                        });
                    } else {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            self.host.card(ui);
                            ui.add_space(16.0);
                            self.tech.connect_panel(ui);
                            ui.add_space(12.0);
                            self.host.details(ui);
                        });
                    }
                });
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
