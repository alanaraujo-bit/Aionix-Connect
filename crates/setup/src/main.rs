//! Aionix Connect setup: installer, silent updater and uninstaller in one executable.
//!
//!   aionix-setup.exe                         themed installer
//!   aionix-setup.exe --silent                install without UI
//!   aionix-setup.exe --update --wait-pid N   started by the app's updater: waits for the
//!                    [--minimized]           app to exit, installs, starts the new version
//!                                            (minimized again if it was, without focus)
//!   Desinstalar.exe --uninstall [--quiet]    uninstaller (Apps & features)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod install;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aionix_desktop::icons::{self, Icon};
use aionix_desktop::theme;
use egui::{
    Align, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Vec2,
    ViewportCommand,
};
use install::{Options, ProductInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Update,
    Uninstall,
}

#[derive(Debug, Clone)]
enum Page {
    Welcome,
    Working,
    Done,
    Failed(String),
}

#[derive(Default)]
struct Progress {
    fraction: f32,
    step: String,
    steps_done: Vec<String>,
    result: Option<Result<Option<PathBuf>, String>>,
}

struct SetupApp {
    mode: Mode,
    product: ProductInfo,
    page: Page,
    installed: Option<install::Installed>,
    desktop_shortcut: bool,
    open_after: bool,
    start_with_windows: bool,
    remove_data: bool,
    progress: Arc<Mutex<Progress>>,
    shown_fraction: f32,
    started: Instant,
    app_exe: Option<PathBuf>,
    /// Update of an app that sat minimized: reopen it minimized.
    relaunch_minimized: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let product = install::product();
    let mode = if has("--uninstall") {
        Mode::Uninstall
    } else if has("--update") {
        Mode::Update
    } else {
        Mode::Install
    };

    if mode == Mode::Update
        && let Some(pid) = args
            .iter()
            .position(|a| a == "--wait-pid")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u32>().ok())
    {
        install::wait_for_pid(pid, Duration::from_secs(60));
    }

    // Headless variants.
    if (mode == Mode::Uninstall && has("--quiet")) || (mode == Mode::Install && has("--silent")) {
        let mut sink = |_: f32, _: &str| {};
        let code = match mode {
            Mode::Uninstall => install::uninstall(&product, false, &mut sink).map(|_| ()),
            _ => {
                install::install(&product, &default_options(&product, true), &mut sink).map(|_| ())
            }
        };
        std::process::exit(if code.is_ok() { 0 } else { 1 });
    }

    let size = if mode == Mode::Update {
        Vec2::new(460.0, 184.0)
    } else {
        Vec2::new(780.0, 520.0)
    };
    let title = match mode {
        Mode::Install => format!("Instalar {}", product.display),
        Mode::Update => format!("Atualizando {}", product.display),
        Mode::Uninstall => format!("Desinstalar {}", product.display),
    };
    let relaunch_minimized = mode == Mode::Update && has("--minimized");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(&title)
            .with_active(!relaunch_minimized)
            .with_inner_size(size)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_decorations(false)
            .with_icon(decode_icon(SETUP_PNG)),
        centered: true,
        ..Default::default()
    };
    let installed = install::installed(&product);
    let _ = eframe::run_native(
        "aionix-setup",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            let mut app = SetupApp {
                mode,
                product,
                page: Page::Welcome,
                installed,
                desktop_shortcut: true,
                open_after: true,
                start_with_windows: true,
                remove_data: false,
                progress: Arc::new(Mutex::new(Progress::default())),
                shown_fraction: 0.0,
                started: Instant::now(),
                app_exe: None,
                relaunch_minimized,
            };
            if mode == Mode::Update {
                app.start(cc.egui_ctx.clone());
            }
            Ok(Box::new(app))
        }),
    );
}

const SETUP_PNG: &[u8] = include_bytes!("../../../branding/icons/setup-512.png");

fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .ok()?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

fn decode_icon(bytes: &[u8]) -> egui::IconData {
    match decode_png(bytes) {
        Some((rgba, width, height)) => egui::IconData {
            rgba,
            width,
            height,
        },
        None => aionix_desktop::icon::app_icon(),
    }
}

/// App + uninstaller (a copy of this setup).
fn install_size_mb() -> f64 {
    let me = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map_or(0, |m| m.len());
    (install::payload().len() as u64 + me) as f64 / 1_048_576.0
}

fn default_options(p: &ProductInfo, desktop: bool) -> Options {
    Options {
        dir: install::installed(p)
            .map(|i| i.dir)
            .unwrap_or_else(|| install::default_install_dir(p)),
        desktop_shortcut: desktop,
        start_with_windows: true,
    }
}

// ------------------------------------------------------------------------- app

impl SetupApp {
    fn start(&mut self, ctx: egui::Context) {
        self.page = Page::Working;
        self.started = Instant::now();
        *self.progress.lock().unwrap() = Progress::default();
        let (mode, product, progress) = (self.mode, self.product, self.progress.clone());
        let opts = Options {
            dir: self
                .installed
                .as_ref()
                .map(|i| i.dir.clone())
                .unwrap_or_else(|| install::default_install_dir(&product)),
            // Updates never add a desktop shortcut the user removed.
            desktop_shortcut: mode == Mode::Install && self.desktop_shortcut,
            // Updates turn it on (Task Manager's "Desabilitado" still wins).
            start_with_windows: mode != Mode::Install || self.start_with_windows,
        };
        let remove_data = self.remove_data;
        std::thread::spawn(move || {
            let mut report = |f: f32, step: &str| {
                let mut p = progress.lock().unwrap();
                if !p.step.is_empty() && p.step != step {
                    let prev = std::mem::take(&mut p.step);
                    p.steps_done.push(prev);
                }
                p.fraction = f;
                p.step = step.to_owned();
                ctx.request_repaint();
                // Small pause so each step is readable; the work itself is instant.
                std::thread::sleep(Duration::from_millis(260));
            };
            let result = match mode {
                Mode::Uninstall => {
                    install::uninstall(&product, remove_data, &mut report).map(|_| None)
                }
                _ => install::install(&product, &opts, &mut report).map(Some),
            };
            progress.lock().unwrap().result = Some(result);
            ctx.request_repaint();
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if !matches!(self.page, Page::Working) {
            return;
        }
        let result = self.progress.lock().unwrap().result.take();
        match result {
            Some(Ok(exe)) => {
                self.app_exe = exe;
                self.page = Page::Done;
                if self.mode == Mode::Update {
                    if let Some(exe) = &self.app_exe {
                        let _ = install::launch(exe, self.relaunch_minimized);
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            Some(Err(e)) => self.page = Page::Failed(e),
            None => {}
        }
    }
}

impl eframe::App for SetupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        let target = self.progress.lock().unwrap().fraction;
        self.shown_fraction += (target - self.shown_fraction) * 0.18;
        if (target - self.shown_fraction).abs() < 0.001 || theme::reduced_motion() {
            self.shown_fraction = target;
        }
        let p = theme::palette(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ui, |ui| {
                let full = ui.max_rect();
                if self.mode == Mode::Update {
                    self.update_view(ui, full);
                } else {
                    self.full_view(ui, full);
                }
                // Hairline around the borderless window.
                ui.painter().rect_stroke(
                    full.shrink(0.5),
                    CornerRadius::ZERO,
                    Stroke::new(1.0, p.border),
                    egui::StrokeKind::Inside,
                );
            });
        if matches!(self.page, Page::Working) {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

impl SetupApp {
    // ---------------------------------------------------------------- full window

    fn full_view(&mut self, ui: &mut egui::Ui, full: Rect) {
        let side = Rect::from_min_max(full.min, Pos2::new(full.min.x + 280.0, full.max.y));
        self.brand_panel(ui, side);
        let content = Rect::from_min_max(Pos2::new(side.max.x, full.min.y), full.max);
        self.title_bar(ui, content);
        let body = Rect::from_min_max(
            content.min + Vec2::new(44.0, 64.0),
            content.max - Vec2::new(40.0, 32.0),
        );
        // Each page fades in and rises into place.
        let key = ui.id().with("page");
        let name = format!("{:?}", self.page);
        let shown: Option<String> = ui.ctx().data(|d| d.get_temp(key.with("name")));
        if shown.as_deref() != Some(name.as_str()) {
            ui.ctx().data_mut(|d| d.insert_temp(key.with("name"), name));
            theme::restart(ui, key);
        }
        let t = theme::entrance(ui, key, 0.0, 0.32);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(body.translate(Vec2::new(0.0, (1.0 - t) * 6.0)))
                .layout(Layout::top_down(Align::Min)),
        );
        child.multiply_opacity(t);
        match self.page.clone() {
            Page::Welcome => match self.mode {
                Mode::Uninstall => self.uninstall_welcome(&mut child),
                _ => self.install_welcome(&mut child),
            },
            Page::Working => self.working(&mut child),
            Page::Done => self.done(&mut child),
            Page::Failed(e) => self.failed(&mut child, &e),
        }
    }

    /// Left panel: the mark, the name and what the product is, on the sidebar fill.
    fn brand_panel(&self, ui: &mut egui::Ui, rect: Rect) {
        let p = theme::palette(ui);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, p.sidebar);
        painter.vline(
            rect.max.x - 0.5,
            rect.y_range(),
            Stroke::new(1.0, p.hairline),
        );
        let drag = ui.interact(rect, ui.id().with("brand-drag"), Sense::drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let x = rect.min.x + 36.0;
        let mut y = rect.min.y + 64.0;
        theme::paint_mark(
            &painter,
            Rect::from_min_size(Pos2::new(x, y), Vec2::splat(64.0)),
        );
        y += 64.0 + 28.0;
        let width = rect.width() - 72.0;
        let name = painter.layout(
            self.product.display.to_owned(),
            theme::semibold(24.0),
            p.text,
            width,
        );
        let h = name.size().y;
        painter.galley(Pos2::new(x, y), name, p.text);
        y += h + 10.0;
        let tag = painter.layout(
            self.product.tagline.to_owned(),
            FontId::proportional(14.0),
            p.muted,
            width,
        );
        painter.galley(Pos2::new(x, y), tag, p.muted);
        // Footer: the promise and the version.
        let fy = rect.max.y - 40.0;
        icons::paint(
            &painter,
            Rect::from_center_size(Pos2::new(x + 7.0, fy - 24.0), Vec2::splat(14.0)),
            Icon::Lock,
            p.faint,
        );
        painter.text(
            Pos2::new(x + 22.0, fy - 24.0),
            egui::Align2::LEFT_CENTER,
            "Criptografia de ponta a ponta",
            FontId::proportional(12.5),
            p.faint,
        );
        painter.text(
            Pos2::new(x, fy),
            egui::Align2::LEFT_CENTER,
            format!("Versão {}", install::VERSION),
            FontId::proportional(12.5),
            p.faint,
        );
    }

    fn title_bar(&self, ui: &mut egui::Ui, content: Rect) {
        let p = theme::palette(ui);
        let bar = Rect::from_min_size(content.min, Vec2::new(content.width(), 44.0));
        let drag = ui.interact(bar, ui.id().with("title-drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let busy = matches!(self.page, Page::Working);
        let mut x = bar.max.x - 46.0;
        for (key, close) in [("close", true), ("min", false)] {
            let r = Rect::from_min_size(Pos2::new(x, bar.min.y), Vec2::new(46.0, 36.0));
            let resp = ui.interact(r, ui.id().with(key), Sense::click());
            let enabled = !(close && busy);
            let h = theme::toggle(ui, resp.id.with("h"), resp.hovered() && enabled, 0.12);
            let hover_fill = if close {
                Color32::from_rgb(0xC4, 0x2B, 0x1C)
            } else {
                p.surface_alt
            };
            if h > 0.0 {
                ui.painter()
                    .rect_filled(r, CornerRadius::ZERO, hover_fill.gamma_multiply(h));
            }
            let ink = if !enabled {
                p.border
            } else if close {
                theme::lerp_color(p.muted, Color32::WHITE, h)
            } else {
                p.muted
            };
            let c = r.center();
            let stroke = Stroke::new(1.2, ink);
            if close {
                ui.painter()
                    .line_segment([c + Vec2::new(-5.0, -5.0), c + Vec2::new(5.0, 5.0)], stroke);
                ui.painter()
                    .line_segment([c + Vec2::new(5.0, -5.0), c + Vec2::new(-5.0, 5.0)], stroke);
            } else {
                ui.painter()
                    .line_segment([c + Vec2::new(-5.0, 0.0), c + Vec2::new(5.0, 0.0)], stroke);
            }
            if resp.clicked() {
                if close && enabled {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                } else if !close {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                }
            }
            x -= 46.0;
        }
    }

    fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
        let p = theme::palette(ui);
        ui.label(theme::title(title, 24.0).color(p.text));
        ui.add_space(4.0);
        ui.label(RichText::new(subtitle).size(14.0).color(p.muted));
        ui.add_space(22.0);
    }

    /// Row of the actions at the bottom right (primary last, on the right).
    fn actions(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
        Self::actions_with_note(ui, None, add);
    }

    /// Like [`Self::actions`], with a small note on its own line above the buttons.
    fn actions_with_note(ui: &mut egui::Ui, note: Option<&str>, add: impl FnOnce(&mut egui::Ui)) {
        let p = theme::palette(ui);
        ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), 36.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    add(ui);
                },
            );
            if let Some(note) = note {
                ui.add_space(10.0);
                ui.label(RichText::new(note).size(12.0).color(p.faint));
            }
        });
    }

    fn install_welcome(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let name = self.product.display;
        let (title, subtitle) = match &self.installed {
            Some(i) if i.version == install::VERSION => (
                "Reinstalar".to_owned(),
                format!(
                    "A versão {} do {name} já está instalada. Reinstale para reparar a instalação.",
                    i.version
                ),
            ),
            Some(i) => (
                "Atualizar".to_owned(),
                format!(
                    "O {name} {} está instalado. Esta instalação atualiza para a versão {}; seus dados e o seu ID continuam os mesmos.",
                    i.version,
                    install::VERSION
                ),
            ),
            None => (
                "Instalar o Aionix Connect".to_owned(),
                "Instala só para o seu usuário, sem pedir permissão de administrador.".to_owned(),
            ),
        };
        Self::heading(ui, &title, &subtitle);
        let dir = default_options(&self.product, true).dir;
        ui.label(
            RichText::new("Local de instalação")
                .size(12.5)
                .color(p.muted),
        );
        ui.add(
            egui::Label::new(
                RichText::new(dir.display().to_string())
                    .size(13.5)
                    .color(p.text),
            )
            .truncate(),
        );
        ui.label(
            RichText::new(format!("{:.0} MB", install_size_mb()))
                .size(12.5)
                .color(p.faint),
        );
        ui.add_space(14.0);
        theme::hairline(ui);
        ui.add_space(10.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        if self.installed.is_none() {
            theme::toggle_chip(
                ui,
                &mut self.desktop_shortcut,
                "Atalho na Área de Trabalho",
                true,
            );
        }
        theme::toggle_chip(ui, &mut self.start_with_windows, "Iniciar com o Windows", true)
            .on_hover_text("Abre minimizado ao entrar no Windows, para este computador continuar acessível pelo ID depois de reiniciar.");
        theme::toggle_chip(ui, &mut self.open_after, "Abrir ao concluir", true);

        let note = "Use somente com a autorização de quem usa o computador.";
        Self::actions_with_note(ui, Some(note), |ui| {
            let label = match &self.installed {
                Some(i) if i.version == install::VERSION => "Reinstalar",
                Some(_) => "Atualizar",
                None => "Instalar",
            };
            if theme::primary_button(ui, label, !install::payload().is_empty(), Some(132.0))
                .clicked()
            {
                self.start(ui.ctx().clone());
            }
            if theme::button(ui, theme::Kind::Ghost, None, "Cancelar", true, None).clicked() {
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        });
    }

    fn uninstall_welcome(&mut self, ui: &mut egui::Ui) {
        let name = self.product.display;
        Self::heading(
            ui,
            &format!("Desinstalar o {name}"),
            "Remove o aplicativo, os atalhos, a inicialização com o Windows e o registro em Aplicativos instalados.",
        );
        theme::toggle_chip(
            ui,
            &mut self.remove_data,
            "Apagar também o ID deste computador e o login salvo",
            true,
        );
        Self::actions(ui, |ui| {
            if theme::danger_button(ui, "Desinstalar", true, Some(132.0)).clicked() {
                self.start(ui.ctx().clone());
            }
            if theme::button(ui, theme::Kind::Ghost, None, "Cancelar", true, None).clicked() {
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        });
    }

    fn working(&mut self, ui: &mut egui::Ui) {
        let (title, subtitle) = if self.mode == Mode::Uninstall {
            ("Desinstalando", "Só um instante.")
        } else {
            ("Instalando", "Isso leva poucos segundos.")
        };
        Self::heading(ui, title, subtitle);
        let pr = self.progress.lock().unwrap();
        progress_bar(ui, self.shown_fraction);
        ui.add_space(18.0);
        for s in &pr.steps_done {
            step_row(ui, s, true);
        }
        if !pr.step.is_empty() {
            step_row(ui, &pr.step, false);
        }
    }

    fn done(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let name = self.product.display;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
        let t = theme::entrance(ui, ui.id().with("done-check"), 0.05, 0.45);
        ui.painter().circle_filled(
            rect.center(),
            24.0 * (0.85 + 0.15 * t),
            p.success.gamma_multiply(0.15 * t),
        );
        icons::paint(
            &ui.painter().with_clip_rect(rect),
            rect.shrink(12.0),
            Icon::Check,
            p.success.gamma_multiply(t),
        );
        ui.add_space(18.0);
        if self.mode == Mode::Uninstall {
            Self::heading(
                ui,
                "Desinstalado",
                &format!("O {name} foi removido deste computador."),
            );
        } else {
            Self::heading(
                ui,
                "Tudo pronto",
                &format!(
                    "O {name} {} está instalado. O atalho está no Menu Iniciar.",
                    install::VERSION
                ),
            );
        }
        Self::actions(ui, |ui| {
            let open = self.mode == Mode::Install && self.open_after && self.app_exe.is_some();
            let label = if open {
                format!("Abrir o {name}")
            } else {
                "Concluir".to_owned()
            };
            if theme::primary_button(ui, &label, true, None).clicked() {
                if open && let Some(exe) = &self.app_exe {
                    let _ = install::launch(exe, false);
                }
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        });
    }

    fn failed(&mut self, ui: &mut egui::Ui, error: &str) {
        Self::heading(ui, "Não foi possível concluir", error);
        Self::actions(ui, |ui| {
            if theme::primary_button(ui, "Tentar novamente", true, None).clicked() {
                self.start(ui.ctx().clone());
            }
            if theme::button(ui, theme::Kind::Ghost, None, "Fechar", true, None).clicked() {
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        });
    }

    // ---------------------------------------------------------------- update window

    /// The compact window clients see while the app updates itself.
    fn update_view(&mut self, ui: &mut egui::Ui, full: Rect) {
        let p = theme::palette(ui);
        let drag = ui.interact(full, ui.id().with("drag"), Sense::drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let inner = full.shrink2(Vec2::new(28.0, 26.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::top_down(Align::Min)),
        );
        let ui = &mut child;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            theme::brand_mark(ui, 44.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(2.0);
                ui.label(
                    theme::title(format!("Atualizando o {}", self.product.display), 16.0)
                        .color(p.text),
                );
                ui.label(
                    RichText::new(format!(
                        "Versão {}. O app abre de novo sozinho.",
                        install::VERSION
                    ))
                    .size(13.0)
                    .color(p.muted),
                );
            });
        });
        ui.add_space(22.0);
        match &self.page {
            Page::Failed(e) => {
                ui.label(RichText::new(e).size(13.0).color(p.danger));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "Tentar novamente", true, None).clicked() {
                        self.start(ui.ctx().clone());
                    }
                    if theme::button(ui, theme::Kind::Ghost, None, "Fechar", true, None).clicked() {
                        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                    }
                });
            }
            _ => {
                progress_bar(ui, self.shown_fraction);
                ui.add_space(10.0);
                let step = self.progress.lock().unwrap().step.clone();
                ui.label(
                    RichText::new(if step.is_empty() {
                        "Aguardando o aplicativo fechar…".into()
                    } else {
                        step
                    })
                    .size(12.5)
                    .color(p.faint),
                );
            }
        }
    }
}

/// Thin rounded progress bar in the accent.
fn progress_bar(ui: &mut egui::Ui, fraction: f32) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 6.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(3), p.hairline);
    let w = (rect.width() * fraction.clamp(0.0, 1.0)).max(6.0);
    ui.painter().rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(w, rect.height())),
        CornerRadius::same(3),
        p.accent,
    );
}

fn step_row(ui: &mut egui::Ui, text: &str, done: bool) {
    let p = theme::palette(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if done {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Check, p.success);
        } else {
            aionix_desktop::views::spinner(ui, 16.0);
        }
        ui.label(
            RichText::new(text)
                .size(13.5)
                .color(if done { p.muted } else { p.text }),
        );
    });
}
