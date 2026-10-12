//! Renders the app's screens with sample data to PNG, light and dark, for design review.
//! Needs a GPU adapter, so it only runs when asked:
//!
//!     cargo test -p aionix-desktop --test ui_preview -- --ignored
//!
//! Output: `.impeccable/review/app-*.png` at the repository root. All names, IDs and
//! customers below are sample data.

use aionix_control_api::rest::OrgRole;
use aionix_desktop::admin_views::{
    self, AccountRow, AdminModel, AdminTab, HistoryRow, NewAccountForm, PasswordForm, SessionRow,
};
use aionix_desktop::{
    theme,
    views::{self, *},
};
use egui::{Theme, Vec2};
use egui_kittest::Harness;
use uuid::Uuid;

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
    let d =
        |name: &str, id: &str, sys: &str, customer: Option<&str>, presence, seen: Option<&str>| {
            DeviceModel {
                id: id.into(),
                name: name.into(),
                system: sys.into(),
                customer: customer.map(Into::into),
                presence,
                this_computer: false,
                seen: seen.map(Into::into),
            }
        };
    let mut v = vec![
        d(
            "DESKTOP-ALAN",
            "845 948 277",
            "Windows 11 Pro",
            None,
            Presence::Online,
            None,
        ),
        d(
            "Recepção",
            "312 604 958",
            "Windows 10 Pro",
            Some("Clínica Santa Luzia"),
            Presence::Online,
            None,
        ),
        d(
            "Financeiro",
            "590 221 734",
            "Windows 11 Pro",
            Some("Escritório Moura"),
            Presence::Online,
            None,
        ),
        d(
            "Notebook Marina",
            "703 118 462",
            "Windows 11 Home",
            Some("Escritório Moura"),
            Presence::Connecting,
            None,
        ),
        d(
            "Caixa 2",
            "468 930 115",
            "Windows 10 Pro",
            Some("Mercado Bom Preço"),
            Presence::Offline,
            Some("visto há 3 h"),
        ),
        d(
            "Servidor de arquivos",
            "927 455 380",
            "Windows Server 2022",
            Some("Clínica Santa Luzia"),
            Presence::Offline,
            Some("visto ontem"),
        ),
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
        status_detail: (status == HostStatus::Offline)
            .then(|| "Verifique a internet. Tentando novamente…".into()),
        pending: None,
        inbound: None,
        last_outcome: None,
        copied: false,
        managed: true,
        unattended_enabled: false,
    }
}

fn side<'a>(page: Page, signed_in: bool, update: UpdateModel<'a>) -> SidebarModel<'a> {
    SidebarModel {
        page,
        account: signed_in.then_some(AccountModel {
            name: "Alan Araujo",
            org: "Aionix Suporte",
        }),
        signing_in: false,
        online: signed_in.then_some(3),
        env_label: None,
        version: "0.2.6",
        update,
        admin: signed_in,
    }
}

fn accounts() -> Vec<AccountRow> {
    let a = |name: &str, email: &str, role, disabled, temporary, last: &str, you| AccountRow {
        name: name.into(),
        email: email.into(),
        role,
        disabled,
        temporary,
        last_login: last.into(),
        is_you: you,
        manageable: !you,
    };
    vec![
        a(
            "Alan Araujo",
            "alanvitoraraujo1a@outlook.com",
            OrgRole::Owner,
            false,
            false,
            "hoje às 09:12",
            true,
        ),
        a(
            "Bruno Lima",
            "bruno.lima@example.com",
            OrgRole::Technician,
            false,
            false,
            "ontem às 18:40",
            false,
        ),
        a(
            "Carla Souza",
            "carla@example.com",
            OrgRole::Technician,
            false,
            true,
            "nunca",
            false,
        ),
        a(
            "Diego Rocha",
            "diego.rocha@example.com",
            OrgRole::Admin,
            true,
            false,
            "03/10 14:05",
            false,
        ),
    ]
}

fn admin_shot(name: &str, mode: Theme, tab: AdminTab, extra: fn(&egui::Context)) {
    let rows = accounts();
    let sessions = vec![
        SessionRow {
            host: "Recepção".into(),
            host_id: "312 604 958".into(),
            technician: "Bruno Lima".into(),
            state: "Conectado".into(),
            tone: Tone::Success,
            live: true,
            when: "hoje às 10:02 · 14 min".into(),
        },
        SessionRow {
            host: "Financeiro".into(),
            host_id: "590 221 734".into(),
            technician: "Alan Araujo".into(),
            state: "Aguardando autorização".into(),
            tone: Tone::Warning,
            live: true,
            when: "hoje às 10:15".into(),
        },
        SessionRow {
            host: "Caixa 2".into(),
            host_id: "468 930 115".into(),
            technician: "Bruno Lima".into(),
            state: "Encerrada".into(),
            tone: Tone::Neutral,
            live: false,
            when: "ontem às 16:20 · 32 min".into(),
        },
        SessionRow {
            host: "Notebook Marina".into(),
            host_id: "703 118 462".into(),
            technician: "Carla Souza".into(),
            state: "Recusada".into(),
            tone: Tone::Danger,
            live: false,
            when: "ontem às 11:03".into(),
        },
    ];
    let history = vec![
        HistoryRow {
            when: "11/10 10:15".into(),
            text: "Alan Araujo pediu acesso a Financeiro".into(),
            tone: Tone::Success,
        },
        HistoryRow {
            when: "11/10 10:02".into(),
            text: "Acesso a Recepção permitido".into(),
            tone: Tone::Success,
        },
        HistoryRow {
            when: "11/10 09:58".into(),
            text: "Senha errada ao entrar na conta de Carla Souza".into(),
            tone: Tone::Danger,
        },
        HistoryRow {
            when: "11/10 09:40".into(),
            text: "Alan Araujo criou a conta de Carla Souza".into(),
            tone: Tone::Neutral,
        },
        HistoryRow {
            when: "11/10 09:12".into(),
            text: "Alan Araujo entrou".into(),
            tone: Tone::Neutral,
        },
        HistoryRow {
            when: "10/10 18:40".into(),
            text: "Alan Araujo desativou a conta de Diego Rocha".into(),
            tone: Tone::Neutral,
        },
    ];
    shot(name, mode, WINDOW, move |ui| {
        let roles = [
            OrgRole::Technician,
            OrgRole::Admin,
            OrgRole::Viewer,
            OrgRole::Owner,
        ];
        let m = AdminModel {
            tab,
            org_name: "Aionix",
            accounts: Some(&rows),
            sessions: Some(&sessions),
            history: Some(&history),
            error: None,
            loading: false,
            assignable: &roles,
        };
        views::shell(ui, &side(Page::Admin, true, UpdateModel::Idle), |ui| {
            admin_views::admin_page(ui, &m);
        });
        extra(ui.ctx());
    });
}

fn home(
    name: &str,
    mode: Theme,
    h: HostModel,
    signed_in: bool,
    update: fn() -> UpdateModel<'static>,
) {
    let devs = devices();
    let quick: Vec<DeviceModel> = devs
        .into_iter()
        .filter(|d| d.presence == Presence::Online)
        .collect();
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
            views::home_page(
                ui,
                &h,
                |ui| {
                    views::connect_section(ui, &connect, &mut typed);
                },
                |ui| {
                    views::disclosure(ui, "Detalhes técnicos", |_| {});
                },
            );
        });
    });
}

#[test]
#[ignore = "renders with the GPU; run explicitly for design review"]
fn preview_admin() {
    for mode in [Theme::Light, Theme::Dark] {
        admin_shot("admin-accounts", mode, AdminTab::Accounts, |_| {});
        admin_shot("admin-sessions", mode, AdminTab::Sessions, |_| {});
        admin_shot("admin-history", mode, AdminTab::History, |_| {});
        admin_shot("admin-new-account", mode, AdminTab::Accounts, |ctx| {
            let mut form = NewAccountForm {
                name: "Carla Souza".into(),
                email: "carla@example.com".into(),
                role: OrgRole::Technician,
            };
            admin_views::new_account_sheet(
                ctx,
                &mut form,
                &[OrgRole::Technician, OrgRole::Admin, OrgRole::Viewer],
                false,
                None,
            );
        });
        admin_shot("admin-secret", mode, AdminTab::Accounts, |ctx| {
            admin_views::secret_sheet(
                ctx,
                "Conta criada",
                "Carla Souza",
                "carla@example.com",
                "Kd7m-Qx2p-Hn4w-Za9r",
                false,
            );
        });
        admin_shot("admin-confirm", mode, AdminTab::Accounts, |ctx| {
            admin_views::confirm_sheet(
                ctx,
                "Desativar a conta de Diego Rocha?",
                "A pessoa é desconectada, as sessões dela são encerradas e ela não consegue mais entrar. Você pode reativar depois.",
                "Desativar conta",
                true,
                false,
            );
        });
        admin_shot("password-mandatory", mode, AdminTab::Accounts, |ctx| {
            let mut form = PasswordForm::default();
            admin_views::password_sheet(ctx, &mut form, true, false, None);
        });
    }
}

#[test]
#[ignore = "renders with the GPU; run explicitly for design review"]
fn preview() {
    for mode in [Theme::Light, Theme::Dark] {
        home("home", mode, host(HostStatus::Online), true, || {
            UpdateModel::Idle
        });
        home("signed-out", mode, host(HostStatus::Online), false, || {
            UpdateModel::UpToDate
        });

        // Unattended access (ADR-0015): the owner's switch is on and the session is shown.
        let mut h = host(HostStatus::Online);
        h.status_text = "Em sessão".into();
        h.unattended_enabled = true;
        h.inbound = Some(InboundModel {
            name: "Técnica Ana".into(),
            email: None,
            permissions: "ver a tela e usar mouse e teclado".into(),
            duration: Some("02:10".into()),
            state: None,
            can_end: true,
            unattended: true,
        });
        home("unattended", mode, h, false, || UpdateModel::Idle);

        let mut h = host(HostStatus::Online);
        h.status_text = "Em sessão".into();
        h.inbound = Some(InboundModel {
            name: "Alan Araujo".into(),
            email: None,
            permissions: "ver a tela e usar mouse e teclado".into(),
            duration: Some("12:03".into()),
            state: None,
            can_end: true,
            unattended: false,
        });
        home("inbound", mode, h, false, || UpdateModel::Ready {
            version: "0.2.4",
            can_restart: false,
        });

        let mut h = host(HostStatus::Online);
        h.pending = Some("Alan Araujo".into());
        home("pending", mode, h, false, || UpdateModel::Downloading {
            version: "0.2.4",
            fraction: 0.42,
        });

        home("starting", mode, host(HostStatus::Starting), false, || {
            UpdateModel::Checking
        });
        home("offline", mode, host(HostStatus::Offline), true, || {
            UpdateModel::Failed { error: "404" }
        });

        // Computadores.
        let devs = devices();
        let mut filter = String::new();
        shot("computers", mode, WINDOW, move |ui| {
            let m = ComputersModel {
                devices: &devs,
                total: 6,
                online: 3,
                loaded: true,
                has_org: true,
                error: None,
                can_manage: true,
            };
            views::shell(ui, &side(Page::Computers, true, UpdateModel::Idle), |ui| {
                views::computers_page(ui, &m, &mut filter);
            });
        });

        // Editar computador and Clientes e grupos (managers, ADR-0015).
        let customers = vec![
            (Uuid::from_u128(1), "Clínica Sul".to_owned()),
            (Uuid::from_u128(2), "Mercado Norte".to_owned()),
        ];
        let groups = vec![(Uuid::from_u128(3), "Servidores".to_owned())];
        let members = vec![(Uuid::from_u128(4), "Técnica Ana".to_owned())];
        let mut fields = views::DeviceFields {
            name: "Servidor 01".into(),
            description: "Arquivos e backup".into(),
            customer: Some(Uuid::from_u128(1)),
            group: None,
            assignee: Some(Uuid::from_u128(4)),
            favorite: true,
            password: String::new(),
        };
        shot("device-edit", mode, WINDOW, move |ui| {
            let m = views::DeviceEditModel {
                id_text: "845 948 277",
                customers: &customers,
                groups: &groups,
                members: &members,
                password_set: true,
                unattended_enabled: true,
                working: None,
                error: None,
                notice: Some("Alterações salvas."),
            };
            views::device_edit_sheet(ui.ctx(), &m, &mut fields);
        });
        let org_customers = vec![
            (Uuid::from_u128(1), "Clínica Sul".to_owned(), 3),
            (Uuid::from_u128(2), "Mercado Norte".to_owned(), 0),
        ];
        let org_groups = vec![(Uuid::from_u128(3), "Servidores".to_owned(), 1)];
        let (mut new_customer, mut new_group) = (String::new(), String::new());
        shot("organize", mode, WINDOW, move |ui| {
            let m = views::OrganizeModel {
                customers: &org_customers,
                groups: &org_groups,
                working: None,
                error: None,
            };
            views::organize_sheet(ui.ctx(), &m, &mut new_customer, &mut new_group);
        });

        // Sign-in sheet over Início.
        let (mut email, mut password) = ("alan@aionixdev.com".to_owned(), String::new());
        let h = host(HostStatus::Online);
        let mut typed = "312 604 958".to_owned();
        shot("login", mode, WINDOW, move |ui| {
            let connect = ConnectModel {
                signed_in: false,
                signing_in: false,
                error: None,
                quick: &[],
                total: 0,
                stale: false,
            };
            views::shell(ui, &side(Page::Home, false, UpdateModel::Idle), |ui| {
                views::home_page(
                    ui,
                    &h,
                    |ui| {
                        views::connect_section(ui, &connect, &mut typed);
                    },
                    |_| {},
                );
            });
            let m = LoginModel {
                working: None,
                error: None,
                then_connect: Some("312 604 958".into()),
            };
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
            views::session_toolbar(
                &mut ui.new_child(egui::UiBuilder::new().max_rect(bar)),
                &m,
                &mut control,
                &mut diag,
            );
            let area = egui::Rect::from_min_max(egui::pos2(full.min.x, bar.max.y), full.max);
            ui.painter().rect_filled(area, 0.0, theme::palette(ui).bg);
            views::session_overlay(
                ui,
                area,
                &OverlayModel {
                    headline: "Aguardando autorização…",
                    detail: "O usuário de Recepção precisa clicar em Permitir.",
                    busy: true,
                    tone: Tone::Warning,
                },
            );
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
            views::session_toolbar(
                &mut ui.new_child(egui::UiBuilder::new().max_rect(bar)),
                &m,
                &mut control,
                &mut diag,
            );
            let area = egui::Rect::from_min_max(egui::pos2(full.min.x, bar.max.y), full.max);
            ui.painter()
                .rect_filled(area, 0.0, egui::Color32::from_rgb(11, 11, 12));
            let screen = egui::Rect::from_center_size(
                area.center(),
                egui::vec2(area.height() * 16.0 / 9.0, area.height()),
            );
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_rgb(0x2b, 0x4a, 0x6f));
            ui.painter().rect_filled(
                egui::Rect::from_min_max(egui::pos2(screen.min.x, screen.max.y - 28.0), screen.max),
                0.0,
                egui::Color32::from_rgb(0x1f, 0x22, 0x28),
            );
        });
    }
}
