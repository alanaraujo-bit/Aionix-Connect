//! Aionix Connect setup: installer, silent updater and uninstaller in one executable.
//!
//!   aionix-setup.exe                         themed installer
//!   aionix-setup.exe --silent                install without UI
//!   aionix-setup.exe --update --wait-pid N   started by the app's updater: waits for the
//!                                            app to exit, installs, starts the new version
//!   Desinstalar.exe --uninstall [--quiet]    uninstaller (Apps & features)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod install;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aionix_desktop::theme::{self, DARK};
use egui::{
    Align, Color32, CornerRadius, FontId, Layout, Mesh, Pos2, Rect, RichText, Sense, Shape, Stroke,
    TextureHandle, Vec2, ViewportCommand,
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
    remove_data: bool,
    progress: Arc<Mutex<Progress>>,
    shown_fraction: f32,
    started: Instant,
    logo: Option<TextureHandle>,
    app_exe: Option<PathBuf>,
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(&title)
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
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            let logo = load_logo(&cc.egui_ctx, &product);
            let mut app = SetupApp {
                mode,
                product,
                page: Page::Welcome,
                installed,
                desktop_shortcut: true,
                open_after: true,
                remove_data: false,
                progress: Arc::new(Mutex::new(Progress::default())),
                shown_fraction: 0.0,
                started: Instant::now(),
                logo,
                app_exe: None,
            };
            if mode == Mode::Update {
                app.start(cc.egui_ctx.clone());
            }
            Ok(Box::new(app))
        }),
    );
}

const HOST_PNG: &[u8] = include_bytes!("../../../branding/icons/host-512.png");
const TECH_PNG: &[u8] = include_bytes!("../../../branding/icons/technician-512.png");
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
        None => aionix_desktop::icon::brand_icon(64, [0x4f, 0x7c, 0xff]),
    }
}

fn load_logo(ctx: &egui::Context, p: &ProductInfo) -> Option<TextureHandle> {
    let bytes = if p.slug == "technician" {
        TECH_PNG
    } else {
        HOST_PNG
    };
    let (rgba, w, h) = decode_png(bytes)?;
    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    Some(ctx.load_texture("logo", img, egui::TextureOptions::LINEAR))
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
    }
}

// ------------------------------------------------------------------------- palette

const GRAD_A: Color32 = Color32::from_rgb(0x5E, 0x8C, 0xFF);
const GRAD_B: Color32 = Color32::from_rgb(0x7A, 0x45, 0xF0);
const HOST_A: Color32 = Color32::from_rgb(0x6A, 0x97, 0xFF);
const HOST_B: Color32 = Color32::from_rgb(0x2D, 0x55, 0xE6);
const TECH_A: Color32 = Color32::from_rgb(0xA3, 0x7B, 0xFF);
const TECH_B: Color32 = Color32::from_rgb(0x55, 0x38, 0xDB);

fn product_gradient(p: &ProductInfo) -> (Color32, Color32) {
    if p.slug == "technician" {
        (TECH_A, TECH_B)
    } else {
        (HOST_A, HOST_B)
    }
}

/// Diagonal gradient rectangle (top-left a → bottom-right b).
fn gradient_rect(painter: &egui::Painter, rect: Rect, a: Color32, b: Color32) {
    let mid = lerp_color(a, b, 0.5);
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), a);
    mesh.colored_vertex(rect.right_top(), mid);
    mesh.colored_vertex(rect.right_bottom(), b);
    mesh.colored_vertex(rect.left_bottom(), mid);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
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
                        let _ = install::launch(exe);
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
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(DARK.bg))
            .show(ui, |ui| {
                let full = ui.max_rect();
                if self.mode == Mode::Update {
                    self.update_view(ui, full);
                } else {
                    self.full_view(ui, full);
                }
                // Thin border around the borderless window.
                ui.painter().rect_stroke(
                    full.shrink(0.5),
                    CornerRadius::ZERO,
                    Stroke::new(1.0, DARK.border),
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
        let hero = Rect::from_min_max(full.min, Pos2::new(full.min.x + 300.0, full.max.y));
        self.hero(ui, hero);
        let content = Rect::from_min_max(Pos2::new(hero.max.x, full.min.y), full.max);
        self.title_bar(ui, content);
        let body = Rect::from_min_max(
            content.min + Vec2::new(40.0, 56.0),
            content.max - Vec2::new(40.0, 32.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(body)
                .layout(Layout::top_down(Align::Min)),
        );
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

    fn hero(&self, ui: &mut egui::Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        let (a, b) = product_gradient(&self.product);
        gradient_rect(&painter, rect, a, b);
        // Soft decorative glows.
        painter.circle_filled(
            rect.left_top() + Vec2::new(40.0, 30.0),
            160.0,
            Color32::from_white_alpha(14),
        );
        painter.circle_filled(
            rect.right_bottom() + Vec2::new(-10.0, 10.0),
            190.0,
            Color32::from_black_alpha(26),
        );
        // Drag the window from the hero too.
        let drag = ui.interact(rect, ui.id().with("hero-drag"), Sense::drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let x = rect.min.x + 34.0;
        let mut y = rect.min.y + 58.0;
        if let Some(logo) = &self.logo {
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(84.0));
            painter.rect_filled(
                r.expand(6.0),
                CornerRadius::same(26),
                Color32::from_white_alpha(26),
            );
            painter.image(
                logo.id(),
                r,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            y += 120.0;
        }
        let white = Color32::WHITE;
        let soft = Color32::from_white_alpha(205);
        let name_galley = painter.layout(
            self.product.display.to_owned(),
            theme::semibold(27.0),
            white,
            rect.width() - 68.0,
        );
        let h = name_galley.size().y;
        painter.galley(Pos2::new(x, y), name_galley, white);
        y += h + 12.0;
        let tag = painter.layout(
            self.product.tagline.to_owned(),
            FontId::proportional(14.5),
            soft,
            rect.width() - 68.0,
        );
        painter.galley(Pos2::new(x, y), tag, soft);
        // Footer badges.
        let fy = rect.max.y - 40.0;
        painter.text(
            Pos2::new(x, fy),
            egui::Align2::LEFT_CENTER,
            format!("Versão {}", install::VERSION),
            FontId::proportional(12.5),
            soft,
        );
        painter.text(
            Pos2::new(x, fy - 22.0),
            egui::Align2::LEFT_CENTER,
            "🔒  Criptografia de ponta a ponta",
            FontId::proportional(12.5),
            soft,
        );
    }

    fn title_bar(&self, ui: &mut egui::Ui, content: Rect) {
        let bar = Rect::from_min_size(content.min, Vec2::new(content.width(), 44.0));
        let drag = ui.interact(bar, ui.id().with("title-drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let busy = matches!(self.page, Page::Working);
        let mut x = bar.max.x - 46.0;
        for (glyph, close) in [("close", true), ("min", false)] {
            let r = Rect::from_min_size(Pos2::new(x, bar.min.y), Vec2::new(46.0, 36.0));
            let resp = ui.interact(r, ui.id().with(glyph), Sense::click());
            let hovered = resp.hovered() && !(close && busy);
            if hovered {
                ui.painter().rect_filled(
                    r,
                    CornerRadius::ZERO,
                    if close {
                        Color32::from_rgb(0xC4, 0x2B, 0x1C)
                    } else {
                        DARK.surface_alt
                    },
                );
            }
            let color = if close && busy {
                DARK.border
            } else if hovered && close {
                Color32::WHITE
            } else {
                DARK.muted
            };
            let c = r.center();
            let stroke = Stroke::new(1.2, color);
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
                if close && !busy {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                } else if !close {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                }
            }
            x -= 46.0;
        }
    }

    fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.label(theme::title(title, 25.0).color(DARK.text));
        ui.add_space(6.0);
        ui.label(RichText::new(subtitle).size(14.5).color(DARK.muted));
        ui.add_space(18.0);
    }

    fn install_welcome(&mut self, ui: &mut egui::Ui) {
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
                "Pronto para instalar".to_owned(),
                format!(
                    "O {name} será instalado só para o seu usuário — sem pedir permissão de administrador."
                ),
            ),
        };
        Self::heading(ui, &title, &subtitle);
        let dir = default_options(&self.product, true).dir;
        egui::Frame::new()
            .fill(DARK.surface)
            .stroke(Stroke::new(1.0, DARK.border))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("Local de instalação")
                        .size(12.0)
                        .color(DARK.muted),
                );
                ui.label(
                    RichText::new(dir.display().to_string())
                        .size(13.0)
                        .color(DARK.text)
                        .monospace(),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Espaço necessário: {:.0} MB", install_size_mb()))
                        .size(12.0)
                        .color(DARK.muted),
                );
            });
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            if self.installed.is_none() {
                theme::toggle_chip(
                    ui,
                    &mut self.desktop_shortcut,
                    "Atalho na Área de Trabalho",
                    true,
                );
            }
            theme::toggle_chip(ui, &mut self.open_after, "Abrir ao concluir", true);
        });

        ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = match &self.installed {
                        Some(i) if i.version == install::VERSION => "Reinstalar",
                        Some(_) => "Atualizar",
                        None => "Instalar",
                    };
                    if theme::primary_button(ui, label, !install::payload().is_empty(), Some(150.0))
                        .clicked()
                    {
                        self.start(ui.ctx().clone());
                    }
                    if theme::secondary_button(ui, "Cancelar", true, Some(120.0)).clicked() {
                        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                    }
                });
            });
            ui.add_space(10.0);
            ui.label(
                RichText::new("Use apenas com a autorização de quem utiliza o computador.")
                    .size(11.5)
                    .color(DARK.muted),
            );
        });
    }

    fn uninstall_welcome(&mut self, ui: &mut egui::Ui) {
        let name = self.product.display;
        Self::heading(
            ui,
            &format!("Desinstalar o {name}"),
            "Remove o aplicativo, os atalhos e o registro em Aplicativos instalados.",
        );
        let data_text = if self.product.slug == "host" {
            "Remover também os dados deste computador (o ID de 9 dígitos será trocado)"
        } else {
            "Remover também os dados (login salvo e registros)"
        };
        theme::toggle_chip(ui, &mut self.remove_data, data_text, true);
        ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::danger_button(ui, "Desinstalar", true, Some(150.0)).clicked() {
                    self.start(ui.ctx().clone());
                }
                if theme::secondary_button(ui, "Cancelar", true, Some(120.0)).clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
        });
    }

    fn working(&mut self, ui: &mut egui::Ui) {
        let (title, subtitle) = if self.mode == Mode::Uninstall {
            ("Desinstalando…", "Só um instante.")
        } else {
            ("Instalando…", "Isso leva só alguns segundos.")
        };
        Self::heading(ui, title, subtitle);
        let p = self.progress.lock().unwrap();
        progress_bar(ui, self.shown_fraction, self.started);
        ui.add_space(18.0);
        for s in &p.steps_done {
            step_row(ui, s, true);
        }
        if !p.step.is_empty() {
            step_row(ui, &p.step, false);
        }
    }

    fn done(&mut self, ui: &mut egui::Ui) {
        let name = self.product.display;
        let center = ui.cursor().min + Vec2::new(28.0, 28.0);
        ui.painter()
            .circle_filled(center, 28.0, DARK.success.gamma_multiply(0.18));
        ui.painter()
            .circle_stroke(center, 28.0, Stroke::new(2.0, DARK.success));
        let s = 9.0;
        ui.painter().line_segment(
            [
                center + Vec2::new(-s, 0.5),
                center + Vec2::new(-s * 0.25, s * 0.8),
            ],
            Stroke::new(3.0, DARK.success),
        );
        ui.painter().line_segment(
            [
                center + Vec2::new(-s * 0.25, s * 0.8),
                center + Vec2::new(s * 1.1, -s * 0.7),
            ],
            Stroke::new(3.0, DARK.success),
        );
        ui.add_space(74.0);
        if self.mode == Mode::Uninstall {
            Self::heading(
                ui,
                "Desinstalado",
                &format!("O {name} foi removido deste computador."),
            );
        } else {
            Self::heading(
                ui,
                "Tudo pronto!",
                &format!(
                    "O {name} {} está instalado. Você encontra o atalho no Menu Iniciar.",
                    install::VERSION
                ),
            );
        }
        ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let open = self.mode == Mode::Install && self.open_after && self.app_exe.is_some();
                let label = if open {
                    format!("Abrir o {name}")
                } else {
                    "Concluir".to_owned()
                };
                if theme::primary_button(ui, &label, true, None).clicked() {
                    if open && let Some(exe) = &self.app_exe {
                        let _ = install::launch(exe);
                    }
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
        });
    }

    fn failed(&mut self, ui: &mut egui::Ui, error: &str) {
        Self::heading(ui, "Não foi possível concluir", error);
        ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::primary_button(ui, "Tentar novamente", true, Some(170.0)).clicked() {
                    self.start(ui.ctx().clone());
                }
                if theme::secondary_button(ui, "Fechar", true, Some(110.0)).clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
        });
    }

    // ---------------------------------------------------------------- update window

    fn update_view(&mut self, ui: &mut egui::Ui, full: Rect) {
        let drag = ui.interact(full, ui.id().with("drag"), Sense::drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let (a, b) = product_gradient(&self.product);
        let strip = Rect::from_min_size(full.min, Vec2::new(full.width(), 4.0));
        gradient_rect(&ui.painter_at(strip), strip, a, b);
        let inner = full.shrink2(Vec2::new(28.0, 26.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::top_down(Align::Min)),
        );
        let ui = &mut child;
        ui.horizontal(|ui| {
            if let Some(logo) = &self.logo {
                ui.add(
                    egui::Image::new(logo)
                        .fit_to_exact_size(Vec2::splat(46.0))
                        .corner_radius(12),
                );
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    theme::title(format!("Atualizando o {}", self.product.display), 17.0)
                        .color(DARK.text),
                );
                ui.label(
                    RichText::new(format!("Instalando a versão {}…", install::VERSION))
                        .size(13.0)
                        .color(DARK.muted),
                );
            });
        });
        ui.add_space(20.0);
        match &self.page {
            Page::Failed(e) => {
                ui.label(RichText::new(e).size(13.0).color(DARK.danger));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "Tentar novamente", true, None).clicked() {
                        self.start(ui.ctx().clone());
                    }
                    if theme::secondary_button(ui, "Fechar", true, None).clicked() {
                        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                    }
                });
            }
            _ => {
                progress_bar(ui, self.shown_fraction, self.started);
                ui.add_space(8.0);
                let step = self.progress.lock().unwrap().step.clone();
                ui.label(
                    RichText::new(if step.is_empty() {
                        "Aguardando o aplicativo fechar…".into()
                    } else {
                        step
                    })
                    .size(12.5)
                    .color(DARK.muted),
                );
            }
        }
    }
}

/// Rounded progress bar with the brand gradient and a moving sheen.
fn progress_bar(ui: &mut egui::Ui, fraction: f32, started: Instant) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(5), DARK.surface_alt);
    let w = (rect.width() * fraction.clamp(0.0, 1.0)).max(10.0);
    let fill = Rect::from_min_size(rect.min, Vec2::new(w, rect.height()));
    let clip = ui.painter_at(fill);
    gradient_rect(&clip, fill, GRAD_A, GRAD_B);
    let t = started.elapsed().as_secs_f32();
    let sheen_x = fill.min.x + ((t * 0.9).fract() * (fill.width() + 120.0)) - 60.0;
    let sheen = Rect::from_min_size(
        Pos2::new(sheen_x, fill.min.y),
        Vec2::new(60.0, fill.height()),
    );
    clip.rect_filled(
        sheen.intersect(fill),
        CornerRadius::ZERO,
        Color32::from_white_alpha(38),
    );
    // Round the ends by redrawing the background corners over the fill.
    painter.rect_stroke(
        rect,
        CornerRadius::same(5),
        Stroke::new(1.0, DARK.bg),
        egui::StrokeKind::Outside,
    );
}

fn step_row(ui: &mut egui::Ui, text: &str, done: bool) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
        if done {
            ui.painter()
                .circle_filled(r.center(), 8.0, DARK.success.gamma_multiply(0.2));
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "✓",
                FontId::proportional(11.0),
                DARK.success,
            );
        } else {
            ui.add_space(-18.0);
            ui.add(egui::Spinner::new().size(16.0).color(DARK.accent));
        }
        ui.label(
            RichText::new(text)
                .size(13.5)
                .color(if done { DARK.muted } else { DARK.text }),
        );
    });
}
