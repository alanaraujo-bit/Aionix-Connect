//! Renders the app's screens with sample data to PNG, light and dark, for design review.
//! Needs a GPU adapter, so it only runs when asked:
//!
//!     cargo test -p aionix-desktop --test ui_preview -- --ignored
//!
//! Output: `.impeccable/review/app-*.png` at the repository root. All names, IDs and
//! customers below are sample data.

use aionix_desktop::{
    theme,
    views::{self, *},
};
use egui::{Theme, Vec2};
use egui_kittest::Harness;

fn out_dir() -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.impeccable/review");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn shot(name: &str, mode: Theme, size: Vec2, mut draw: impl FnMut(&mut egui::Ui)) {
    let mut installed = false;
    let mut harness = Harness::builder()
        .with_size(size)
        .with_theme(mode)
        .with_step_dt(0.25)
        .with_max_steps(64)
        .wgpu()
        .build_ui(move |ui| {
            // Fonts set during a frame apply from the next one.
            if !installed {
                theme::install(ui.ctx());
                ui.ctx().set_theme(mode);
                installed = true;
                ui.ctx().request_repaint();
                return;
            }
            draw(ui);
        });
    harness.run_steps(14);
    let img = harness.render().expect("render");
    let suffix = if mode == Theme::Dark { "dark" } else { "light" };
    img.save(out_dir().join(format!("app-{name}-{suffix}.png")))
        .unwrap();
}

const WINDOW: Vec2 = Vec2::new(1040.0, 680.0);

fn devices() -> Vec<DeviceModel> {
    let d = |name: &str, id: &str, sys: &str, customer: Option<&str>, presence, seen: Option<&str>| DeviceModel {
        id: id.into(),
        name: name.into(),
        system: sys.into(),
        customer: customer.map(Into::into),
        presence,
        this_computer: false,
        seen: seen.map(Into::into),
    };
    let mut v = vec![
        d("DESKTOP-ALAN", "845 948 277", "Windows 11 Pro", None, Presence::Online, None),
        d("Recepção", "312 604 958", "Windows 10 Pro", Some("Clínica Santa Luzia"), Presence::Online, None),
        d("Financeiro", "590 221 734", "Windows 11 Pro", Some("Escritório Moura"), Presence::Online, None),
        d("Notebook Marina", "703 118 462", "Windows 11 Home", Some("Escritório Moura"), Presence::Connecting, None),
        d("Caixa 2", "468 930 115", "Windows 10 Pro", Some("Mercado Bom Preço"), Presence::Offline, Some("visto há 3 h")),
        d("Servidor de arquivos", "927 455 380", "Windows Server 2022", Some("Clínica Santa Luzia"), Presence::Offline, Some("visto ontem")),
    ];
    v[0].this_computer = true;
    v
}

fn host(status: HostStatus) -> HostModel {
    HostModel {
        id: (status != HostStatus::Starting).then(|| ("845 948 277".into(), "845948277".into())),
        status,
        status_text: match status {
            HostStatus::Online => "Pronto para receber suporte",
            HostStatus::Starting => "Iniciando…",
            HostStatus::Connecting => "Conectando ao servidor…",
            HostStatus::Offline => "Sem conexão com o servidor",
        }
        .into(),
        status_detail: (status == HostStatus::Offline).then(|| "Verifique a internet. Tentando novamente…".into()),
        pending: None,
        inbound: None,
        last_outcome: None,
        copied: false,
    }
}

fn side<'a>(page: Page, signed_in: bool, update: UpdateModel<'a>) -> SidebarModel<'a> {
    SidebarModel {
        page,
        account: signed_in.then_some(AccountModel { name: "Alan Araujo", org: "Aionix Suporte" }),
        signing_in: false,
        online: signed_in.then_some(3),
        env_label: None,
        version: "0.2.3",
        update,
    }
}

fn home(name: &str, mode: Theme, h: HostModel, signed_in: bool, update: fn() -> UpdateModel<'static>) {
    let devs = devices();
    let quick: Vec<DeviceModel> = devs.into_iter().filter(|d| d.presence == Presence::Online).collect();
    let mut typed = String::new();
    let stale = h.status == HostStatus::Offline;
    shot(name, mode, WINDOW, move |ui| {
        let connect = ConnectModel {
            signed_in,
            signing_in: false,
            error: None,
            quick: &quick,
            total: 6,
            stale,
        };
        views::shell(ui, &side(Page::Home, signed_in, update()), |ui| {
            views::home_page(ui, &h, |ui| { views::connect_section(ui, &connect, &mut typed); }, |ui| {
                views::disclosure(ui, "Detalhes técnicos", |_| {});
            });
        });
    });
}

#[test]
#[ignore = "renders with the GPU; run explicitly for design review"]
fn preview() {
    for mode in [Theme::Light, Theme::Dark] {
        home("home", mode, host(HostStatus::Online), true, || UpdateModel::Idle);
        home("signed-out", mode, host(HostStatus::Online), false, || UpdateModel::UpToDate);

        let mut h = host(HostStatus::Online);
        h.status_text = "Em sessão".into();
        h.inbound = Some(InboundModel {
            name: "Alan Araujo".into(),
            email: None,
            permissions: "ver a tela e usar mouse e teclado".into(),
            duration: Some("12:03".into()),
            state: None,
            can_end: true,
        });
        home("inbound", mode, h, false, || UpdateModel::Ready { version: "0.2.4", can_restart: false });

        let mut h = host(HostStatus::Online);
        h.pending = Some("Alan Araujo".into());
        home("pending", mode, h, false, || UpdateModel::Downloading { version: "0.2.4", fraction: 0.42 });

        home("starting", mode, host(HostStatus::Starting), false, || UpdateModel::Checking);
        home("offline", mode, host(HostStatus::Offline), true, || UpdateModel::Failed { error: "404" });

        // Computadores.
        let devs = devices();
        let mut filter = String::new();
        shot("computers", mode, WINDOW, move |ui| {
            let m = ComputersModel { devices: &devs, total: 6, online: 3, loaded: true, has_org: true, error: None };
            views::shell(ui, &side(Page::Computers, true, UpdateModel::Idle), |ui| {
                views::computers_page(ui, &m, &mut filter);
            });
        });

        // Sign-in sheet over Início.
        let (mut email, mut password) = ("alan@aionixdev.com".to_owned(), String::new());
        let h = host(HostStatus::Online);
        let mut typed = "312 604 958".to_owned();
        shot("login", mode, WINDOW, move |ui| {
            let connect = ConnectModel { signed_in: false, signing_in: false, error: None, quick: &[], total: 0, stale: false };
            views::shell(ui, &side(Page::Home, false, UpdateModel::Idle), |ui| {
                views::home_page(ui, &h, |ui| { views::connect_section(ui, &connect, &mut typed); }, |_| {});
            });
            let m = LoginModel { working: None, error: None, then_connect: Some("312 604 958".into()) };
            views::login_sheet(ui.ctx(), &m, &mut email, &mut password);
        });

        // Session: waiting for the other side to allow.
        let (mut control, mut diag) = (true, false);
        shot("session-waiting", mode, WINDOW, move |ui| {
            let full = ui.max_rect();
            let bar = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), TOOLBAR_HEIGHT));
            let m = ToolbarModel {
                title: "Recepção",
                id: "312 604 958",
                state: "Aguardando autorização",
                tone: Tone::Warning,
                pulse: true,
                metrics: None,
                terminal: false,
                can_control: false,
                fullscreen: false,
            };
            views::session_toolbar(&mut ui.new_child(egui::UiBuilder::new().max_rect(bar)), &m, &mut control, &mut diag);
            let area = egui::Rect::from_min_max(egui::pos2(full.min.x, bar.max.y), full.max);
            ui.painter().rect_filled(area, 0.0, theme::palette(ui).bg);
            views::session_overlay(ui, area, &OverlayModel {
                headline: "Aguardando autorização…",
                detail: "O usuário de Recepção precisa clicar em Permitir.",
                busy: true,
                tone: Tone::Warning,
            });
        });

        // Session: connected (sample picture stands in for the remote screen).
        let (mut control, mut diag) = (true, false);
        shot("session-live", mode, WINDOW, move |ui| {
            let full = ui.max_rect();
            let bar = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), TOOLBAR_HEIGHT));
            let m = ToolbarModel {
                title: "Recepção",
                id: "312 604 958",
                state: "Conectado",
                tone: Tone::Success,
                pulse: false,
                metrics: Some("Direto  ·  42 ms  ·  30 fps  ·  1920×1080".into()),
                terminal: false,
                can_control: true,
                fullscreen: false,
            };
            views::session_toolbar(&mut ui.new_child(egui::UiBuilder::new().max_rect(bar)), &m, &mut control, &mut diag);
            let area = egui::Rect::from_min_max(egui::pos2(full.min.x, bar.max.y), full.max);
            ui.painter().rect_filled(area, 0.0, egui::Color32::from_rgb(11, 11, 12));
            let screen = egui::Rect::from_center_size(area.center(), egui::vec2(area.height() * 16.0 / 9.0, area.height()));
            ui.painter().rect_filled(screen, 0.0, egui::Color32::from_rgb(0x2b, 0x4a, 0x6f));
            ui.painter().rect_filled(
                egui::Rect::from_min_max(egui::pos2(screen.min.x, screen.max.y - 28.0), screen.max),
                0.0,
                egui::Color32::from_rgb(0x1f, 0x22, 0x28),
            );
        });
    }
}
