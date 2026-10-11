//! Aionix Connect visual language for the desktop apps.
//!
//! THESIS: the category's canonical remote-support app (sidebar + content, the ID as the
//! hero of Início, a computer list, a full-bleed session window) executed at the finish
//! of Apple Screen Sharing and the current TeamViewer; it refuses gradients on controls,
//! glows, neon, monogram marks and card grids.
//! OWN-WORLD: Windows-native neutrals that follow the system light/dark setting, one blue
//! accent only for primary actions, focus and selection, green only for "online" and
//! "conectado", 1 px hairlines, 8 px control corners, 12 px sheets, Segoe UI, drawn
//! line icons, and the two-screens mark.
//! STORY: the person being helped reads out one ID; the technician signs in once,
//! picks a computer or types the ID, and the session opens edge to edge.
//! FIRST VIEWPORT: 232 px sidebar (mark + name, Início, Computadores, account and
//! version at the bottom); content: "Seu ID" at 44 px with Copiar, the status line, a
//! hairline, then "Acessar outro computador" with the ID field and the blue Conectar.
//! FORM: the category standard (canon), quality bar Apple Screen Sharing + TeamViewer;
//! seed 2c2b5fae.
//! FINISH: unreviewed and undocumented is unfinished; this build ends with the finish
//! review, the verdict, DESIGN.md, and every shipping raster carrying its provenance.

use std::sync::OnceLock;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Id, Margin, Rect,
    Response, RichText, Sense, Shadow, Stroke, TextStyle, Theme, Ui, Vec2, Visuals, pos2,
};

use crate::icons::{self, Icon};

/// Semantic colors of one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Content area.
    pub bg: Color32,
    /// Sidebar and the session toolbar.
    pub sidebar: Color32,
    /// Raised surfaces: sheets, menus, overlays.
    pub surface: Color32,
    /// Fields, secondary buttons, hover fills.
    pub surface_alt: Color32,
    /// Control outlines.
    pub border: Color32,
    /// Text field outline at rest (≥ 3:1 on `bg`).
    pub field_border: Color32,
    /// Separators between regions and rows.
    pub hairline: Color32,
    pub text: Color32,
    /// Secondary text.
    pub muted: Color32,
    /// Tertiary text and placeholders (≥ 4.5:1 on `bg`, `sidebar` and `surface_alt`).
    pub faint: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_press: Color32,
    /// Selected rows and nav items.
    pub accent_soft: Color32,
    /// Accent used as text (links) on `bg`.
    pub accent_text: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    /// Filled destructive buttons (white text ≥ 4.5:1).
    pub danger_fill: Color32,
    pub info: Color32,
    /// Opacity of elevation shadows.
    pub shadow: u8,
}

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xff, 0xff, 0xff),
    sidebar: Color32::from_rgb(0xf4, 0xf4, 0xf6),
    surface: Color32::from_rgb(0xff, 0xff, 0xff),
    surface_alt: Color32::from_rgb(0xf0, 0xf1, 0xf3),
    border: Color32::from_rgb(0xd8, 0xda, 0xdf),
    field_border: Color32::from_rgb(0x8c, 0x8f, 0x96),
    hairline: Color32::from_rgb(0xe6, 0xe7, 0xea),
    text: Color32::from_rgb(0x18, 0x19, 0x1c),
    muted: Color32::from_rgb(0x55, 0x58, 0x5f),
    faint: Color32::from_rgb(0x68, 0x6b, 0x72),
    accent: Color32::from_rgb(0x0b, 0x63, 0xe5),
    accent_hover: Color32::from_rgb(0x0a, 0x58, 0xcc),
    accent_press: Color32::from_rgb(0x09, 0x4d, 0xb3),
    accent_soft: Color32::from_rgb(0xe8, 0xf0, 0xfd),
    accent_text: Color32::from_rgb(0x0b, 0x5c, 0xd6),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x1a, 0x8f, 0x4c),
    warning: Color32::from_rgb(0x9a, 0x60, 0x08),
    danger: Color32::from_rgb(0xcf, 0x32, 0x38),
    danger_fill: Color32::from_rgb(0xd1, 0x34, 0x3a),
    info: Color32::from_rgb(0x0b, 0x63, 0xe5),
    shadow: 34,
};

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x1e, 0x1e, 0x21),
    sidebar: Color32::from_rgb(0x17, 0x17, 0x1a),
    surface: Color32::from_rgb(0x28, 0x28, 0x2c),
    surface_alt: Color32::from_rgb(0x2d, 0x2d, 0x32),
    border: Color32::from_rgb(0x3d, 0x3d, 0x44),
    field_border: Color32::from_rgb(0x72, 0x74, 0x7b),
    hairline: Color32::from_rgb(0x30, 0x30, 0x35),
    text: Color32::from_rgb(0xed, 0xed, 0xef),
    muted: Color32::from_rgb(0xa6, 0xa8, 0xaf),
    faint: Color32::from_rgb(0x95, 0x97, 0x9e),
    accent: Color32::from_rgb(0x22, 0x6c, 0xe8),
    accent_hover: Color32::from_rgb(0x26, 0x70, 0xec),
    accent_press: Color32::from_rgb(0x1f, 0x62, 0xd4),
    accent_soft: Color32::from_rgb(0x22, 0x2e, 0x45),
    accent_text: Color32::from_rgb(0x6c, 0xa6, 0xff),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x3c, 0xc3, 0x73),
    warning: Color32::from_rgb(0xe6, 0xa2, 0x3c),
    danger: Color32::from_rgb(0xf2, 0x5c, 0x58),
    danger_fill: Color32::from_rgb(0xd1, 0x34, 0x3a),
    info: Color32::from_rgb(0x6c, 0xa6, 0xff),
    shadow: 110,
};

/// Palette of the theme currently shown.
pub fn palette(ui: &Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

pub const SEMIBOLD: &str = "aionix-semibold";

// ------------------------------------------------------------------ type scale

/// The 9-digit ID on Início.
pub const SIZE_ID: f32 = 44.0;
/// Page and sheet titles.
pub const SIZE_TITLE: f32 = 20.0;
/// Section headings.
pub const SIZE_HEADING: f32 = 15.0;
pub const SIZE_BODY: f32 = 14.0;
pub const SIZE_SMALL: f32 = 12.5;
pub const SIZE_CAPTION: f32 = 11.5;

/// Installs fonts and both themes; the app follows the system theme.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    for (theme, p) in [(Theme::Dark, DARK), (Theme::Light, LIGHT)] {
        ctx.set_visuals_of(theme, visuals(theme, p));
        ctx.style_mut_of(theme, |style| {
            style.animation_time = if reduced_motion() { 0.0 } else { 0.16 };
            style.spacing.item_spacing = Vec2::new(8.0, 8.0);
            style.spacing.button_padding = Vec2::new(14.0, 7.0);
            style.spacing.interact_size.y = 34.0;
            style.spacing.text_edit_width = 320.0;
            style.spacing.scroll = egui::style::ScrollStyle {
                bar_width: 8.0,
                floating_width: 3.0,
                floating_allocated_width: 0.0,
                ..egui::style::ScrollStyle::floating()
            };
            style.text_styles = [
                (TextStyle::Small, FontId::proportional(SIZE_SMALL)),
                (TextStyle::Body, FontId::proportional(SIZE_BODY)),
                (TextStyle::Button, FontId::proportional(SIZE_BODY)),
                (TextStyle::Monospace, FontId::monospace(13.0)),
                (TextStyle::Heading, semibold(SIZE_TITLE)),
            ]
            .into();
        });
    }
    ctx.set_theme(egui::ThemePreference::System);
}

fn visuals(theme: Theme, p: Palette) -> Visuals {
    let mut v = match theme {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };
    let r8 = CornerRadius::same(8);
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface_alt;
    v.text_edit_bg_color = Some(p.bg);
    v.faint_bg_color = p.surface_alt;
    v.code_bg_color = p.surface_alt;
    v.window_stroke = Stroke::new(1.0, p.hairline);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = elevation(&p, 2);
    v.popup_shadow = elevation(&p, 1);
    v.hyperlink_color = p.accent_text;
    v.selection.bg_fill = p.accent.gamma_multiply(if theme == Theme::Dark { 0.45 } else { 0.22 });
    v.selection.stroke = Stroke::new(1.5, p.accent);
    v.text_cursor.stroke = Stroke::new(1.5, p.accent);
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.faint);
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.striped = false;
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.bg;
    w.noninteractive.weak_bg_fill = p.bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.hairline);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r8;
    for (state, fill, stroke) in [
        (&mut w.inactive, p.surface_alt, p.border),
        (&mut w.hovered, p.surface_alt, p.faint),
        (&mut w.active, p.surface_alt, p.accent),
        (&mut w.open, p.surface_alt, p.border),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = r8;
        state.fg_stroke = Stroke::new(1.25, p.text);
        state.bg_stroke = Stroke::new(1.0, stroke);
        state.expansion = 0.0;
    }
    // Text fields: an outline at rest, darker on hover, the accent ring when focused.
    w.inactive.bg_stroke = Stroke::new(1.0, p.field_border);
    w.hovered.bg_stroke = Stroke::new(1.0, p.muted);
    v
}

/// Soft neutral elevation: level 1 menus/tooltips, 2 sheets and floating bars.
pub fn elevation(p: &Palette, level: u8) -> Shadow {
    let (offset, blur, alpha) = match level {
        0 => (1, 3, p.shadow / 2),
        1 => (4, 14, p.shadow),
        _ => (12, 36, p.shadow.saturating_add(p.shadow / 2)),
    };
    Shadow {
        offset: [0, offset],
        blur,
        spread: 0,
        color: Color32::from_black_alpha(alpha),
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join("Fonts");
    let load = |name: &str| std::fs::read(dir.join(name)).ok();
    if let Some(regular) = load("segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe".into(), FontData::from_owned(regular).into());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".into());
    }
    let semibold = load("seguisb.ttf").or_else(|| load("segoeuib.ttf"));
    let mut semibold_family = Vec::new();
    if let Some(bytes) = semibold {
        fonts
            .font_data
            .insert("segoe-semibold".into(), FontData::from_owned(bytes).into());
        semibold_family.push("segoe-semibold".to_owned());
    }
    // Fallbacks (default egui fonts) for glyphs Segoe UI does not have.
    semibold_family.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold_family);
    ctx.set_fonts(fonts);
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Text in the semibold face.
pub fn title(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(semibold(size))
}

/// Secondary text at `size`.
pub fn muted(ui: &Ui, text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).size(size).color(palette(ui).muted)
}

// ------------------------------------------------------------------ motion

/// Windows "Efeitos de animação" turned off (Acessibilidade → Efeitos visuais): every
/// transition jumps to its end state.
pub fn reduced_motion() -> bool {
    static REDUCED: OnceLock<bool> = OnceLock::new();
    *REDUCED.get_or_init(|| {
        #[cfg(windows)]
        {
            use windows::Win32::UI::WindowsAndMessaging::{
                SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
                SystemParametersInfoW,
            };
            let mut on = windows::core::BOOL(1);
            // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the pointer we pass.
            let ok = unsafe {
                SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    Some(&mut on as *mut _ as *mut _),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            };
            ok.is_ok() && !on.as_bool()
        }
        #[cfg(not(windows))]
        false
    })
}

/// Exponential ease-out: fast start, long gentle settle, no overshoot.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 { 1.0 } else { 1.0 - 2f32.powf(-10.0 * t) }
}

/// 0→1 while `on` becomes true (and back), eased; instant under reduced motion.
pub fn toggle(ui: &Ui, id: Id, on: bool, seconds: f32) -> f32 {
    if reduced_motion() {
        return if on { 1.0 } else { 0.0 };
    }
    ui.ctx()
        .animate_bool_with_time_and_easing(id, on, seconds, emath_ease_out)
}

fn emath_ease_out(t: f32) -> f32 {
    ease_out(t)
}

/// Entrance progress 0→1 measured from the first frame `id` was seen (or since
/// [`restart`] was called for it), starting after `delay` seconds.
pub fn entrance(ui: &Ui, id: Id, delay: f32, seconds: f32) -> f32 {
    if reduced_motion() {
        return 1.0;
    }
    let now = ui.input(|i| i.time);
    let start = ui
        .ctx()
        .data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    let t = ((now - start) as f32 - delay) / seconds;
    if t < 1.0 {
        ui.ctx().request_repaint();
    }
    ease_out(t)
}

/// Replays the [`entrance`] of `id` from now.
pub fn restart(ui: &Ui, id: Id) {
    let now = ui.input(|i| i.time);
    ui.ctx().data_mut(|d| d.insert_temp(id, now));
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

// ------------------------------------------------------------------ buttons

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The one blue action of a region.
    Primary,
    /// Neutral filled.
    Secondary,
    /// No fill until hovered (toolbars, sidebar).
    Ghost,
    /// Filled red: ends something for good.
    Danger,
    /// Accent text, soft accent fill on hover (row actions in lists).
    Quiet,
    /// Red text, soft red fill on hover (destructive row actions).
    QuietDanger,
}

/// The button: label with an optional leading icon; hover and press are animated
/// color changes, and a pressed button settles 1 px lower.
pub fn button(
    ui: &mut Ui,
    kind: Kind,
    icon: Option<Icon>,
    text: &str,
    enabled: bool,
    width: Option<f32>,
) -> Response {
    let p = palette(ui);
    let font = semibold(13.5);
    let (fill, hover, press, fg) = match kind {
        Kind::Primary => (p.accent, p.accent_hover, p.accent_press, p.on_accent),
        Kind::Secondary => (p.surface_alt, hover_of(&p, p.surface_alt), press_of(&p, p.surface_alt), p.text),
        Kind::Ghost => (p.surface_alt.gamma_multiply(0.0), p.surface_alt, press_of(&p, p.surface_alt), p.text),
        Kind::Danger => (p.danger_fill, p.danger_fill.gamma_multiply(0.9), p.danger_fill.gamma_multiply(0.8), Color32::WHITE),
        Kind::Quiet => (p.accent_soft.gamma_multiply(0.0), p.accent_soft, lerp_color(p.accent_soft, p.accent, 0.15), p.accent_text),
        Kind::QuietDanger => (p.danger.gamma_multiply(0.0), p.danger.gamma_multiply(0.12), p.danger.gamma_multiply(0.2), p.danger),
    };
    let ink = match (enabled, kind) {
        (true, _) => fg,
        (false, Kind::Primary) => p.accent_text.gamma_multiply(0.6),
        (false, _) => p.faint,
    };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, ink);
    let icon_w = if icon.is_some() { 16.0 + if text.is_empty() { 0.0 } else { 8.0 } } else { 0.0 };
    let content = galley.size().x + icon_w;
    let pad = if text.is_empty() { 9.0 } else { 16.0 };
    let size = Vec2::new(width.unwrap_or(content + pad * 2.0).max(content + 18.0), 36.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let hovered = enabled && response.hovered();
    let pressed = enabled && response.is_pointer_button_down_on();
    let h = toggle(ui, response.id.with("h"), hovered, 0.12);
    let d = toggle(ui, response.id.with("d"), pressed, 0.08);
    if ui.is_rect_visible(rect) {
        let mut color = lerp_color(lerp_color(fill, hover, h), press, d);
        if !enabled && kind == Kind::Primary {
            color = p.accent_soft;
        } else if !enabled && !matches!(kind, Kind::Ghost | Kind::Quiet | Kind::QuietDanger) {
            color = p.surface_alt;
        }
        let r = rect.translate(Vec2::new(0.0, d));
        ui.painter().rect_filled(r, CornerRadius::same(8), color);
        if kind == Kind::Secondary && enabled {
            ui.painter().rect_stroke(r, CornerRadius::same(8), Stroke::new(1.0, p.hairline), egui::StrokeKind::Inside);
        }
        let x0 = r.center().x - content / 2.0;
        if let Some(icon) = icon {
            let ir = Rect::from_center_size(pos2(x0 + 8.0, r.center().y), Vec2::splat(16.0));
            icons::paint(ui.painter(), ir, icon, ink);
        }
        let pos = pos2(x0 + icon_w, r.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, ink);
        if response.has_focus() {
            focus_ring(ui, r, 8);
        }
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

fn hover_of(p: &Palette, c: Color32) -> Color32 {
    lerp_color(c, p.text, 0.06)
}

fn press_of(p: &Palette, c: Color32) -> Color32 {
    lerp_color(c, p.text, 0.11)
}

/// Keyboard focus: a 2 px accent ring just outside the control.
pub fn focus_ring(ui: &Ui, rect: Rect, radius: u8) {
    let p = palette(ui);
    ui.painter().rect_stroke(
        rect.expand(2.0),
        CornerRadius::same(radius + 2),
        Stroke::new(2.0, p.accent.gamma_multiply(0.8)),
        egui::StrokeKind::Outside,
    );
}

pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool, width: Option<f32>) -> Response {
    button(ui, Kind::Primary, None, text, enabled, width)
}

pub fn secondary_button(ui: &mut Ui, text: &str, enabled: bool, width: Option<f32>) -> Response {
    button(ui, Kind::Secondary, None, text, enabled, width)
}

pub fn danger_button(ui: &mut Ui, text: &str, enabled: bool, width: Option<f32>) -> Response {
    button(ui, Kind::Danger, None, text, enabled, width)
}

/// Square icon-only button (toolbars). `active` keeps it tinted like a pressed toggle.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str, active: bool, enabled: bool) -> Response {
    let p = palette(ui);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), sense);
    let h = toggle(ui, response.id.with("h"), enabled && response.hovered(), 0.12);
    let a = toggle(ui, response.id.with("a"), active, 0.16);
    if ui.is_rect_visible(rect) {
        let rest = lerp_color(p.surface_alt.gamma_multiply(0.0), p.accent_soft, a);
        let fill = lerp_color(rest, if active { p.accent_soft } else { p.surface_alt }, h);
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        let ink = if !enabled {
            p.faint.gamma_multiply(0.6)
        } else {
            lerp_color(p.muted, p.accent_text, a)
        };
        icons::paint(ui.painter(), rect.shrink(8.0), icon, ink);
        if response.has_focus() {
            focus_ring(ui, rect, 8);
        }
    }
    let response = response.on_hover_text(tooltip);
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Text-only link-style action.
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let p = palette(ui);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(SIZE_SMALL), p.accent_text);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
    let h = toggle(ui, response.id.with("h"), response.hovered(), 0.12);
    let color = lerp_color(p.accent_text, p.text, h * 0.35);
    ui.painter().galley(rect.min, galley, color);
    if h > 0.0 {
        let y = rect.max.y - 0.5;
        ui.painter().line_segment(
            [pos2(rect.min.x, y), pos2(rect.max.x, y)],
            Stroke::new(1.0, color.gamma_multiply(h)),
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// On/off switch with a label (installer options, settings).
pub fn toggle_chip(ui: &mut Ui, on: &mut bool, text: &str, enabled: bool) -> Response {
    let p = palette(ui);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(SIZE_BODY), p.text);
    let size = Vec2::new(galley.size().x + 50.0, 32.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, mut response) = ui.allocate_exact_size(size, sense);
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = toggle(ui, response.id.with("on"), *on, 0.18);
    if ui.is_rect_visible(rect) {
        let alpha = if enabled { 1.0 } else { 0.45 };
        let track = Rect::from_center_size(pos2(rect.min.x + 18.0, rect.center().y), Vec2::new(34.0, 20.0));
        let off = p.border;
        ui.painter().rect_filled(track, CornerRadius::same(10), lerp_color(off, p.accent, t).gamma_multiply(alpha));
        let knob = pos2(track.min.x + 10.0 + t * 14.0, track.center().y);
        ui.painter().circle_filled(knob + Vec2::new(0.0, 0.5), 8.0, Color32::from_black_alpha(30));
        ui.painter().circle_filled(knob, 8.0, Color32::WHITE.gamma_multiply(alpha));
        let pos = pos2(rect.min.x + 44.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, p.text.gamma_multiply(alpha));
        if response.has_focus() {
            focus_ring(ui, track, 10);
        }
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

// ------------------------------------------------------------------ status

/// Status dot; `pulse` breathes a soft ring around it (only for in-progress states).
pub fn status_dot(ui: &mut Ui, color: Color32, pulse: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    let c = rect.center();
    if pulse && !reduced_motion() {
        let t = (ui.input(|i| i.time) * 0.9).fract() as f32;
        ui.painter().circle_filled(c, 4.0 + 5.0 * ease_out(t), color.gamma_multiply(0.35 * (1.0 - t)));
        ui.ctx().request_repaint();
    }
    ui.painter().circle_filled(c, 4.0, color);
    response
}

/// Dot + text, no container.
pub fn status_line(ui: &mut Ui, text: &str, color: Color32, pulse: bool) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        status_dot(ui, color, pulse);
        ui.label(RichText::new(text).size(SIZE_BODY).color(palette(ui).text));
    })
    .response
}

/// Compact status for tight rows: dot and label.
pub fn status_pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        status_dot(ui, color, false);
        ui.label(RichText::new(text).size(SIZE_SMALL).color(palette(ui).muted));
    })
    .response
}

// ------------------------------------------------------------------ structure

/// A full-width 1 px separator.
pub fn hairline(ui: &mut Ui) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.hairline);
}

/// Raised surface for overlays and sheets.
pub fn sheet(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.hairline))
        .corner_radius(CornerRadius::same(12))
        .shadow(elevation(p, 2))
        .inner_margin(Margin::same(24))
}

/// Plain grouping surface (installer, development launcher).
pub fn card(ui: &Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.bg)
        .stroke(Stroke::new(1.0, p.hairline))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(20))
}

/// Label + read-only value row used by diagnostics panels.
pub fn kv(ui: &mut Ui, key: &str, value: &str) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 18.0],
            egui::Label::new(RichText::new(key).size(SIZE_SMALL).color(p.muted)),
        );
        ui.label(RichText::new(value).size(SIZE_SMALL));
    });
}

/// Initials in a neutral circle.
pub fn avatar(ui: &mut Ui, name: &str, size: f32) -> Response {
    let p = palette(ui);
    let initials: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, p.accent_soft);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initials,
        semibold(size * 0.4),
        p.accent_text,
    );
    response
}

/// Placeholder bar that shimmers while content loads.
pub fn skeleton(ui: &mut Ui, size: Vec2) -> Response {
    let p = palette(ui);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let base = p.surface_alt;
    let t = if reduced_motion() { 0.5 } else { (ui.input(|i| i.time) * 0.8).fract() as f32 };
    let shine = (1.0 - ((t * 2.0 - 1.0).abs())) * 0.5;
    ui.painter().rect_filled(rect, CornerRadius::same(6), lerp_color(base, p.hairline, shine));
    if !reduced_motion() {
        ui.ctx().request_repaint();
    }
    response
}

// ------------------------------------------------------------------ brand

/// The mark: two landscape screens that overlap (the front one on a stand), the region
/// they share filled solid, on the blue tile. Same 24-unit geometry as
/// infra/branding/make-icons.py.
pub fn brand_mark(ui: &mut Ui, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_mark(ui.painter(), rect);
    response
}

pub fn paint_mark(painter: &egui::Painter, rect: Rect) {
    let s = rect.width() / 24.0;
    let at = |x0: f32, y0: f32, x1: f32, y1: f32| {
        Rect::from_min_max(rect.min + Vec2::new(x0, y0) * s, rect.min + Vec2::new(x1, y1) * s)
    };
    let radius = |r: f32| CornerRadius::same((r * s).round() as u8);
    painter.rect_filled(rect, CornerRadius::same((rect.width() * 0.225) as u8), Color32::from_rgb(0x1b, 0x6a, 0xeb));
    let w = if rect.width() <= 32.0 { 2.1 } else { 1.7 } * s;
    let white = Color32::WHITE;
    for r in [at(3.0, 3.6, 15.6, 11.8), at(8.4, 8.8, 21.0, 17.0)] {
        painter.rect_stroke(r, radius(1.6), Stroke::new(w, white), egui::StrokeKind::Inside);
    }
    painter.rect_filled(at(8.4, 8.8, 15.6, 11.8), radius(0.5), white);
    painter.rect_filled(at(13.85, 17.0, 15.55, 19.4), radius(0.4), white);
    painter.rect_filled(at(11.6, 19.0, 17.8, 20.6), radius(0.6), white);
}

/// Product name next to the mark.
pub fn brand_header(ui: &mut Ui, subtitle: &str) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        brand_mark(ui, 32.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(title(aionix_brand::brand().product_name.clone(), 16.0));
            ui.label(RichText::new(subtitle).size(SIZE_SMALL).color(p.muted));
        });
    });
}
