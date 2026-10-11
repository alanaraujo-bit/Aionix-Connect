//! The administration page (owners and admins only): accounts, sessions and history,
//! plus the sheets it opens, and the password sheets every account uses. Same pattern as
//! views.rs: small models in, the user's request out. The server is the real gate; these
//! screens only offer what it will accept.

use aionix_control_api::rest::OrgRole;
use egui::{
    Align, Align2, CornerRadius, FontId, Id, Layout, Margin, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder, Vec2, pos2, vec2,
};

use crate::icons::{self, Icon};
use crate::theme::{self, Kind};
use crate::views::{self, Tone};

// ------------------------------------------------------------------ models

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdminTab {
    #[default]
    Accounts,
    Sessions,
    History,
}

pub fn role_label(r: OrgRole) -> &'static str {
    match r {
        OrgRole::Owner => "Dono",
        OrgRole::Admin => "Administrador",
        OrgRole::Technician => "Técnico",
        OrgRole::Viewer => "Visualizador",
    }
}

fn role_hint(r: OrgRole) -> &'static str {
    match r {
        OrgRole::Owner => "Controle total, inclusive de outros donos.",
        OrgRole::Admin => "Acessa computadores e gerencia contas e sessões.",
        OrgRole::Technician => "Acessa os computadores pelo ID.",
        OrgRole::Viewer => "Só vê a lista de computadores.",
    }
}

pub struct AccountRow {
    pub name: String,
    pub email: String,
    pub role: OrgRole,
    pub disabled: bool,
    /// Still on a temporary password.
    pub temporary: bool,
    /// "hoje às 09:12", "nunca".
    pub last_login: String,
    pub is_you: bool,
    /// The caller may act on this account (see the server rules).
    pub manageable: bool,
}

pub struct SessionRow {
    pub host: String,
    pub host_id: String,
    pub technician: String,
    pub state: String,
    pub tone: Tone,
    pub live: bool,
    /// "hoje às 09:12 · 14 min".
    pub when: String,
}

pub struct HistoryRow {
    /// "11/10 09:12".
    pub when: String,
    pub text: String,
    pub tone: Tone,
}

pub struct AdminModel<'a> {
    pub tab: AdminTab,
    pub org_name: &'a str,
    pub accounts: Option<&'a [AccountRow]>,
    pub sessions: Option<&'a [SessionRow]>,
    pub history: Option<&'a [HistoryRow]>,
    pub error: Option<&'a str>,
    pub loading: bool,
    /// Roles the caller may give (a new account or a role change).
    pub assignable: &'a [OrgRole],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAction {
    Tab(AdminTab),
    Refresh,
    NewAccount,
    Role(usize, OrgRole),
    Reset(usize),
    SignOut(usize),
    Disable(usize),
    Enable(usize),
    EndSession(usize),
}

// ------------------------------------------------------------------ page

pub fn admin_page(ui: &mut Ui, m: &AdminModel<'_>) -> Option<AdminAction> {
    let p = theme::palette(ui);
    let mut action = None;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(theme::title("Administração", theme::SIZE_TITLE));
            let sub = match (m.tab, m.accounts) {
                (AdminTab::Accounts, Some(a)) => format!(
                    "{} · {} {}",
                    m.org_name,
                    a.len(),
                    if a.len() == 1 { "conta" } else { "contas" }
                ),
                _ => m.org_name.to_owned(),
            };
            ui.label(RichText::new(sub).size(13.5).color(p.muted));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if m.tab == AdminTab::Accounts
                && theme::button(
                    ui,
                    Kind::Primary,
                    Some(Icon::Plus),
                    "Nova conta",
                    true,
                    None,
                )
                .clicked()
            {
                action = Some(AdminAction::NewAccount);
            }
            if m.loading {
                views::spinner(ui, 16.0);
            } else if theme::icon_button(ui, Icon::Refresh, "Atualizar", false, true).clicked() {
                action = Some(AdminAction::Refresh);
            }
        });
    });
    ui.add_space(14.0);
    if let Some(t) = tabs(ui, m.tab) {
        action = Some(AdminAction::Tab(t));
    }
    ui.add_space(6.0);
    if let Some(e) = m.error {
        ui.add_space(6.0);
        ui.label(RichText::new(e).size(13.0).color(p.danger));
    }
    let body = match m.tab {
        AdminTab::Accounts => accounts(ui, m),
        AdminTab::Sessions => sessions(ui, m),
        AdminTab::History => history(ui, m),
    };
    body.or(action)
}

/// Underlined text tabs; the underline slides to the selected one.
fn tabs(ui: &mut Ui, current: AdminTab) -> Option<AdminTab> {
    let p = theme::palette(ui);
    let mut picked = None;
    let items = [
        (AdminTab::Accounts, "Contas"),
        (AdminTab::Sessions, "Sessões"),
        (AdminTab::History, "Histórico"),
    ];
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
    ui.painter()
        .hline(bar.x_range(), bar.max.y - 0.5, Stroke::new(1.0, p.hairline));
    let mut x = bar.min.x;
    let mut selected_span = (bar.min.x, bar.min.x);
    for (tab, label) in items {
        let selected = tab == current;
        let font = if selected {
            theme::semibold(14.0)
        } else {
            FontId::proportional(14.0)
        };
        let galley = ui.painter().layout_no_wrap(label.to_owned(), font, p.text);
        let w = galley.size().x;
        let rect = Rect::from_min_size(pos2(x, bar.min.y), vec2(w + 4.0, bar.height()));
        let r = ui.interact(rect, ui.id().with(("admin-tab", label)), Sense::click());
        let h = theme::toggle(ui, r.id.with("h"), r.hovered(), 0.12);
        let ink = if selected {
            p.text
        } else {
            theme::lerp_color(p.muted, p.text, h)
        };
        ui.painter().galley(
            pos2(x + 2.0, bar.center().y - galley.size().y / 2.0 - 1.0),
            galley,
            ink,
        );
        if selected {
            selected_span = (x, x + w + 4.0);
        }
        if r.clicked() && !selected {
            picked = Some(tab);
        }
        r.on_hover_cursor(egui::CursorIcon::PointingHand);
        x += w + 28.0;
    }
    let ctx = ui.ctx().clone();
    let a = ctx.animate_value_with_time(
        Id::new("admin-tab-a"),
        selected_span.0,
        if theme::reduced_motion() { 0.0 } else { 0.22 },
    );
    let b = ctx.animate_value_with_time(
        Id::new("admin-tab-b"),
        selected_span.1,
        if theme::reduced_motion() { 0.0 } else { 0.22 },
    );
    ui.painter().rect_filled(
        Rect::from_min_max(pos2(a, bar.max.y - 2.0), pos2(b, bar.max.y)),
        CornerRadius::same(1),
        p.accent,
    );
    picked
}

fn loading_rows(ui: &mut Ui) {
    for _ in 0..4 {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            theme::skeleton(ui, vec2(180.0, 14.0));
            ui.add_space(60.0);
            theme::skeleton(ui, vec2(90.0, 14.0));
        });
        ui.add_space(14.0);
        theme::hairline(ui);
    }
}

fn empty(ui: &mut Ui, title: &str, detail: &str) {
    let p = theme::palette(ui);
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(theme::title(title, 15.0));
        ui.label(RichText::new(detail).size(13.5).color(p.muted));
    });
}

fn header(ui: &mut Ui, cols: &[(&str, f32)]) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    for (label, at) in cols {
        ui.painter().text(
            pos2(rect.min.x + rect.width() * at, rect.center().y),
            Align2::LEFT_CENTER,
            *label,
            FontId::proportional(12.0),
            p.faint,
        );
    }
    ui.painter().hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0, p.hairline),
    );
}

/// A table row of `height` with hover fill and a hairline; returns its rect.
fn row(ui: &mut Ui, height: f32) -> (Rect, egui::Response, f32) {
    let p = theme::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
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
    (rect, response, h)
}

const ACC_COLS: [f32; 3] = [0.0, 0.44, 0.64];

fn accounts(ui: &mut Ui, m: &AdminModel<'_>) -> Option<AdminAction> {
    let p = theme::palette(ui);
    let Some(rows) = m.accounts else {
        loading_rows(ui);
        return None;
    };
    if rows.is_empty() {
        empty(ui, "Nenhuma conta ainda.", "Crie a primeira em Nova conta.");
        return None;
    }
    let mut action = None;
    header(
        ui,
        &[
            ("Nome", ACC_COLS[0] + 0.0),
            ("Papel", ACC_COLS[1]),
            ("Situação", ACC_COLS[2]),
        ],
    );
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, a) in rows.iter().enumerate() {
                let (rect, _, _) = row(ui, 58.0);
                let w = rect.width();
                let cy = rect.center().y;
                let ink = if a.disabled { p.muted } else { p.text };
                let name = ui
                    .painter()
                    .layout_no_wrap(a.name.clone(), theme::semibold(14.0), ink);
                let name_w = name.size().x;
                let x0 = rect.min.x + 4.0;
                ui.painter().galley(pos2(x0, cy - 17.0), name, ink);
                if a.is_you {
                    ui.painter().text(
                        pos2(x0 + name_w + 8.0, cy - 9.0),
                        Align2::LEFT_CENTER,
                        "você",
                        FontId::proportional(12.0),
                        p.accent_text,
                    );
                }
                ui.painter().text(
                    pos2(x0, cy + 9.0),
                    Align2::LEFT_CENTER,
                    &a.email,
                    FontId::proportional(12.5),
                    p.faint,
                );
                ui.painter().text(
                    pos2(rect.min.x + w * ACC_COLS[1], cy),
                    Align2::LEFT_CENTER,
                    role_label(a.role),
                    FontId::proportional(13.5),
                    p.muted,
                );
                let (state, dot) = if a.disabled {
                    ("Desativada", p.faint)
                } else if a.temporary {
                    ("Senha temporária", p.warning)
                } else {
                    ("Ativa", p.success)
                };
                let sx = rect.min.x + w * ACC_COLS[2];
                ui.painter()
                    .circle_filled(pos2(sx + 4.0, cy - 8.0), 4.0, dot);
                ui.painter().text(
                    pos2(sx + 14.0, cy - 8.0),
                    Align2::LEFT_CENTER,
                    state,
                    FontId::proportional(13.5),
                    p.muted,
                );
                ui.painter().text(
                    pos2(sx + 14.0, cy + 10.0),
                    Align2::LEFT_CENTER,
                    format!("Último acesso: {}", a.last_login),
                    FontId::proportional(12.0),
                    p.faint,
                );
                if a.manageable && !a.is_you {
                    let r = Rect::from_center_size(pos2(rect.max.x - 22.0, cy), Vec2::splat(32.0));
                    let mut child = ui.new_child(UiBuilder::new().max_rect(r));
                    let button = theme::icon_button(&mut child, Icon::More, "Ações", false, true);
                    egui::Popup::menu(&button)
                        .align(egui::RectAlign::BOTTOM_END)
                        .gap(4.0)
                        .show(|ui| {
                            ui.set_min_width(250.0);
                            for &role in m.assignable {
                                if role != a.role
                                    && menu_item(
                                        ui,
                                        Icon::User,
                                        &format!("Tornar {}", role_label(role).to_lowercase()),
                                        false,
                                    )
                                {
                                    action = Some(AdminAction::Role(i, role));
                                }
                            }
                            menu_separator(ui);
                            if menu_item(ui, Icon::Key, "Redefinir senha", false) {
                                action = Some(AdminAction::Reset(i));
                            }
                            if menu_item(
                                ui,
                                Icon::SignOut,
                                "Desconectar de todos os dispositivos",
                                false,
                            ) {
                                action = Some(AdminAction::SignOut(i));
                            }
                            menu_separator(ui);
                            if a.disabled {
                                if menu_item(ui, Icon::Check, "Reativar conta", false) {
                                    action = Some(AdminAction::Enable(i));
                                }
                            } else if menu_item(ui, Icon::Lock, "Desativar conta", true) {
                                action = Some(AdminAction::Disable(i));
                            }
                        });
                }
            }
        });
    action
}

/// A menu entry; `danger` paints it red. Closes the menu when clicked.
fn menu_item(ui: &mut Ui, icon: Icon, text: &str, danger: bool) -> bool {
    let p = theme::palette(ui);
    let (rect, r) =
        ui.allocate_exact_size(vec2(ui.available_width().max(230.0), 32.0), Sense::click());
    let h = theme::toggle(ui, r.id.with("h"), r.hovered(), 0.1);
    if h > 0.0 {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), p.surface_alt.gamma_multiply(h));
    }
    let ink = if danger { p.danger } else { p.text };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(rect.min.x + 18.0, rect.center().y), Vec2::splat(14.0)),
        icon,
        if danger { p.danger } else { p.muted },
    );
    ui.painter().text(
        pos2(rect.min.x + 36.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.5),
        ink,
    );
    let clicked = r.clicked();
    if clicked {
        ui.close();
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand);
    clicked
}

fn menu_separator(ui: &mut Ui) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, p.hairline),
    );
}

fn sessions(ui: &mut Ui, m: &AdminModel<'_>) -> Option<AdminAction> {
    let p = theme::palette(ui);
    let Some(rows) = m.sessions else {
        loading_rows(ui);
        return None;
    };
    ui.add_space(4.0);
    ui.label(
        RichText::new("As sessões em andamento aparecem primeiro. Encerrar exige o Aionix Connect 0.2.6 ou mais novo no computador acessado.")
            .size(12.5)
            .color(p.faint),
    );
    ui.add_space(4.0);
    if rows.is_empty() {
        empty(
            ui,
            "Nenhuma sessão ainda.",
            "Os acessos feitos pelas contas desta organização aparecem aqui.",
        );
        return None;
    }
    let mut action = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, s) in rows.iter().enumerate() {
                let (rect, _, _) = row(ui, 58.0);
                let cy = rect.center().y;
                let dot = s.tone.color(&p);
                let c = pos2(rect.min.x + 10.0, cy - 8.0);
                if s.live && s.tone == Tone::Warning && !theme::reduced_motion() {
                    let t = (ui.input(|i| i.time) * 0.9).fract() as f32;
                    ui.painter().circle_filled(
                        c,
                        4.0 + 5.0 * theme::ease_out(t),
                        dot.gamma_multiply(0.35 * (1.0 - t)),
                    );
                    ui.ctx().request_repaint();
                }
                ui.painter().circle_filled(c, 4.0, dot);
                let x0 = rect.min.x + 24.0;
                let host =
                    ui.painter()
                        .layout_no_wrap(s.host.clone(), theme::semibold(14.0), p.text);
                let hw = host.size().x;
                ui.painter().galley(pos2(x0, cy - 17.0), host, p.text);
                ui.painter().text(
                    pos2(x0 + hw + 10.0, cy - 8.0),
                    Align2::LEFT_CENTER,
                    format!("ID {}", s.host_id),
                    FontId::proportional(12.5),
                    p.faint,
                );
                ui.painter().text(
                    pos2(x0, cy + 9.0),
                    Align2::LEFT_CENTER,
                    format!("{}  ·  por {}  ·  {}", s.state, s.technician, s.when),
                    FontId::proportional(12.5),
                    p.muted,
                );
                if s.live {
                    let r = Rect::from_min_max(pos2(rect.max.x - 140.0, rect.min.y), rect.max);
                    let mut child = ui.new_child(
                        UiBuilder::new()
                            .max_rect(r)
                            .layout(Layout::right_to_left(Align::Center)),
                    );
                    if theme::button(&mut child, Kind::QuietDanger, None, "Encerrar", true, None)
                        .clicked()
                    {
                        action = Some(AdminAction::EndSession(i));
                    }
                }
            }
        });
    action
}

fn history(ui: &mut Ui, m: &AdminModel<'_>) -> Option<AdminAction> {
    let p = theme::palette(ui);
    let Some(rows) = m.history else {
        loading_rows(ui);
        return None;
    };
    if rows.is_empty() {
        empty(
            ui,
            "Nada registrado ainda.",
            "Logins, contas criadas e acessos aparecem aqui.",
        );
        return None;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for h in rows {
                let (rect, _, _) = row(ui, 42.0);
                let cy = rect.center().y;
                ui.painter().text(
                    pos2(rect.min.x + 4.0, cy),
                    Align2::LEFT_CENTER,
                    &h.when,
                    FontId::proportional(12.5),
                    p.faint,
                );
                ui.painter()
                    .circle_filled(pos2(rect.min.x + 104.0, cy), 3.5, h.tone.color(&p));
                let text_rect = Rect::from_min_max(
                    pos2(rect.min.x + 118.0, rect.min.y),
                    pos2(rect.max.x - 4.0, rect.max.y),
                );
                let galley = ui.painter().layout(
                    h.text.clone(),
                    FontId::proportional(13.5),
                    p.text,
                    text_rect.width(),
                );
                ui.painter().with_clip_rect(text_rect).galley(
                    pos2(text_rect.min.x, cy - galley.size().y.min(20.0) / 2.0),
                    galley,
                    p.text,
                );
            }
        });
    None
}

// ------------------------------------------------------------------ sheets

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetAction {
    Submit,
    Cancel,
}

pub struct NewAccountForm {
    pub name: String,
    pub email: String,
    pub role: OrgRole,
}

impl Default for NewAccountForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            email: String::new(),
            role: OrgRole::Technician,
        }
    }
}

/// "Nova conta": name, e-mail and role. The server creates the temporary password.
pub fn new_account_sheet(
    ctx: &egui::Context,
    form: &mut NewAccountForm,
    roles: &[OrgRole],
    working: bool,
    error: Option<&str>,
) -> Option<SheetAction> {
    let (action, dismissed) = views::modal_sheet(ctx, "new-account", 380.0, |ui, p| {
        let mut action = None;
        ui.label(theme::title("Nova conta", theme::SIZE_TITLE));
        ui.label(
            RichText::new(
                "A pessoa recebe uma senha temporária e cria a própria no primeiro acesso.",
            )
            .size(13.5)
            .color(p.muted),
        );
        ui.add_space(12.0);
        ui.label(RichText::new("Nome").size(theme::SIZE_SMALL).color(p.muted));
        let (n, _) = views::field(
            ui,
            &mut form.name,
            "Nome e sobrenome",
            380.0,
            false,
            !working,
        );
        if form.name.is_empty() && form.email.is_empty() && !n.has_focus() {
            n.request_focus();
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("E-mail")
                .size(theme::SIZE_SMALL)
                .color(p.muted),
        );
        let (_, enter) = views::field(
            ui,
            &mut form.email,
            "nome@empresa.com.br",
            380.0,
            false,
            !working,
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new("Papel")
                .size(theme::SIZE_SMALL)
                .color(p.muted),
        );
        for &r in roles {
            if role_choice(ui, r, form.role == r, !working) {
                form.role = r;
            }
        }
        if let Some(e) = error {
            ui.add_space(2.0);
            ui.label(RichText::new(e).size(13.0).color(p.danger));
        }
        ui.add_space(14.0);
        let ok = !working && !form.name.trim().is_empty() && form.email.contains('@');
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let label = if working { "Criando…" } else { "Criar conta" };
            if theme::button(ui, Kind::Primary, None, label, ok, Some(120.0)).clicked()
                || (enter && ok)
            {
                action = Some(SheetAction::Submit);
            }
            if theme::button(ui, Kind::Ghost, None, "Cancelar", !working, None).clicked() {
                action = Some(SheetAction::Cancel);
            }
            if working {
                views::spinner(ui, 16.0);
            }
        });
        action
    });
    if dismissed && !working {
        return Some(SheetAction::Cancel);
    }
    action
}

/// One radio row: role name and what it allows.
fn role_choice(ui: &mut Ui, role: OrgRole, selected: bool, enabled: bool) -> bool {
    let p = theme::palette(ui);
    let (rect, r) = ui.allocate_exact_size(
        vec2(ui.available_width(), 44.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let h = theme::toggle(ui, r.id.with("h"), r.hovered(), 0.12);
    let s = theme::toggle(ui, r.id.with("s"), selected, 0.16);
    let fill = theme::lerp_color(p.surface_alt.gamma_multiply(h), p.accent_soft, s);
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    let c = pos2(rect.min.x + 16.0, rect.center().y);
    ui.painter().circle_stroke(
        c,
        7.0,
        Stroke::new(1.5, theme::lerp_color(p.field_border, p.accent, s)),
    );
    ui.painter().circle_filled(c, 3.5 * s, p.accent);
    ui.painter().text(
        pos2(rect.min.x + 32.0, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        role_label(role),
        theme::semibold(13.5),
        p.text,
    );
    ui.painter().text(
        pos2(rect.min.x + 32.0, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        role_hint(role),
        FontId::proportional(12.0),
        p.muted,
    );
    r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretAction {
    Copy,
    Done,
}

/// Shows a temporary password once, with what to send and to whom.
pub fn secret_sheet(
    ctx: &egui::Context,
    title: &str,
    name: &str,
    email: &str,
    password: &str,
    copied: bool,
) -> Option<SecretAction> {
    let first = name.split_whitespace().next().unwrap_or(name).to_owned();
    let (action, _) = views::modal_sheet(ctx, "secret", 400.0, |ui, p| {
        let mut action = None;
        ui.label(theme::title(title, theme::SIZE_TITLE));
        ui.label(
            RichText::new(format!(
                "Envie para {first} o e-mail e a senha temporária abaixo."
            ))
            .size(13.5)
            .color(p.muted),
        );
        ui.add_space(12.0);
        egui::Frame::new()
            .fill(p.surface_alt)
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new("E-mail").size(12.0).color(p.muted));
                ui.label(RichText::new(email).size(14.0));
                ui.add_space(8.0);
                ui.label(RichText::new("Senha temporária").size(12.0).color(p.muted));
                ui.label(theme::title(password, 20.0));
            });
        ui.add_space(8.0);
        ui.label(
            RichText::new(format!(
                "No primeiro acesso, {first} cria a própria senha. Esta senha não aparece de novo."
            ))
            .size(12.5)
            .color(p.faint),
        );
        ui.add_space(14.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if theme::button(ui, Kind::Primary, None, "Concluir", true, Some(104.0)).clicked() {
                action = Some(SecretAction::Done);
            }
            let (icon, label) = if copied {
                (Icon::Check, "Copiado")
            } else {
                (Icon::Copy, "Copiar e-mail e senha")
            };
            if theme::button(ui, Kind::Secondary, Some(icon), label, true, None).clicked() {
                action = Some(SecretAction::Copy);
            }
        });
        action
    });
    action
}

/// Confirmation for something that cannot be undone quietly.
pub fn confirm_sheet(
    ctx: &egui::Context,
    title: &str,
    body: &str,
    confirm: &str,
    danger: bool,
    working: bool,
) -> Option<SheetAction> {
    let (action, dismissed) = views::modal_sheet(ctx, "confirm", 360.0, |ui, p| {
        let mut action = None;
        ui.label(theme::title(title, 17.0));
        ui.add_space(2.0);
        ui.label(RichText::new(body).size(13.5).color(p.muted));
        ui.add_space(16.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let kind = if danger { Kind::Danger } else { Kind::Primary };
            if theme::button(ui, kind, None, confirm, !working, None).clicked() {
                action = Some(SheetAction::Submit);
            }
            if theme::button(ui, Kind::Ghost, None, "Cancelar", !working, None).clicked() {
                action = Some(SheetAction::Cancel);
            }
            if working {
                views::spinner(ui, 16.0);
            }
        });
        action
    });
    if dismissed && !working {
        return Some(SheetAction::Cancel);
    }
    action
}

#[derive(Default)]
pub struct PasswordForm {
    pub current: String,
    pub new: String,
    pub confirm: String,
}

/// "Alterar senha" (any account) or, `mandatory`, "Crie sua senha" after signing in with
/// a temporary one (no current-password field: the one just typed is used, and the only
/// way out is signing out).
pub fn password_sheet(
    ctx: &egui::Context,
    form: &mut PasswordForm,
    mandatory: bool,
    working: bool,
    error: Option<&str>,
) -> Option<SheetAction> {
    let (action, dismissed) = views::modal_sheet(ctx, "password", 360.0, |ui, p| {
        let mut action = None;
        let (title, detail) = if mandatory {
            (
                "Crie sua senha",
                "Você entrou com uma senha temporária. Escolha uma senha só sua para continuar.",
            )
        } else {
            (
                "Alterar senha",
                "Os outros computadores em que você entrou serão desconectados.",
            )
        };
        ui.label(theme::title(title, theme::SIZE_TITLE));
        ui.label(RichText::new(detail).size(13.5).color(p.muted));
        ui.add_space(12.0);
        let mut enter = false;
        if !mandatory {
            ui.label(
                RichText::new("Senha atual")
                    .size(theme::SIZE_SMALL)
                    .color(p.muted),
            );
            let (c, e) = views::field(ui, &mut form.current, "", 360.0, true, !working);
            if form.current.is_empty() && !c.has_focus() && form.new.is_empty() {
                c.request_focus();
            }
            enter |= e;
            ui.add_space(4.0);
        }
        ui.label(
            RichText::new("Nova senha")
                .size(theme::SIZE_SMALL)
                .color(p.muted),
        );
        let (n, e) = views::field(
            ui,
            &mut form.new,
            "Pelo menos 12 caracteres",
            360.0,
            true,
            !working,
        );
        if mandatory && form.new.is_empty() && !n.has_focus() {
            n.request_focus();
        }
        enter |= e;
        ui.add_space(4.0);
        ui.label(
            RichText::new("Repita a nova senha")
                .size(theme::SIZE_SMALL)
                .color(p.muted),
        );
        let (_, e) = views::field(ui, &mut form.confirm, "", 360.0, true, !working);
        enter |= e;
        let length_ok = form.new.chars().count() >= 12;
        let same = form.new == form.confirm;
        let hint = if !form.new.is_empty() && !length_ok {
            Some(("Use pelo menos 12 caracteres.", p.muted))
        } else if !form.confirm.is_empty() && !same {
            Some(("As senhas não são iguais.", p.danger))
        } else {
            error.map(|e| (e, p.danger))
        };
        if let Some((text, color)) = hint {
            ui.add_space(2.0);
            ui.label(RichText::new(text).size(13.0).color(color));
        }
        ui.add_space(14.0);
        let ok = !working && length_ok && same && (mandatory || !form.current.is_empty());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let label = match (working, mandatory) {
                (true, _) => "Salvando…",
                (false, true) => "Salvar e entrar",
                (false, false) => "Alterar senha",
            };
            if theme::button(ui, Kind::Primary, None, label, ok, None).clicked() || (enter && ok) {
                action = Some(SheetAction::Submit);
            }
            let cancel = if mandatory { "Sair" } else { "Cancelar" };
            if theme::button(ui, Kind::Ghost, None, cancel, !working, None).clicked() {
                action = Some(SheetAction::Cancel);
            }
            if working {
                views::spinner(ui, 16.0);
            }
        });
        action
    });
    if dismissed && !working && !mandatory {
        return Some(SheetAction::Cancel);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_portuguese_words() {
        for r in [
            OrgRole::Owner,
            OrgRole::Admin,
            OrgRole::Technician,
            OrgRole::Viewer,
        ] {
            assert!(!role_label(r).is_empty());
            assert!(!role_hint(r).is_empty());
        }
    }
}
