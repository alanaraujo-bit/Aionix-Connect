//! Aionix Connect visual language for the desktop apps: dark and light palettes,
//! Segoe UI typography (loaded from the system, never redistributed) and a few
//! reusable components (cards, buttons, status pills).

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response,
    RichText, Sense, Shadow, Stroke, TextStyle, Theme, Ui, Vec2, Visuals,
};

/// Semantic colors of one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface_alt: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub info: Color32,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x0d, 0x10, 0x15),
    surface: Color32::from_rgb(0x15, 0x19, 0x21),
    surface_alt: Color32::from_rgb(0x1c, 0x21, 0x2b),
    border: Color32::from_rgb(0x2a, 0x30, 0x3c),
    text: Color32::from_rgb(0xe8, 0xeb, 0xf1),
    muted: Color32::from_rgb(0x8d, 0x96, 0xa7),
    accent: Color32::from_rgb(0x4f, 0x7c, 0xff),
    accent_hover: Color32::from_rgb(0x6b, 0x91, 0xff),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x34, 0xc7, 0x7b),
    warning: Color32::from_rgb(0xf2, 0xab, 0x3c),
    danger: Color32::from_rgb(0xef, 0x55, 0x5a),
    info: Color32::from_rgb(0x5a, 0xa9, 0xff),
};

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf4, 0xf6, 0xf9),
    surface: Color32::WHITE,
    surface_alt: Color32::from_rgb(0xee, 0xf1, 0xf5),
    border: Color32::from_rgb(0xdc, 0xe1, 0xe8),
    text: Color32::from_rgb(0x11, 0x16, 0x1d),
    muted: Color32::from_rgb(0x5b, 0x65, 0x74),
    accent: Color32::from_rgb(0x36, 0x62, 0xf2),
    accent_hover: Color32::from_rgb(0x4a, 0x73, 0xf7),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x1d, 0x9a, 0x5a),
    warning: Color32::from_rgb(0xc7, 0x7d, 0x0a),
    danger: Color32::from_rgb(0xd6, 0x33, 0x39),
    info: Color32::from_rgb(0x2f, 0x7f, 0xe0),
};

/// Palette of the theme currently shown.
pub fn palette(ui: &Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

pub const SEMIBOLD: &str = "aionix-semibold";

/// Installs fonts and both themes; the app follows the system theme.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    for (theme, p) in [(Theme::Dark, DARK), (Theme::Light, LIGHT)] {
        ctx.set_visuals_of(theme, visuals(theme, p));
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = Vec2::new(10.0, 10.0);
            style.spacing.button_padding = Vec2::new(16.0, 8.0);
            style.spacing.interact_size.y = 36.0;
            style.spacing.text_edit_width = 320.0;
            style.text_styles = [
                (TextStyle::Small, FontId::proportional(12.0)),
                (TextStyle::Body, FontId::proportional(14.5)),
                (TextStyle::Button, FontId::proportional(14.5)),
                (TextStyle::Monospace, FontId::monospace(13.0)),
                (
                    TextStyle::Heading,
                    FontId::new(22.0, FontFamily::Name(SEMIBOLD.into())),
                ),
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
    v.faint_bg_color = p.surface_alt;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(if theme == Theme::Dark { 120 } else { 40 }),
    };
    v.popup_shadow = v.window_shadow;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.override_text_color = Some(p.text);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r8;
    for (state, fill) in [
        (&mut w.inactive, p.surface_alt),
        (&mut w.hovered, p.border),
        (&mut w.active, p.border),
        (&mut w.open, p.surface_alt),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = r8;
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.bg_stroke = Stroke::new(1.0, p.border);
        state.expansion = 0.0;
    }
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.6));
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    v
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

/// Title text in the semibold face.
pub fn title(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(semibold(size))
}

/// Raised surface with border and rounded corners.
pub fn card(ui: &Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(20))
}

fn filled_button(
    ui: &mut Ui,
    text: &str,
    fill: Color32,
    hover: Color32,
    fg: Color32,
    enabled: bool,
    width: Option<f32>,
) -> Response {
    let font = semibold(14.5);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, fg);
    let size = Vec2::new(
        width
            .unwrap_or(galley.size().x + 36.0)
            .max(galley.size().x + 24.0),
        40.0,
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let mut color = if response.hovered() && enabled {
            hover
        } else {
            fill
        };
        if !enabled {
            color = color.gamma_multiply(0.45);
        }
        ui.painter()
            .rect_filled(rect, CornerRadius::same(10), color);
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, fg);
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool, width: Option<f32>) -> Response {
    let p = palette(ui);
    filled_button(
        ui,
        text,
        p.accent,
        p.accent_hover,
        p.on_accent,
        enabled,
        width,
    )
}

pub fn danger_button(ui: &mut Ui, text: &str, enabled: bool, width: Option<f32>) -> Response {
    let p = palette(ui);
    filled_button(
        ui,
        text,
        p.danger,
        p.danger.gamma_multiply(0.85),
        Color32::WHITE,
        enabled,
        width,
    )
}

pub fn secondary_button(ui: &mut Ui, text: &str, enabled: bool, width: Option<f32>) -> Response {
    let p = palette(ui);
    filled_button(ui, text, p.surface_alt, p.border, p.text, enabled, width)
}

/// On/off chip: a small switch and a label; accent-tinted when on.
pub fn toggle_chip(ui: &mut Ui, on: &mut bool, text: &str, enabled: bool) -> Response {
    let p = palette(ui);
    let font = FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, p.text);
    let size = Vec2::new(galley.size().x + 64.0, 36.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(size, sense);
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let alpha = if enabled { 1.0 } else { 0.45 };
        let (fill, stroke) = if *on {
            (
                p.accent.gamma_multiply(0.16 * alpha),
                p.accent.gamma_multiply(0.7 * alpha),
            )
        } else if response.hovered() && enabled {
            (p.border, p.border)
        } else {
            (p.surface_alt, p.border)
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(10),
            fill,
            Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );
        // Switch track + knob.
        let track = egui::Rect::from_center_size(
            egui::pos2(rect.min.x + 26.0, rect.center().y),
            Vec2::new(26.0, 15.0),
        );
        let track_color = if *on {
            p.accent
        } else {
            p.muted.gamma_multiply(0.6)
        };
        ui.painter().rect_filled(
            track,
            CornerRadius::same(8),
            track_color.gamma_multiply(alpha),
        );
        let knob_x = if *on {
            track.max.x - 7.5
        } else {
            track.min.x + 7.5
        };
        ui.painter().circle_filled(
            egui::pos2(knob_x, track.center().y),
            5.5,
            Color32::WHITE.gamma_multiply(alpha),
        );
        let pos = egui::pos2(rect.min.x + 48.0, rect.center().y - galley.size().y / 2.0);
        ui.painter()
            .galley(pos, galley, p.text.gamma_multiply(alpha));
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Small colored dot followed by a label, inside a pill.
pub fn status_pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let p = palette(ui);
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, color);
                ui.label(RichText::new(text).size(13.0).color(p.text));
            });
        })
        .response
}

/// Label + read-only value row used by diagnostics panels.
pub fn kv(ui: &mut Ui, key: &str, value: &str) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 18.0],
            egui::Label::new(RichText::new(key).size(13.0).color(p.muted)),
        );
        ui.label(RichText::new(value).size(13.0));
    });
}

/// The brand mark: a rounded square with an "A" monogram, drawn (no image assets).
pub fn brand_mark(ui: &mut Ui, size: f32) -> Response {
    let p = palette(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same((size * 0.28) as u8), p.accent);
    let c = rect.center();
    let s = size * 0.26;
    let stroke = Stroke::new(size * 0.09, Color32::WHITE);
    painter.line_segment([c + Vec2::new(-s, s), c + Vec2::new(0.0, -s)], stroke);
    painter.line_segment([c + Vec2::new(0.0, -s), c + Vec2::new(s, s)], stroke);
    painter.line_segment(
        [
            c + Vec2::new(-s * 0.5, s * 0.25),
            c + Vec2::new(s * 0.5, s * 0.25),
        ],
        stroke,
    );
    response
}

/// Product name next to the brand mark.
pub fn brand_header(ui: &mut Ui, subtitle: &str) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        brand_mark(ui, 34.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(title(aionix_brand::brand().product_name.clone(), 17.0));
            ui.label(RichText::new(subtitle).size(12.5).color(p.muted));
        });
    });
}
