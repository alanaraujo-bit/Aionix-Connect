//! The screens of Aionix Connect as plain functions: each takes a small model (strings
//! and flags, no handles) and returns what the user asked for. `HostSide`, `TechSide`
//! and the app shell build the models and act on the answers, so every screen can also
//! be rendered with sample data (tests/ui_preview.rs).

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Margin, Rect, RichText, Sense,
    Stroke, Ui, UiBuilder, Vec2, pos2, vec2,
};

use crate::icons::{self, Icon};
use crate::theme::{self, Kind, Palette};

use uuid::Uuid;

// ------------------------------------------------------------------ shared bits

/// Meaning of a status color; views pick the palette entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

impl Tone {
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
            Tone::Neutral => p.faint,
            Tone::Accent => p.accent,
            Tone::Success => p.success,
            Tone::Warning => p.warning,
            Tone::Danger => p.danger,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Home,
    Computers,
    Admin,
}

/// Rotating arc in the accent color.
pub fn spinner(ui: &mut Ui, size: f32) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let c = rect.center();
    let r = size / 2.0 - 1.5;
    ui.painter()
        .circle_stroke(c, r, Stroke::new(2.0, p.hairline));
    let start = if theme::reduced_motion() {
        0.0
    } else {
        t * 5.5
    };
    let pts: Vec<_> = (0..=24)
        .map(|i| {
            let a = start + i as f32 / 24.0 * 1.6;
            pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect();
    ui.painter()
        .add(egui::Shape::line(pts, Stroke::new(2.0, p.accent)));
    if !theme::reduced_motion() {
        ui.ctx().request_repaint();
    }
}

/// Digits only, at most nine, grouped as "000 000 000".
pub fn format_id_input(raw: &str) -> String {
    let digits: String = raw.chars().filter(char::is_ascii_digit).take(9).collect();
    digits
        .as_bytes()
        .chunks(3)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Single-line field; returns the response and whether Enter was pressed in it.
pub(crate) fn field(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    password: bool,
    enabled: bool,
) -> (egui::Response, bool) {
    let p = theme::palette(ui);
    let edit = egui::TextEdit::singleline(text)
        .hint_text(RichText::new(hint).color(p.faint))
        .password(password)
        .desired_width(width - 24.0)
        .min_size(vec2(width, 36.0))
        .vertical_align(Align::Center)
        .margin(Margin::symmetric(12, 8));
    let r = ui.add_enabled(enabled, edit);
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    (r, enter)
}

fn section_heading(ui: &mut Ui, text: &str, detail: &str) {
    let p = theme::palette(ui);
    ui.label(theme::title(text, theme::SIZE_HEADING));
    if !detail.is_empty() {
        ui.add_space(-4.0);
        ui.label(RichText::new(detail).size(13.5).color(p.muted));
    }
}

/// A quiet "show more" row: chevron and label; the content fades in under it. The
/// open state lives in egui memory.
pub fn disclosure(ui: &mut Ui, label: &str, content: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui);
    let id = ui.id().with(("disclosure", label));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(theme::SIZE_SMALL),
        p.muted,
    );
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 22.0, 24.0), Sense::click());
    if response.clicked() {
        open = !open;
        ui.ctx().data_mut(|d| d.insert_temp(id, open));
    }
    let h = theme::toggle(ui, id.with("h"), response.hovered(), 0.12);
    let ink = theme::lerp_color(p.muted, p.text, h);
    let icon = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(rect.min.x + 7.0, rect.center().y), Vec2::splat(12.0)),
        icon,
        ink,
    );
    ui.painter().galley(
        pos2(rect.min.x + 20.0, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    if response.has_focus() {
        theme::focus_ring(ui, rect, 6);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let t = theme::toggle(ui, id.with("t"), open, 0.22);
    if open {
        ui.scope(|ui| {
            ui.multiply_opacity(t);
            ui.add_space(6.0);
            content(ui);
        });
    }
}

// ------------------------------------------------------------------ sidebar

pub struct AccountModel<'a> {
    pub name: &'a str,
    pub org: &'a str,
}

pub enum UpdateModel<'a> {
    /// Development build: nothing to show but the version.
    Disabled,
    Idle,
    Checking,
    UpToDate,
    Downloading {
        version: &'a str,
        fraction: f32,
    },
    Ready {
        version: &'a str,
        can_restart: bool,
    },
    Failed {
        error: &'a str,
    },
}

pub struct SidebarModel<'a> {
    pub page: Page,
    pub account: Option<AccountModel<'a>>,
    pub signing_in: bool,
    /// Computers online in the organization (signed in).
    pub online: Option<usize>,
    pub env_label: Option<&'a str>,
    pub version: &'a str,
    pub update: UpdateModel<'a>,
    /// Show "Administração" (the account manages its organization).
    pub admin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarAction {
    Go(Page),
    SignIn,
    SignOut,
    ChangePassword,
    CheckUpdates,
    RestartToUpdate,
}

pub const SIDEBAR_WIDTH: f32 = 232.0;

pub fn sidebar(ui: &mut Ui, m: &SidebarModel<'_>) -> Option<SidebarAction> {
    let p = theme::palette(ui);
    let mut action = None;
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, p.sidebar);
    ui.painter().vline(
        full.max.x - 0.5,
        full.y_range(),
        Stroke::new(1.0, p.hairline),
    );
    let inner = full.shrink2(vec2(12.0, 14.0));
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut ui;
    // The bottom block first, so the rest takes what is left above it.
    egui::Panel::bottom("sidebar-bottom")
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .resizable(false)
        .show(ui, |ui| sidebar_bottom(ui, m, &mut action));
    ui.spacing_mut().item_spacing.y = 2.0;

    // Brand.
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.x = 10.0;
        theme::brand_mark(ui, 26.0);
        ui.label(theme::title("Aionix Connect", 14.5));
    });
    if let Some(label) = m.env_label {
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(p.warning.gamma_multiply(0.14))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(label)
                        .size(theme::SIZE_CAPTION)
                        .color(p.warning),
                );
            });
    }
    ui.add_space(20.0);

    let computers_trailing = match (&m.account, m.online) {
        (None, _) => Trailing::Icon(Icon::Lock),
        (Some(_), Some(n)) if n > 0 => Trailing::Count(n),
        _ => Trailing::None,
    };
    if nav_item(
        ui,
        Icon::Home,
        "Início",
        m.page == Page::Home,
        Trailing::None,
    )
    .clicked()
    {
        action = Some(SidebarAction::Go(Page::Home));
    }
    if nav_item(
        ui,
        Icon::Computers,
        "Computadores",
        m.page == Page::Computers,
        computers_trailing,
    )
    .clicked()
    {
        action = Some(SidebarAction::Go(Page::Computers));
    }
    if m.admin
        && nav_item(
            ui,
            Icon::Shield,
            "Administração",
            m.page == Page::Admin,
            Trailing::None,
        )
        .clicked()
    {
        action = Some(SidebarAction::Go(Page::Admin));
    }

    action
}

/// Update state, account and version, pinned to the bottom of the sidebar.
fn sidebar_bottom(ui: &mut Ui, m: &SidebarModel<'_>, action: &mut Option<SidebarAction>) {
    let p = theme::palette(ui);
    ui.spacing_mut().item_spacing.y = 0.0;
    match &m.update {
        UpdateModel::Ready {
            version,
            can_restart,
        } => {
            update_card(ui, version, *can_restart, action);
            ui.add_space(12.0);
        }
        UpdateModel::Downloading { version, fraction } => {
            download_line(ui, version, *fraction);
            ui.add_space(14.0);
        }
        UpdateModel::Failed { error } => {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new("Não foi possível verificar atualizações.")
                            .size(theme::SIZE_CAPTION)
                            .color(p.warning),
                    )
                    .on_hover_text(*error);
                    if theme::link(ui, "Tentar de novo").clicked() {
                        *action = Some(SidebarAction::CheckUpdates);
                    }
                });
            });
            ui.add_space(14.0);
        }
        _ => {}
    }
    theme::hairline(ui);
    ui.add_space(10.0);
    account_row(ui, m, action);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(
            RichText::new(format!("Versão {}", m.version))
                .size(theme::SIZE_CAPTION)
                .color(p.faint),
        );
        match &m.update {
            UpdateModel::Checking => {
                ui.label(
                    RichText::new("· verificando…")
                        .size(theme::SIZE_CAPTION)
                        .color(p.faint),
                );
            }
            UpdateModel::UpToDate => {
                ui.label(
                    RichText::new("· atualizado")
                        .size(theme::SIZE_CAPTION)
                        .color(p.faint),
                );
            }
            UpdateModel::Idle => {
                ui.label(RichText::new("·").size(theme::SIZE_CAPTION).color(p.faint));
                if theme::link(ui, "Verificar atualizações").clicked() {
                    *action = Some(SidebarAction::CheckUpdates);
                }
            }
            _ => {}
        }
    });
}

enum Trailing {
    None,
    Count(usize),
    Icon(Icon),
}

fn nav_item(
    ui: &mut Ui,
    icon: Icon,
    text: &str,
    selected: bool,
    trailing: Trailing,
) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let h = theme::toggle(ui, response.id.with("h"), response.hovered(), 0.12);
    let s = theme::toggle(ui, response.id.with("s"), selected, 0.18);
    let hover_fill = theme::lerp_color(p.sidebar, p.text, 0.05);
    let fill = theme::lerp_color(
        theme::lerp_color(p.sidebar, hover_fill, h),
        p.accent_soft,
        s,
    );
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    let ink = theme::lerp_color(p.muted, p.accent_text, s);
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(rect.min.x + 20.0, rect.center().y), Vec2::splat(16.0)),
        icon,
        ink,
    );
    let font = if selected {
        theme::semibold(theme::SIZE_BODY)
    } else {
        FontId::proportional(theme::SIZE_BODY)
    };
    ui.painter().text(
        pos2(rect.min.x + 38.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font,
        p.text,
    );
    match trailing {
        Trailing::None => {}
        Trailing::Count(n) => {
            ui.painter().text(
                pos2(rect.max.x - 10.0, rect.center().y),
                Align2::RIGHT_CENTER,
                n.to_string(),
                FontId::proportional(theme::SIZE_SMALL),
                p.faint,
            );
        }
        Trailing::Icon(i) => icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(rect.max.x - 16.0, rect.center().y), Vec2::splat(13.0)),
            i,
            p.faint,
        ),
    }
    if response.has_focus() {
        theme::focus_ring(ui, rect, 8);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn account_row(ui: &mut Ui, m: &SidebarModel<'_>, action: &mut Option<SidebarAction>) {
    let p = theme::palette(ui);
    if let Some(a) = &m.account {
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 40.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(4.0);
                ui.spacing_mut().item_spacing.x = 10.0;
                theme::avatar(ui, a.name, 32.0);
                let w = ui.available_width() - 76.0;
                ui.allocate_ui_with_layout(vec2(w, 36.0), Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add(egui::Label::new(theme::title(a.name, 13.5)).truncate());
                    ui.add(
                        egui::Label::new(
                            RichText::new(a.org)
                                .size(theme::SIZE_CAPTION)
                                .color(p.muted),
                        )
                        .truncate(),
                    );
                });
                ui.spacing_mut().item_spacing.x = 2.0;
                if theme::icon_button(ui, Icon::Key, "Alterar senha", false, true).clicked() {
                    *action = Some(SidebarAction::ChangePassword);
                }
                if theme::icon_button(
                    ui,
                    Icon::SignOut,
                    "Sair da conta neste computador",
                    false,
                    true,
                )
                .clicked()
                {
                    *action = Some(SidebarAction::SignOut);
                }
            },
        );
    } else if m.signing_in {
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 40.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(10.0);
                spinner(ui, 16.0);
                ui.label(RichText::new("Entrando…").size(13.5).color(p.muted));
            },
        );
    } else if theme::button(
        ui,
        Kind::Secondary,
        Some(Icon::User),
        "Entrar",
        true,
        Some(ui.available_width()),
    )
    .clicked()
    {
        *action = Some(SidebarAction::SignIn);
    }
}

fn update_card(ui: &mut Ui, version: &str, can_restart: bool, action: &mut Option<SidebarAction>) {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.bg)
        .stroke(Stroke::new(1.0, p.hairline))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
                    icons::paint(ui.painter(), r, Icon::Download, p.accent_text);
                    ui.label(theme::title("Atualização pronta", 13.0));
                });
                ui.label(
                    RichText::new(format!("A versão {version} instala em segundos."))
                        .size(theme::SIZE_SMALL)
                        .color(p.muted),
                );
                ui.add_space(8.0);
                let r = theme::button(
                    ui,
                    Kind::Primary,
                    None,
                    "Reiniciar e atualizar",
                    can_restart,
                    Some(ui.available_width()),
                );
                let r = if can_restart {
                    r
                } else {
                    r.on_disabled_hover_text("Disponível quando não houver sessão em andamento.")
                };
                if r.clicked() {
                    *action = Some(SidebarAction::RestartToUpdate);
                }
            });
        });
}

fn download_line(ui: &mut Ui, version: &str, fraction: f32) {
    let p = theme::palette(ui);
    ui.with_layout(Layout::top_down(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Baixando a versão {version}"))
                    .size(theme::SIZE_SMALL)
                    .color(p.muted),
            );
        });
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
        let rect = rect.shrink2(vec2(6.0, 0.0));
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), p.hairline);
        let target = fraction.clamp(0.0, 1.0);
        let shown = if theme::reduced_motion() {
            target
        } else {
            ui.ctx()
                .animate_value_with_time(Id::new("update-progress"), target, 0.3)
        };
        let mut done = rect;
        done.set_width(rect.width() * shown);
        ui.painter()
            .rect_filled(done, CornerRadius::same(2), p.accent);
    });
}

// ------------------------------------------------------------------ home: this computer

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStatus {
    Starting,
    Connecting,
    Online,
    Offline,
}

pub struct InboundModel {
    pub name: String,
    /// Shown on hover over the name.
    pub email: Option<String>,
    /// "ver a tela, usar mouse e teclado".
    pub permissions: String,
    pub duration: Option<String>,
    /// Session state in words when it is not simply "connected".
    pub state: Option<(String, Tone)>,
    pub can_end: bool,
    /// Connected without the owner's confirmation (unattended access, ADR-0015).
    pub unattended: bool,
}

pub struct HostModel {
    /// ("845 948 277", "845948277").
    pub id: Option<(String, String)>,
    pub status: HostStatus,
    pub status_text: String,
    pub status_detail: Option<String>,
    /// Name of who is asking right now.
    pub pending: Option<String>,
    pub inbound: Option<InboundModel>,
    pub last_outcome: Option<String>,
    pub copied: bool,
    /// Enrolled as a managed computer: unattended access is available here.
    pub managed: bool,
    pub unattended_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAction {
    Copy(String),
    EndInbound,
    SetUnattended(bool),
}

/// Banners for someone asking for, or using, this computer. Drawn at the top of Início.
pub fn inbound_banner(ui: &mut Ui, m: &HostModel) -> Option<HostAction> {
    let p = theme::palette(ui);
    let mut action = None;
    let appear = |ui: &Ui, key: &str| theme::entrance(ui, Id::new(("banner", key)), 0.0, 0.35);
    if let Some(name) = &m.pending {
        let t = appear(ui, &format!("pending-{name}"));
        ui.scope(|ui| {
            ui.multiply_opacity(t);
            banner_frame(ui, p.accent_soft, p.accent.gamma_multiply(0.35)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                    icons::paint(ui.painter(), r, Icon::Lock, p.accent_text);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(theme::title(
                            format!("{name} pediu para acessar este computador"),
                            14.0,
                        ));
                        ui.label(
                            RichText::new(
                                "Responda na janela de confirmação: Permitir ou Recusar.",
                            )
                            .size(13.0)
                            .color(p.muted),
                        );
                    });
                });
            });
        });
        ui.add_space(24.0);
    } else if let Some(s) = &m.inbound {
        let t = appear(ui, &format!("inbound-{}", s.name));
        ui.scope(|ui| {
            ui.multiply_opacity(t);
            banner_frame(
                ui,
                p.success.gamma_multiply(0.10),
                p.success.gamma_multiply(0.35),
            )
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let (dot, pulse) = match &s.state {
                        Some((_, tone)) => (tone.color(&p), *tone == Tone::Warning),
                        None => (p.success, false),
                    };
                    ui.add_space(4.0);
                    theme::status_dot(ui, dot, pulse);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        let headline = if s.unattended {
                            format!("Acesso sem aceitação: {} está conectado", s.name)
                        } else {
                            format!("{} está acessando este computador", s.name)
                        };
                        let who = ui.label(theme::title(headline, 14.0));
                        if let Some(e) = &s.email {
                            who.on_hover_text(e);
                        }
                        let mut line = format!("Pode {}", s.permissions);
                        if let Some(d) = &s.duration {
                            line.push_str(&format!("  ·  {d}"));
                        }
                        if let Some((state, _)) = &s.state {
                            line.push_str(&format!("  ·  {state}"));
                        }
                        ui.label(RichText::new(line).size(13.0).color(p.muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::button(ui, Kind::Danger, None, "Encerrar acesso", s.can_end, None)
                            .clicked()
                        {
                            action = Some(HostAction::EndInbound);
                        }
                    });
                });
            });
        });
        ui.add_space(24.0);
    }
    action
}

fn banner_frame(_ui: &Ui, fill: Color32, stroke: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(16, 14))
}

/// "Este computador": the ID, copy, status and the consent promise.
pub fn this_computer(ui: &mut Ui, m: &HostModel) -> Option<HostAction> {
    let p = theme::palette(ui);
    let mut action = None;
    ui.label(RichText::new("Seu ID").size(13.5).color(p.muted));
    ui.add_space(-2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 20.0;
        match &m.id {
            Some((display, compact)) => {
                id_digits(ui, display);
                ui.allocate_ui_with_layout(
                    vec2(140.0, 54.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        let (icon, text) = if m.copied {
                            (Icon::Check, "Copiado")
                        } else {
                            (Icon::Copy, "Copiar")
                        };
                        if theme::button(ui, Kind::Secondary, Some(icon), text, true, Some(112.0))
                            .on_hover_text("Copiar o ID para enviar por mensagem")
                            .clicked()
                        {
                            action = Some(HostAction::Copy(compact.clone()));
                        }
                    },
                );
            }
            None => {
                ui.allocate_ui_with_layout(
                    vec2(330.0, 54.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        theme::skeleton(ui, vec2(300.0, 36.0));
                    },
                );
            }
        }
    });
    ui.add_space(6.0);
    let (tone, pulse) = match m.status {
        HostStatus::Online => (Tone::Success, false),
        HostStatus::Starting | HostStatus::Connecting => (Tone::Warning, true),
        HostStatus::Offline => (Tone::Danger, false),
    };
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        theme::status_line(ui, &m.status_text, tone.color(&p), pulse);
        if let Some(d) = &m.status_detail {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                ui.label(RichText::new(d).size(13.0).color(p.muted));
            });
        }
    });
    ui.add_space(14.0);
    let promise = if m.unattended_enabled {
        "Passe este ID para quem vai te ajudar. O acesso sem aceitação está ligado: você pode desligá-lo a qualquer momento."
    } else {
        "Passe este ID para quem vai te ajudar. Ninguém acessa este computador sem a sua permissão."
    };
    ui.label(RichText::new(promise).size(13.5).color(p.muted));
    if m.managed {
        ui.add_space(18.0);
        let mut on = m.unattended_enabled;
        if theme::toggle_chip(ui, &mut on, "Permitir acesso sem aceitação", true).clicked() {
            action = Some(HostAction::SetUnattended(on));
        }
        let explain = if m.unattended_enabled {
            "Técnicos autorizados pela sua empresa podem conectar com a senha de acesso, sem clicar em Permitir. Enquanto houver uma sessão, ela aparece nesta janela."
        } else {
            "Desligado: cada acesso precisa da sua confirmação em Permitir."
        };
        ui.label(RichText::new(explain).size(13.0).color(p.muted));
    }
    if let Some(o) = &m.last_outcome
        && m.inbound.is_none()
        && m.pending.is_none()
    {
        ui.add_space(2.0);
        ui.label(RichText::new(o).size(13.0).color(p.faint));
    }
    action
}

/// The ID at display size; on first appearance each digit rises into place, group by
/// group, like the numbers on a departures board settling.
fn id_digits(ui: &mut Ui, display: &str) {
    let p = theme::palette(ui);
    let font = theme::semibold(theme::SIZE_ID);
    let digit_w = ui
        .painter()
        .layout_no_wrap("0".into(), font.clone(), p.text)
        .size()
        .x;
    let height = 54.0;
    let gap = digit_w * 0.42;
    let count = display.chars().filter(char::is_ascii_digit).count() as f32;
    let groups = display.split(' ').count().max(1) as f32;
    let width = digit_w * count + gap * (groups - 1.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let base = Id::new(("id-digits", display));
    let mut x = rect.min.x;
    let mut n = 0;
    for c in display.chars() {
        if c == ' ' {
            x += gap;
            continue;
        }
        let t = theme::entrance(ui, base, 0.05 + n as f32 * 0.035, 0.55);
        let cell = Rect::from_min_size(pos2(x, rect.min.y), vec2(digit_w, height));
        let rise = (1.0 - t) * height * 0.55;
        let painter = ui.painter().with_clip_rect(cell.expand2(vec2(2.0, 0.0)));
        painter.text(
            pos2(cell.center().x, cell.center().y + rise),
            Align2::CENTER_CENTER,
            c,
            font.clone(),
            p.text.gamma_multiply(t.max(0.0)),
        );
        x += digit_w;
        n += 1;
    }
    response.on_hover_text("Este é o ID deste computador");
}

// ------------------------------------------------------------------ home: connect

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Online,
    Connecting,
    Offline,
}

pub struct DeviceModel {
    pub id: String,
    pub name: String,
    pub system: String,
    pub customer: Option<String>,
    pub presence: Presence,
    pub this_computer: bool,
    /// "visto há 2 h" for offline computers.
    pub seen: Option<String>,
}

pub struct ConnectModel<'a> {
    pub signed_in: bool,
    pub signing_in: bool,
    pub error: Option<&'a str>,
    /// Online computers to offer right on Início (signed in).
    pub quick: &'a [DeviceModel],
    pub total: usize,
    /// The list could not be refreshed (no server).
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectAction {
    /// Connect to the typed ID (digits only).
    Connect(String),
    SignIn,
    Device(usize),
    ShowAll,
}

/// "Acessar outro computador": the ID field and, signed in, the computers online now.
pub fn connect_section(
    ui: &mut Ui,
    m: &ConnectModel<'_>,
    typed: &mut String,
) -> Option<ConnectAction> {
    let p = theme::palette(ui);
    let mut action = None;
    section_heading(
        ui,
        "Acessar outro computador",
        "Digite o ID que aparece no Aionix Connect da outra pessoa.",
    );
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let p = theme::palette(ui);
        let edit = egui::TextEdit::singleline(typed)
            .hint_text(RichText::new("000 000 000").color(p.faint))
            .font(theme::semibold(17.0))
            .desired_width(196.0)
            .min_size(vec2(220.0, 36.0))
            .vertical_align(Align::Center)
            .margin(Margin::symmetric(12, 7));
        let mut out = edit.show(ui);
        if out.response.changed() {
            let formatted = format_id_input(typed);
            if formatted != *typed {
                *typed = formatted;
                let end = egui::text::CCursor::new(typed.chars().count());
                out.state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(end)));
                out.state.store(ui.ctx(), out.response.id);
            }
        }
        let digits: String = typed.chars().filter(char::is_ascii_digit).collect();
        let ready = digits.len() == 9 && !digits.starts_with('0');
        let enter = out.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (theme::button(ui, Kind::Primary, None, "Conectar", ready, Some(112.0)).clicked()
            || (enter && ready))
            && ready
        {
            action = Some(ConnectAction::Connect(digits));
        }
    });
    if let Some(e) = m.error {
        ui.add_space(2.0);
        ui.label(RichText::new(e).size(13.0).color(p.danger));
    }
    if !m.signed_in {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Lock, p.faint);
            ui.label(
                RichText::new("Para acessar outro computador, entre com a sua conta.")
                    .size(13.0)
                    .color(p.muted),
            );
            if m.signing_in {
                ui.label(RichText::new("Entrando…").size(13.0).color(p.faint));
            } else if theme::link(ui, "Entrar").clicked() {
                action = Some(ConnectAction::SignIn);
            }
        });
    } else if !m.quick.is_empty() {
        ui.add_space(26.0);
        ui.horizontal(|ui| {
            let title = if m.stale {
                "Última lista conhecida"
            } else {
                "Disponíveis agora"
            };
            ui.label(theme::title(title, 13.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::link(ui, &format!("Ver todos ({})", m.total)).clicked() {
                    action = Some(ConnectAction::ShowAll);
                }
            });
        });
        ui.add_space(4.0);
        theme::hairline(ui);
        for (i, d) in m.quick.iter().enumerate() {
            if device_row(ui, d, false, m.stale, false) == RowAction::Connect {
                action = Some(ConnectAction::Device(i));
            }
        }
    }
    action
}

// ------------------------------------------------------------------ computers page

pub struct ComputersModel<'a> {
    pub devices: &'a [DeviceModel],
    pub total: usize,
    pub online: usize,
    pub loaded: bool,
    pub has_org: bool,
    pub error: Option<&'a str>,
    /// Owners and admins organize computers, clients and groups here.
    pub can_manage: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    None,
    Connect,
    Edit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputersAction {
    Connect(usize),
    Edit(usize),
    Organize,
}

/// The organization's computers as a list.
pub fn computers_page(
    ui: &mut Ui,
    m: &ComputersModel<'_>,
    filter: &mut String,
) -> Option<ComputersAction> {
    let p = theme::palette(ui);
    let mut pick = None;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(theme::title("Computadores", theme::SIZE_TITLE));
            let sub = if m.loaded {
                format!(
                    "{} {} · {} online",
                    m.total,
                    if m.total == 1 {
                        "computador"
                    } else {
                        "computadores"
                    },
                    m.online
                )
            } else {
                "Carregando…".into()
            };
            ui.label(RichText::new(sub).size(13.5).color(p.muted));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            search_field(ui, filter);
            if m.can_manage
                && theme::button(ui, Kind::Secondary, None, "Clientes e grupos", true, None)
                    .clicked()
            {
                pick = Some(ComputersAction::Organize);
            }
        });
    });
    ui.add_space(18.0);
    if let Some(e) = m.error {
        ui.label(
            RichText::new(format!("Não foi possível atualizar a lista: {e}"))
                .size(13.0)
                .color(p.danger),
        );
        ui.add_space(8.0);
    }
    if !m.loaded {
        for _ in 0..4 {
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                theme::skeleton(ui, vec2(10.0, 10.0));
                theme::skeleton(ui, vec2(180.0, 14.0));
                ui.add_space(40.0);
                theme::skeleton(ui, vec2(96.0, 14.0));
            });
            ui.add_space(14.0);
            theme::hairline(ui);
        }
        return None;
    }
    if !m.has_org {
        empty_state(
            ui,
            "Sua conta ainda não pertence a uma organização.",
            "Você pode conectar pelo ID em Início.",
        );
        return None;
    }
    if m.total == 0 {
        empty_state(
            ui,
            "Nenhum computador cadastrado ainda.",
            "Os computadores aparecem aqui quando instalam o Aionix Connect com a sua organização.",
        );
        return None;
    }
    if m.devices.is_empty() {
        empty_state(
            ui,
            "Nenhum computador encontrado.",
            "Procure pelo nome, pelo cliente ou pelo ID.",
        );
        return None;
    }
    column_header(ui);
    theme::hairline(ui);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, d) in m.devices.iter().enumerate() {
                match device_row(ui, d, true, m.error.is_some(), m.can_manage) {
                    RowAction::Connect => pick = Some(ComputersAction::Connect(i)),
                    RowAction::Edit => pick = Some(ComputersAction::Edit(i)),
                    RowAction::None => {}
                }
            }
        });
    pick
}

fn search_field(ui: &mut Ui, filter: &mut String) {
    let p = theme::palette(ui);
    let r = ui.add(
        egui::TextEdit::singleline(filter)
            .hint_text(RichText::new("Buscar").color(p.faint))
            .desired_width(200.0)
            .min_size(vec2(236.0, 34.0))
            .vertical_align(Align::Center)
            .margin(Margin {
                left: 34,
                right: 10,
                top: 7,
                bottom: 7,
            }),
    );
    let icon = Rect::from_center_size(
        pos2(r.rect.min.x + 18.0, r.rect.center().y),
        Vec2::splat(14.0),
    );
    icons::paint(ui.painter(), icon, Icon::Search, p.faint);
}

const COLS: [f32; 3] = [0.40, 0.20, 0.22];

fn column_header(ui: &mut Ui) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let w = rect.width();
    let mut x = rect.min.x + 26.0;
    for (i, label) in ["Nome", "ID", "Sistema"].iter().enumerate() {
        ui.painter().text(
            pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            *label,
            FontId::proportional(12.0),
            p.faint,
        );
        x += w * COLS[i];
    }
}

/// One computer; `columns` lays it out as a table row (Computadores) instead of the
/// compact Início row. Returns true when the user asks to connect.
/// `stale`: the list could not be refreshed (no server); presence is not shown as
/// current and connecting is not offered.
fn device_row(
    ui: &mut Ui,
    d: &DeviceModel,
    columns: bool,
    stale: bool,
    can_edit: bool,
) -> RowAction {
    let p = theme::palette(ui);
    let online = d.presence == Presence::Online && !stale;
    let height = if columns { 52.0 } else { 46.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let h = theme::toggle(ui, response.id.with("h"), response.hovered(), 0.12);
    if h > 0.0 {
        ui.painter().rect_filled(
            rect.shrink2(vec2(0.0, 2.0)),
            CornerRadius::same(8),
            p.surface_alt.gamma_multiply(h),
        );
    }
    ui.painter().hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0, p.hairline),
    );
    let cy = rect.center().y;
    let dot = match d.presence {
        _ if stale => p.faint,
        Presence::Online => p.success,
        Presence::Connecting => p.warning,
        Presence::Offline => p.faint,
    };
    let center = pos2(rect.min.x + 12.0, cy);
    if d.presence == Presence::Connecting && !stale && !theme::reduced_motion() {
        let t = (ui.input(|i| i.time) * 0.9).fract() as f32;
        ui.painter().circle_filled(
            center,
            4.0 + 5.0 * theme::ease_out(t),
            dot.gamma_multiply(0.35 * (1.0 - t)),
        );
        ui.ctx().request_repaint();
    }
    ui.painter().circle_filled(center, 4.0, dot);
    let text_color = if online { p.text } else { p.muted };
    let name_x = rect.min.x + 26.0;
    let name = ui
        .painter()
        .layout_no_wrap(d.name.clone(), theme::semibold(14.0), text_color);
    let name_w = name.size().x;
    if columns {
        ui.painter().galley(
            pos2(
                name_x,
                cy - name.size().y / 2.0 - if d.customer.is_some() { 8.0 } else { 0.0 },
            ),
            name,
            text_color,
        );
        if let Some(c) = &d.customer {
            ui.painter().text(
                pos2(name_x, cy + 9.0),
                Align2::LEFT_CENTER,
                c,
                FontId::proportional(12.0),
                p.faint,
            );
        }
        let w = rect.width();
        let id_x = rect.min.x + 26.0 + w * COLS[0];
        ui.painter().text(
            pos2(id_x, cy),
            Align2::LEFT_CENTER,
            &d.id,
            FontId::proportional(13.5),
            p.muted,
        );
        let sys_x = id_x + w * COLS[1];
        ui.painter().text(
            pos2(sys_x, cy),
            Align2::LEFT_CENTER,
            &d.system,
            FontId::proportional(13.0),
            p.muted,
        );
    } else {
        ui.painter()
            .galley(pos2(name_x, cy - name.size().y / 2.0), name, text_color);
        ui.painter().text(
            pos2(name_x + name_w + 12.0, cy),
            Align2::LEFT_CENTER,
            &d.id,
            FontId::proportional(13.0),
            p.faint,
        );
    }
    if d.this_computer {
        let (x, y) = if columns {
            (
                name_x + name_w + 10.0,
                if d.customer.is_some() { cy - 8.0 } else { cy },
            )
        } else {
            let id_w = ui
                .painter()
                .layout_no_wrap(d.id.clone(), FontId::proportional(13.0), p.faint)
                .size()
                .x;
            (name_x + name_w + 12.0 + id_w + 12.0, cy)
        };
        ui.painter().text(
            pos2(x, y),
            Align2::LEFT_CENTER,
            "este computador",
            FontId::proportional(12.0),
            p.accent_text,
        );
    }
    // Trailing, on one right edge: Conectar for online computers, the last-seen time
    // for the rest. The button's label ends where the text does (16 px padding).
    let mut clicked = false;
    let right = Rect::from_min_max(pos2(rect.max.x - 140.0, rect.min.y), rect.max);
    if d.presence == Presence::Online {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(right)
                .layout(Layout::right_to_left(Align::Center)),
        );
        let r = theme::button(&mut child, Kind::Quiet, None, "Conectar", !stale, None);
        clicked |= r
            .on_disabled_hover_text(
                "Sem conexão com o servidor. A lista volta quando a conexão voltar.",
            )
            .clicked()
            && !stale;
    } else {
        let text = match d.presence {
            Presence::Connecting => "conectando…".to_owned(),
            _ => d.seen.clone().unwrap_or_else(|| "offline".into()),
        };
        ui.painter().text(
            pos2(rect.max.x - 16.0, cy),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(12.5),
            p.faint,
        );
    }
    if online && response.double_clicked() {
        clicked = true;
    }
    let response = if online {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    };
    let mut action = if clicked {
        RowAction::Connect
    } else {
        RowAction::None
    };
    response.context_menu(|ui| {
        if online && ui.button("Conectar").clicked() {
            action = RowAction::Connect;
            ui.close();
        }
        if can_edit && ui.button("Editar computador…").clicked() {
            action = RowAction::Edit;
            ui.close();
        }
    });
    action
}

fn empty_state(ui: &mut Ui, title: &str, detail: &str) {
    let p = theme::palette(ui);
    ui.add_space(48.0);
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
        icons::paint(ui.painter(), r, Icon::Computers, p.faint);
        ui.add_space(6.0);
        ui.label(theme::title(title, 15.0));
        ui.label(RichText::new(detail).size(13.5).color(p.muted));
    });
}

// ------------------------------------------------------------------ sign-in sheet

pub struct LoginModel<'a> {
    pub working: Option<&'a str>,
    pub error: Option<&'a str>,
    /// ID that will be connected right after signing in.
    pub then_connect: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginAction {
    Submit,
    Cancel,
}

/// Centered sheet over a dimmed window; fades and rises into place.
pub fn login_sheet(
    ctx: &egui::Context,
    m: &LoginModel<'_>,
    email: &mut String,
    password: &mut String,
) -> Option<LoginAction> {
    let mut action = None;
    let id = Id::new("login-sheet");
    let t = if theme::reduced_motion() {
        1.0
    } else {
        let now = ctx.input(|i| i.time);
        let start = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
        let t = ((now - start) as f32 / 0.28).clamp(0.0, 1.0);
        if t < 1.0 {
            ctx.request_repaint();
        }
        theme::ease_out(t)
    };
    let dark = ctx.global_style().visuals.dark_mode;
    let p = if dark { theme::DARK } else { theme::LIGHT };
    let backdrop = Color32::from_black_alpha((if dark { 140.0 } else { 80.0 } * t) as u8);
    let area = egui::Modal::default_area(id.with("area"))
        .anchor(Align2::CENTER_CENTER, vec2(0.0, (1.0 - t) * 14.0));
    let response = egui::Modal::new(id.with("modal"))
        .area(area)
        .backdrop_color(backdrop)
        .frame(theme::sheet(&p).inner_margin(Margin::same(28)))
        .show(ctx, |ui| {
            ui.multiply_opacity(t);
            ui.set_width(340.0);
            let working = m.working.is_some();
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(theme::title("Entrar", theme::SIZE_TITLE));
            let detail = match &m.then_connect {
                Some(id) => format!("Depois de entrar, conectamos ao ID {id}."),
                None => "Use a conta Aionix criada pelo administrador.".into(),
            };
            ui.label(RichText::new(detail).size(13.5).color(p.muted));
            ui.add_space(14.0);
            ui.label(
                RichText::new("E-mail")
                    .size(theme::SIZE_SMALL)
                    .color(p.muted),
            );
            let (e, e_enter) = field(ui, email, "voce@empresa.com.br", 340.0, false, !working);
            ui.add_space(6.0);
            ui.label(
                RichText::new("Senha")
                    .size(theme::SIZE_SMALL)
                    .color(p.muted),
            );
            let (pw, pw_enter) = field(ui, password, "", 340.0, true, !working);
            if email.is_empty() && !e.has_focus() && !pw.has_focus() {
                e.request_focus();
            } else if !email.is_empty() && password.is_empty() && !e.has_focus() && !pw.has_focus()
            {
                pw.request_focus();
            }
            if let Some(err) = m.error {
                ui.add_space(2.0);
                ui.label(RichText::new(err).size(13.0).color(p.danger));
            }
            ui.add_space(16.0);
            let can = !working && !email.trim().is_empty() && !password.is_empty();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let label = m.working.unwrap_or("Entrar");
                if theme::button(ui, Kind::Primary, None, label, can, Some(104.0)).clicked()
                    || ((e_enter || pw_enter) && can)
                {
                    action = Some(LoginAction::Submit);
                }
                if theme::button(ui, Kind::Ghost, None, "Cancelar", !working, None).clicked() {
                    action = Some(LoginAction::Cancel);
                }
                if working {
                    spinner(ui, 16.0);
                }
            });
        });
    if response.should_close() && m.working.is_none() {
        action = Some(LoginAction::Cancel);
    }
    if action == Some(LoginAction::Cancel) {
        ctx.data_mut(|d| d.remove::<f64>(id));
    }
    action
}

/// Call when the sheet is not shown, so it animates in again next time.
pub fn reset_login_sheet(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<f64>(Id::new("login-sheet")));
}

// ------------------------------------------------- unattended access (ADR-0015)

pub struct UnattendedModel<'a> {
    /// The computer's name, as the sheet's text refers to it.
    pub name: &'a str,
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnattendedAction {
    /// Connect with the device password: nobody at the computer is asked.
    Connect,
    /// Connect the usual way: the person at the computer clicks "Permitir".
    AskPermission,
    Cancel,
}

/// Asks for the device's unattended-access password before connecting. The technician
/// may also choose the usual request, which keeps the person in control.
pub fn unattended_sheet(
    ctx: &egui::Context,
    m: &UnattendedModel<'_>,
    password: &mut String,
) -> Option<UnattendedAction> {
    let mut action = None;
    let id = Id::new("unattended-sheet");
    let t = sheet_progress(ctx, id);
    let dark = ctx.global_style().visuals.dark_mode;
    let p = if dark { theme::DARK } else { theme::LIGHT };
    let backdrop = Color32::from_black_alpha((if dark { 140.0 } else { 80.0 } * t) as u8);
    let area = egui::Modal::default_area(id.with("area"))
        .anchor(Align2::CENTER_CENTER, vec2(0.0, (1.0 - t) * 14.0));
    let response = egui::Modal::new(id.with("modal"))
        .area(area)
        .backdrop_color(backdrop)
        .frame(theme::sheet(&p).inner_margin(Margin::same(28)))
        .show(ctx, |ui| {
            ui.multiply_opacity(t);
            ui.set_width(360.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(theme::title("Conectar sem aceitação", theme::SIZE_TITLE));
            ui.label(
                RichText::new(format!(
                    "Digite a senha de acesso de {}. A conexão acontece sem que ninguém precise clicar em Permitir. O computador mostra o aviso enquanto você estiver conectado.",
                    m.name
                ))
                .size(13.5)
                .color(p.muted),
            );
            ui.add_space(14.0);
            ui.label(RichText::new("Senha de acesso").size(theme::SIZE_SMALL).color(p.muted));
            let (pw, enter) = field(ui, password, "", 360.0, true, true);
            if !pw.has_focus() && password.is_empty() {
                pw.request_focus();
            }
            if let Some(err) = m.error {
                ui.add_space(2.0);
                ui.label(RichText::new(err).size(13.0).color(p.danger));
            }
            ui.add_space(16.0);
            let can = !password.is_empty();
            ui.horizontal(|ui| {
                if theme::button(ui, Kind::Ghost, None, "Pedir permissão", true, None)
                    .on_hover_text("A pessoa do computador precisa clicar em Permitir")
                    .clicked()
                {
                    action = Some(UnattendedAction::AskPermission);
                }
            });
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if theme::button(ui, Kind::Primary, None, "Conectar", can, Some(120.0)).clicked()
                    || (enter && can)
                {
                    action = Some(UnattendedAction::Connect);
                }
                if theme::button(ui, Kind::Ghost, None, "Cancelar", true, None).clicked() {
                    action = Some(UnattendedAction::Cancel);
                }
            });
        });
    if response.should_close() {
        action = Some(UnattendedAction::Cancel);
    }
    if action.is_some() {
        ctx.data_mut(|d| d.remove::<f64>(id));
    }
    action
}

/// Call when the sheet is not shown, so it animates in again next time.
pub fn reset_unattended_sheet(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<f64>(Id::new("unattended-sheet")));
}

/// 0→1 entrance progress of a sheet, measured from the first frame it was shown.
fn sheet_progress(ctx: &egui::Context, id: Id) -> f32 {
    if theme::reduced_motion() {
        return 1.0;
    }
    let now = ctx.input(|i| i.time);
    let start = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    let t = ((now - start) as f32 / 0.28).clamp(0.0, 1.0);
    if t < 1.0 {
        ctx.request_repaint();
    }
    theme::ease_out(t)
}

// --------------------------------------------- organize computers (ADR-0015)

/// The editable fields of "Editar computador".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceFields {
    pub name: String,
    pub description: String,
    pub customer: Option<Uuid>,
    pub group: Option<Uuid>,
    pub assignee: Option<Uuid>,
    pub favorite: bool,
    /// The unattended-access password being typed; emptied once it is set.
    pub password: String,
}

/// An entry of a dropdown: its id and the name shown.
pub type Choice = (Uuid, String);

pub struct DeviceEditModel<'a> {
    /// "845 948 277" for the header.
    pub id_text: &'a str,
    pub customers: &'a [Choice],
    pub groups: &'a [Choice],
    pub members: &'a [Choice],
    pub password_set: bool,
    /// The owner allowed unattended access on the computer right now.
    pub unattended_enabled: bool,
    pub working: Option<&'a str>,
    pub error: Option<&'a str>,
    pub notice: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEditAction {
    Save,
    SetPassword,
    ClearPassword,
    Organize,
    Close,
}

/// Shows a centered sheet: `body` scrolls when it is taller than the window, `footer`
/// (the actions) always stays visible. Returns the first action asked for, and whether
/// the user asked to close the sheet (Escape or a click outside).
fn show_sheet<T>(
    ctx: &egui::Context,
    id: Id,
    width: f32,
    body: impl FnOnce(&mut Ui, &theme::Palette) -> Option<T>,
    footer: impl FnOnce(&mut Ui, &theme::Palette) -> Option<T>,
) -> (Option<T>, bool) {
    let t = sheet_progress(ctx, id);
    let dark = ctx.global_style().visuals.dark_mode;
    let p = if dark { theme::DARK } else { theme::LIGHT };
    let backdrop = Color32::from_black_alpha((if dark { 140.0 } else { 80.0 } * t) as u8);
    let area = egui::Modal::default_area(id.with("area"))
        .anchor(Align2::CENTER_CENTER, vec2(0.0, (1.0 - t) * 14.0));
    // Leaves room for the frame, the footer and the window's edges.
    let max_body = (ctx.content_rect().height() - 190.0).max(200.0);
    let (mut from_body, mut from_footer) = (None, None);
    let response = egui::Modal::new(id.with("modal"))
        .area(area)
        .backdrop_color(backdrop)
        .frame(theme::sheet(&p).inner_margin(Margin::same(24)))
        .show(ctx, |ui| {
            ui.multiply_opacity(t);
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 6.0;
            from_body = egui::ScrollArea::vertical()
                .max_height(max_body)
                .auto_shrink([false, true])
                .show(ui, |ui| body(ui, &p))
                .inner;
            ui.add_space(8.0);
            from_footer = footer(ui, &p);
        });
    (from_body.or(from_footer), response.should_close())
}

/// A dropdown over `choices`, with `none` for "no choice".
fn choice_box(
    ui: &mut Ui,
    salt: &str,
    current: &mut Option<Uuid>,
    none: &str,
    choices: &[Choice],
    width: f32,
) {
    let selected = current
        .and_then(|c| choices.iter().find(|(id, _)| *id == c))
        .map(|(_, name)| name.as_str())
        .unwrap_or(none);
    egui::ComboBox::from_id_salt(salt)
        .width(width)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(current, None, none);
            for (id, name) in choices {
                ui.selectable_value(current, Some(*id), name);
            }
        });
}

fn field_label(ui: &mut Ui, p: &theme::Palette, text: &str) {
    ui.label(RichText::new(text).size(theme::SIZE_SMALL).color(p.muted));
}

/// "Editar computador": two columns. Left: how the computer is organized. Right: its
/// unattended access (the owner's consent and the device password).
pub fn device_edit_sheet(
    ctx: &egui::Context,
    m: &DeviceEditModel<'_>,
    f: &mut DeviceFields,
) -> Option<DeviceEditAction> {
    let id = Id::new("device-edit-sheet");
    let busy = m.working.is_some();
    let save_ok = !busy && !f.name.trim().is_empty();
    let column = 286.0;
    let (action, close) = show_sheet(
        ctx,
        id,
        620.0,
        |ui, p| {
            let mut action = None;
            ui.label(theme::title("Editar computador", theme::SIZE_TITLE));
            ui.label(RichText::new(m.id_text).size(13.5).color(p.muted));
            ui.add_space(12.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 28.0;
                ui.vertical(|ui| {
                    ui.set_width(column);
                    field_label(ui, p, "Nome");
                    field(ui, &mut f.name, "Nome do computador", column, false, !busy);
                    field_label(ui, p, "Descrição");
                    field(ui, &mut f.description, "Ex.: servidor de arquivos", column, false, !busy);
                    field_label(ui, p, "Cliente");
                    ui.add_enabled_ui(!busy, |ui| {
                        choice_box(ui, "device-customer", &mut f.customer, "Sem cliente", m.customers, column);
                    });
                    field_label(ui, p, "Grupo");
                    ui.add_enabled_ui(!busy, |ui| {
                        choice_box(ui, "device-group", &mut f.group, "Sem grupo", m.groups, column);
                    });
                    field_label(ui, p, "Técnico responsável");
                    ui.add_enabled_ui(!busy, |ui| {
                        choice_box(ui, "device-assignee", &mut f.assignee, "Ninguém", m.members, column);
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        theme::toggle_chip(ui, &mut f.favorite, "Favorito", !busy);
                        if theme::button(ui, Kind::Ghost, None, "Clientes e grupos…", !busy, None)
                            .clicked()
                        {
                            action = Some(DeviceEditAction::Organize);
                        }
                    });
                });
                ui.vertical(|ui| {
                    ui.set_width(column);
                    ui.label(theme::title("Acesso sem aceitação", theme::SIZE_HEADING));
                    let (line, tone) = match (m.password_set, m.unattended_enabled) {
                        (true, true) => (
                            "Pronto: o dono deste computador permitiu e a senha está definida.",
                            p.success,
                        ),
                        (true, false) => (
                            "Senha definida. O dono ainda não permitiu o acesso sem aceitação neste computador.",
                            p.muted,
                        ),
                        (false, _) => ("Nenhuma senha definida.", p.muted),
                    };
                    ui.label(RichText::new(line).size(13.0).color(tone));
                    ui.add_space(8.0);
                    field_label(ui, p, "Nova senha (mínimo de 12 caracteres)");
                    let (_, enter) = field(ui, &mut f.password, "", column, true, !busy);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let set_ok = f.password.chars().count() >= 12 && !busy;
                        let set_clicked =
                            theme::button(ui, Kind::Secondary, None, "Definir senha", set_ok, None)
                                .clicked();
                        if set_clicked || (enter && set_ok) {
                            action = Some(DeviceEditAction::SetPassword);
                        }
                        if theme::button(
                            ui,
                            Kind::Ghost,
                            None,
                            "Remover senha",
                            m.password_set && !busy,
                            None,
                        )
                        .clicked()
                        {
                            action = Some(DeviceEditAction::ClearPassword);
                        }
                    });
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("O computador só aceita a senha enquanto o dono mantiver o acesso sem aceitação ligado.")
                            .size(12.5)
                            .color(p.faint),
                    );
                });
            });
            action
        },
        |ui, p| {
            let mut action = None;
            if let Some(e) = m.error {
                ui.label(RichText::new(e).size(13.0).color(p.danger));
            } else if let Some(n) = m.notice {
                ui.label(RichText::new(n).size(13.0).color(p.success));
            } else if let Some(w) = m.working {
                ui.label(RichText::new(w).size(13.0).color(p.muted));
            }
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if theme::button(ui, Kind::Primary, None, "Salvar", save_ok, Some(104.0)).clicked()
                {
                    action = Some(DeviceEditAction::Save);
                }
                if theme::button(ui, Kind::Ghost, None, "Fechar", !busy, None).clicked() {
                    action = Some(DeviceEditAction::Close);
                }
            });
            action
        },
    );
    let action = match action {
        Some(a) => Some(a),
        None if close && !busy => Some(DeviceEditAction::Close),
        None => None,
    };
    if action == Some(DeviceEditAction::Close) {
        ctx.data_mut(|d| d.remove::<f64>(id));
    }
    action
}

/// Call when the device sheet is not shown, so it animates in again next time.
pub fn reset_device_edit_sheet(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<f64>(Id::new("device-edit-sheet")));
}

pub struct OrganizeModel<'a> {
    /// (id, name, number of computers).
    pub customers: &'a [(Uuid, String, i64)],
    pub groups: &'a [(Uuid, String, i64)],
    pub working: Option<&'a str>,
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrganizeAction {
    AddCustomer,
    AddGroup,
    DeleteCustomer(Uuid),
    DeleteGroup(Uuid),
    Close,
}

/// One list of the organize sheet: customers or groups.
struct OrganizeSection<'a> {
    title: &'a str,
    rows: &'a [(Uuid, String, i64)],
    new: &'a mut String,
    hint: &'a str,
    add: OrganizeAction,
    del: fn(Uuid) -> OrganizeAction,
}

/// "Clientes e grupos": the organization's customers and device groups.
pub fn organize_sheet(
    ctx: &egui::Context,
    m: &OrganizeModel<'_>,
    new_customer: &mut String,
    new_group: &mut String,
) -> Option<OrganizeAction> {
    let id = Id::new("organize-sheet");
    let armed_id = Id::new("organize-armed");
    let busy = m.working.is_some();
    // The row whose delete waits for a second click (deleting unlinks its computers).
    let armed: Option<Uuid> = ctx.data(|d| d.get_temp::<Option<Uuid>>(armed_id)).flatten();
    let mut arm: Option<Option<Uuid>> = None;
    let (action, close) = show_sheet(
        ctx,
        id,
        420.0,
        |ui, p| {
            let mut action = None;
            ui.label(theme::title("Clientes e grupos", theme::SIZE_TITLE));
            ui.label(
                RichText::new(
                    "Clientes são quem você atende. Grupos organizam os computadores em pastas.",
                )
                .size(13.0)
                .color(p.muted),
            );
            let sections = [
                OrganizeSection {
                    title: "Clientes",
                    rows: m.customers,
                    new: &mut *new_customer,
                    hint: "Nome do cliente",
                    add: OrganizeAction::AddCustomer,
                    del: OrganizeAction::DeleteCustomer,
                },
                OrganizeSection {
                    title: "Grupos",
                    rows: m.groups,
                    new: &mut *new_group,
                    hint: "Nome do grupo",
                    add: OrganizeAction::AddGroup,
                    del: OrganizeAction::DeleteGroup,
                },
            ];
            for OrganizeSection {
                title,
                rows,
                new,
                hint,
                add,
                del,
            } in sections
            {
                ui.add_space(10.0);
                ui.label(theme::title(title, theme::SIZE_HEADING));
                if rows.is_empty() {
                    ui.label(
                        RichText::new("Nenhum cadastrado ainda.")
                            .size(13.0)
                            .color(p.faint),
                    );
                }
                for (rid, name, count) in rows {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(name).size(14.0).color(p.text));
                        ui.label(
                            RichText::new(format!("{count} computador(es)"))
                                .size(12.5)
                                .color(p.faint),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let armed_here = armed == Some(*rid);
                            let label = if armed_here {
                                "Confirmar exclusão"
                            } else {
                                "Excluir"
                            };
                            let btn = theme::button(ui, Kind::Ghost, None, label, !busy, None)
                                .on_hover_text(
                                    "Os computadores continuam cadastrados, sem este vínculo.",
                                );
                            if btn.clicked() {
                                if armed_here {
                                    action = Some(del(*rid));
                                } else {
                                    arm = Some(Some(*rid));
                                }
                            }
                        });
                    });
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let (_, enter) = field(ui, new, hint, 280.0, false, !busy);
                    let ok = !new.trim().is_empty() && !busy;
                    if theme::button(ui, Kind::Secondary, None, "Adicionar", ok, None).clicked()
                        || (enter && ok)
                    {
                        action = Some(add);
                    }
                });
            }
            action
        },
        |ui, p| {
            if let Some(e) = m.error {
                ui.label(RichText::new(e).size(13.0).color(p.danger));
            } else if let Some(w) = m.working {
                ui.label(RichText::new(w).size(13.0).color(p.muted));
            }
            let mut action = None;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::button(ui, Kind::Ghost, None, "Fechar", !busy, None).clicked() {
                    action = Some(OrganizeAction::Close);
                }
            });
            action
        },
    );
    if let Some(a) = arm {
        ctx.data_mut(|d| d.insert_temp(armed_id, a));
    }
    if matches!(
        action,
        Some(OrganizeAction::DeleteCustomer(_) | OrganizeAction::DeleteGroup(_))
    ) {
        ctx.data_mut(|d| d.insert_temp::<Option<Uuid>>(armed_id, None));
    }
    let action = match action {
        Some(a) => Some(a),
        None if close && !busy => Some(OrganizeAction::Close),
        None => None,
    };
    if action == Some(OrganizeAction::Close) {
        ctx.data_mut(|d| d.remove::<f64>(id));
    }
    action
}

/// Call when the organize sheet is not shown, so it animates in again next time.
pub fn reset_organize_sheet(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<f64>(Id::new("organize-sheet")));
}

// ------------------------------------------------------------------ session

pub struct ToolbarModel<'a> {
    pub title: &'a str,
    pub id: &'a str,
    pub state: &'a str,
    pub tone: Tone,
    pub pulse: bool,
    pub metrics: Option<String>,
    pub terminal: bool,
    pub can_control: bool,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    Leave,
    End,
    Retry,
    Fullscreen,
    /// Control was switched off (release held keys).
    ControlOff,
}

pub const TOOLBAR_HEIGHT: f32 = 52.0;

/// The session toolbar (top strip, never over the remote image).
pub fn session_toolbar(
    ui: &mut Ui,
    m: &ToolbarModel<'_>,
    control: &mut bool,
    diagnostics: &mut bool,
) -> Option<ToolbarAction> {
    let p = theme::palette(ui);
    let mut action = None;
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, p.sidebar);
    ui.painter().hline(
        full.x_range(),
        full.max.y - 0.5,
        Stroke::new(1.0, p.hairline),
    );
    let inner = full.shrink2(vec2(12.0, 0.0));
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let ui = &mut ui;
    ui.spacing_mut().item_spacing.x = 10.0;
    if m.terminal {
        if theme::button(
            ui,
            Kind::Ghost,
            Some(Icon::ChevronLeft),
            "Início",
            true,
            None,
        )
        .clicked()
        {
            action = Some(ToolbarAction::Leave);
        }
    } else {
        ui.add_space(4.0);
    }
    ui.allocate_ui_with_layout(vec2(220.0, 40.0), Layout::top_down(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_space(3.0);
        ui.add(egui::Label::new(theme::title(m.title, 14.0)).truncate());
        ui.label(
            RichText::new(format!("ID {}", m.id))
                .size(theme::SIZE_CAPTION)
                .color(p.faint),
        );
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        theme::status_dot(ui, m.tone.color(&p), m.pulse);
        ui.label(RichText::new(m.state).size(13.0).color(p.muted));
    });
    if let Some(metrics) = &m.metrics {
        ui.label(RichText::new(metrics).size(12.5).color(p.faint));
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if m.terminal {
            if theme::button(ui, Kind::Primary, None, "Conectar novamente", true, None).clicked() {
                action = Some(ToolbarAction::Retry);
            }
        } else if theme::button(ui, Kind::Danger, None, "Encerrar", true, None).clicked() {
            action = Some(ToolbarAction::End);
        }
        ui.add_space(8.0);
        let (fs_icon, fs_tip) = if m.fullscreen {
            (Icon::ExitFullscreen, "Sair da tela cheia")
        } else {
            (Icon::Fullscreen, "Tela cheia")
        };
        if theme::icon_button(ui, fs_icon, fs_tip, false, true).clicked() {
            action = Some(ToolbarAction::Fullscreen);
        }
        if theme::icon_button(ui, Icon::Pulse, "Diagnóstico da sessão", *diagnostics, true)
            .clicked()
        {
            *diagnostics = !*diagnostics;
        }
        let tip = if m.can_control {
            if *control {
                "Controle ligado: mouse e teclado vão para o computador remoto"
            } else {
                "Ligar controle (mouse e teclado)"
            }
        } else {
            "Sem permissão de controle"
        };
        if theme::icon_button(
            ui,
            Icon::Pointer,
            tip,
            *control && m.can_control,
            m.can_control && !m.terminal,
        )
        .clicked()
        {
            *control = !*control;
            if !*control {
                action = Some(ToolbarAction::ControlOff);
            }
        }
    });
    action
}

/// What the session area shows when there is no picture.
pub struct OverlayModel<'a> {
    pub headline: &'a str,
    pub detail: &'a str,
    pub busy: bool,
    pub tone: Tone,
}

/// Centered status sheet over the session area.
pub fn session_overlay(ui: &mut Ui, area: Rect, m: &OverlayModel<'_>) {
    let p = theme::palette(ui);
    let t = theme::entrance(ui, Id::new(("overlay", m.headline)), 0.0, 0.3);
    let size = vec2(400.0, 176.0);
    let rect = Rect::from_center_size(area.center() + vec2(0.0, (1.0 - t) * 8.0), size);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Center)),
    );
    child.multiply_opacity(t);
    theme::sheet(&p)
        .inner_margin(Margin::same(28))
        .show(&mut child, |ui| {
            ui.set_width(size.x - 56.0);
            ui.vertical_centered(|ui| {
                if m.busy {
                    spinner(ui, 26.0);
                } else {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    let c = m.tone.color(&p);
                    ui.painter()
                        .circle_filled(r.center(), 13.0, c.gamma_multiply(0.15));
                    ui.painter().circle_filled(r.center(), 4.5, c);
                }
                ui.add_space(8.0);
                ui.label(theme::title(m.headline, 17.0));
                if !m.detail.is_empty() {
                    ui.label(RichText::new(m.detail).size(13.5).color(p.muted));
                }
            });
        });
}

/// Thin notice under the toolbar (local self-test).
pub fn session_notice(ui: &mut Ui, text: &str) {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.warning.gamma_multiply(0.12))
        .inner_margin(Margin::symmetric(16, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).size(13.0).color(p.warning));
        });
}

// ------------------------------------------------------------------ window layout

/// Window layout: the sidebar on the left and the page on the right. A page change
/// fades the new page in while it rises 6 px into place.
pub fn shell(
    ui: &mut Ui,
    side: &SidebarModel<'_>,
    page: impl FnOnce(&mut Ui),
) -> Option<SidebarAction> {
    let p = theme::palette(ui);
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, p.bg);
    let side_rect = Rect::from_min_size(full.min, vec2(SIDEBAR_WIDTH, full.height()));
    let content = Rect::from_min_max(pos2(full.min.x + SIDEBAR_WIDTH, full.min.y), full.max);
    let action = sidebar(
        &mut ui.new_child(UiBuilder::new().max_rect(side_rect)),
        side,
    );

    let key = Id::new("page-transition");
    let shown: Option<Page> = ui.ctx().data(|d| d.get_temp(key.with("page")));
    if shown != Some(side.page) {
        ui.ctx()
            .data_mut(|d| d.insert_temp(key.with("page"), side.page));
        theme::restart(ui, key);
    }
    let t = theme::entrance(ui, key, 0.0, 0.32);
    let inner = content
        .shrink2(vec2(40.0, 0.0))
        .with_min_y(content.min.y + 32.0)
        .translate(vec2(0.0, (1.0 - t) * 6.0));
    let mut page_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    page_ui.multiply_opacity(t);
    page(&mut page_ui);
    ui.allocate_rect(full, Sense::hover());
    action
}

/// Início: banners, this computer's ID, then `connect` (connecting to another
/// computer) and `footer` (technical details).
pub fn home_page(
    ui: &mut Ui,
    host: &HostModel,
    connect: impl FnOnce(&mut Ui),
    footer: impl FnOnce(&mut Ui),
) -> Option<HostAction> {
    let mut out = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(760.0));
            if let Some(a) = inbound_banner(ui, host) {
                out = Some(a);
            }
            if let Some(a) = this_computer(ui, host) {
                out = Some(a);
            }
            ui.add_space(30.0);
            theme::hairline(ui);
            ui.add_space(26.0);
            connect(ui);
            ui.add_space(36.0);
            footer(ui);
            ui.add_space(24.0);
        });
    out
}

// ------------------------------------------------------------------ sheets

/// A centered sheet over a dimmed window that fades and rises into place, as the
/// sign-in sheet does. Returns what `add` returned and whether the user dismissed it
/// (Esc or a click outside).
pub fn modal_sheet<R>(
    ctx: &egui::Context,
    key: &str,
    width: f32,
    add: impl FnOnce(&mut Ui, &Palette) -> R,
) -> (R, bool) {
    let id = Id::new(("sheet", key));
    let t = if theme::reduced_motion() {
        1.0
    } else {
        let now = ctx.input(|i| i.time);
        let start = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
        let t = ((now - start) as f32 / 0.28).clamp(0.0, 1.0);
        if t < 1.0 {
            ctx.request_repaint();
        }
        theme::ease_out(t)
    };
    let dark = ctx.global_style().visuals.dark_mode;
    let p = if dark { theme::DARK } else { theme::LIGHT };
    let backdrop = Color32::from_black_alpha((if dark { 140.0 } else { 80.0 } * t) as u8);
    let area = egui::Modal::default_area(id.with("area"))
        .anchor(Align2::CENTER_CENTER, vec2(0.0, (1.0 - t) * 14.0));
    let response = egui::Modal::new(id.with("modal"))
        .area(area)
        .backdrop_color(backdrop)
        .frame(theme::sheet(&p).inner_margin(Margin::same(28)))
        .show(ctx, |ui| {
            ui.multiply_opacity(t);
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui, &p)
        });
    let dismissed = response.should_close();
    (response.inner, dismissed)
}

/// Forget a sheet's entrance, so it animates in again next time it opens.
pub fn reset_sheet(ctx: &egui::Context, key: &str) {
    ctx.data_mut(|d| d.remove::<f64>(Id::new(("sheet", key))));
}
