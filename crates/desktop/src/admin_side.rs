//! The administration panel's state and server calls (owners and admins). Screens are in
//! admin_views.rs; the server enforces every rule, so a refused request only shows its
//! reason here.

use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use aionix_client::{ApiClient, ClientError, authority::UserSession};
use aionix_control_api::{
    ErrorCode,
    rest::{AuditEvent, CreateAccountRequest, Member, OrgRole, SessionRecord},
};
use time::{OffsetDateTime, UtcOffset, format_description::FormatItem, macros::format_description};
use uuid::Uuid;

use crate::{
    admin_views::{
        self, AccountRow, AdminAction, AdminModel, AdminTab, HistoryRow, NewAccountForm,
        SecretAction, SessionRow, SheetAction,
    },
    common,
    views::Tone,
};

/// What the panel needs from the signed-in technician.
#[derive(Clone)]
pub struct AdminHandle {
    pub api: ApiClient,
    pub user: Arc<UserSession>,
    pub me: Uuid,
    pub org: Uuid,
    pub org_name: String,
    pub role: OrgRole,
}

impl AdminHandle {
    /// Roles this account may hand out.
    fn assignable(&self) -> Vec<OrgRole> {
        [
            OrgRole::Technician,
            OrgRole::Admin,
            OrgRole::Viewer,
            OrgRole::Owner,
        ]
        .into_iter()
        .filter(|r| *r <= self.role)
        .collect()
    }
}

#[derive(Default)]
struct Data {
    members: Option<Vec<Member>>,
    sessions: Option<Vec<SessionRecord>>,
    audit: Option<Vec<AuditEvent>>,
    error: Option<String>,
    loading: u32,
}

enum Task {
    Idle,
    Working,
    Failed(String),
}

enum Op {
    Reset {
        user: Uuid,
        name: String,
        email: String,
    },
    SignOut {
        user: Uuid,
        name: String,
    },
    Disable {
        user: Uuid,
        name: String,
    },
    EndSession {
        id: Uuid,
        host: String,
    },
}

enum Sheet {
    NewAccount,
    Confirm(Op),
}

struct Secret {
    title: &'static str,
    name: String,
    email: String,
    password: String,
}

pub struct AdminSide {
    rt: tokio::runtime::Handle,
    tab: AdminTab,
    data: Arc<Mutex<Data>>,
    loaded: Option<(AdminTab, Instant)>,
    sheet: Option<Sheet>,
    form: NewAccountForm,
    task: Arc<Mutex<Task>>,
    /// A temporary password to show once (filled by a background task).
    secret_in: Arc<Mutex<Option<Secret>>>,
    secret: Option<Secret>,
    copied_at: Option<Instant>,
    /// Close the open sheet (set by a background task that succeeded).
    close_sheet: Arc<Mutex<bool>>,
}

impl AdminSide {
    pub fn new(rt: &tokio::runtime::Handle) -> Self {
        Self {
            rt: rt.clone(),
            tab: AdminTab::Accounts,
            data: Arc::default(),
            loaded: None,
            sheet: None,
            form: NewAccountForm::default(),
            task: Arc::new(Mutex::new(Task::Idle)),
            secret_in: Arc::default(),
            secret: None,
            copied_at: None,
            close_sheet: Arc::default(),
        }
    }

    /// A sheet is open (typing or confirming): updates wait.
    pub fn busy(&self) -> bool {
        self.sheet.is_some() || self.secret.is_some()
    }

    /// Signed out: forget everything shown.
    pub fn clear(&mut self) {
        *self.data.lock().unwrap() = Data::default();
        self.loaded = None;
        self.sheet = None;
        self.secret = None;
        *self.secret_in.lock().unwrap() = None;
    }

    fn load(&mut self, h: &AdminHandle, ctx: &egui::Context, tab: AdminTab) {
        self.loaded = Some((tab, Instant::now()));
        let (h, data, ctx) = (h.clone(), self.data.clone(), ctx.clone());
        data.lock().unwrap().loading += 1;
        self.rt.spawn(async move {
            let result: Result<(), ClientError> = async {
                let access = h.user.access_token().await?;
                match tab {
                    AdminTab::Accounts => {
                        let m = h.api.org_members(&access, h.org).await?;
                        data.lock().unwrap().members = Some(m.members);
                    }
                    AdminTab::Sessions => {
                        let s = h.api.org_sessions(&access, h.org).await?;
                        data.lock().unwrap().sessions = Some(s.sessions);
                    }
                    AdminTab::History => {
                        let a = h.api.org_audit(&access, h.org, 300).await?;
                        data.lock().unwrap().audit = Some(a.events);
                    }
                }
                Ok(())
            }
            .await;
            let mut d = data.lock().unwrap();
            d.loading = d.loading.saturating_sub(1);
            d.error = result.err().map(|e| error_text(&e));
            ctx.request_repaint();
        });
    }

    /// The administration page.
    pub fn page(&mut self, ui: &mut egui::Ui, h: &AdminHandle) {
        let ctx = ui.ctx().clone();
        // Fresh data when the tab is shown, and live sessions every few seconds.
        let stale = match self.loaded {
            None => true,
            Some((tab, at)) => {
                tab != self.tab
                    || (self.tab == AdminTab::Sessions && at.elapsed() > Duration::from_secs(5))
            }
        };
        if stale {
            self.load(h, &ctx, self.tab);
        }
        if self.tab == AdminTab::Sessions {
            ctx.request_repaint_after(Duration::from_secs(5));
        }
        let d = self.data.lock().unwrap();
        let accounts = d.members.as_ref().map(|m| account_rows(m, h));
        let sessions = d.sessions.as_ref().map(|s| session_rows(s));
        let history = d.audit.as_ref().map(|a| history_rows(a));
        let assignable = h.assignable();
        let m = AdminModel {
            tab: self.tab,
            org_name: &h.org_name,
            accounts: accounts.as_deref(),
            sessions: sessions.as_deref(),
            history: history.as_deref(),
            error: d.error.as_deref(),
            loading: d.loading > 0,
            assignable: &assignable,
        };
        let action = admin_views::admin_page(ui, &m);
        let members = d.members.clone().unwrap_or_default();
        let session_list = d.sessions.clone().unwrap_or_default();
        drop(d);
        match action {
            Some(AdminAction::Tab(t)) => {
                self.tab = t;
                self.load(h, &ctx, t);
            }
            Some(AdminAction::Refresh) => self.load(h, &ctx, self.tab),
            Some(AdminAction::NewAccount) => {
                self.form = NewAccountForm::default();
                *self.task.lock().unwrap() = Task::Idle;
                self.sheet = Some(Sheet::NewAccount);
            }
            Some(AdminAction::Role(i, role)) => {
                if let Some(m) = members.get(i) {
                    let email = m.email.clone();
                    self.run(h, &ctx, move |h, access| async move {
                        h.api.set_member_role(&access, h.org, &email, role).await?;
                        Ok(None)
                    });
                }
            }
            Some(AdminAction::Enable(i)) => {
                if let Some(m) = members.get(i) {
                    let user = m.user_id;
                    self.run(h, &ctx, move |h, access| async move {
                        h.api.enable_member(&access, h.org, user).await?;
                        Ok(None)
                    });
                }
            }
            Some(AdminAction::Reset(i)) => {
                if let Some(m) = members.get(i) {
                    self.confirm(Op::Reset {
                        user: m.user_id,
                        name: m.display_name.clone(),
                        email: m.email.clone(),
                    });
                }
            }
            Some(AdminAction::SignOut(i)) => {
                if let Some(m) = members.get(i) {
                    self.confirm(Op::SignOut {
                        user: m.user_id,
                        name: m.display_name.clone(),
                    });
                }
            }
            Some(AdminAction::Disable(i)) => {
                if let Some(m) = members.get(i) {
                    self.confirm(Op::Disable {
                        user: m.user_id,
                        name: m.display_name.clone(),
                    });
                }
            }
            Some(AdminAction::EndSession(i)) => {
                if let Some(s) = session_list.get(i) {
                    self.confirm(Op::EndSession {
                        id: s.id,
                        host: s.host_name.clone(),
                    });
                }
            }
            None => {}
        }
    }

    fn confirm(&mut self, op: Op) {
        *self.task.lock().unwrap() = Task::Idle;
        self.sheet = Some(Sheet::Confirm(op));
    }

    /// Runs one server call in the background; on success the open sheet closes, an
    /// optional temporary password is shown, and the current tab reloads.
    fn run<F, Fut>(&mut self, h: &AdminHandle, ctx: &egui::Context, f: F)
    where
        F: FnOnce(AdminHandle, String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Option<Secret>, ClientError>> + Send,
    {
        *self.task.lock().unwrap() = Task::Working;
        let (h, task, secret, close, data, ctx) = (
            h.clone(),
            self.task.clone(),
            self.secret_in.clone(),
            self.close_sheet.clone(),
            self.data.clone(),
            ctx.clone(),
        );
        self.loaded = None;
        self.rt.spawn(async move {
            let result = match h.user.access_token().await {
                Ok(access) => f(h, access).await,
                Err(e) => Err(e),
            };
            match result {
                Ok(s) => {
                    *task.lock().unwrap() = Task::Idle;
                    *secret.lock().unwrap() = s;
                    *close.lock().unwrap() = true;
                }
                Err(e) => {
                    let text = error_text(&e);
                    data.lock().unwrap().error = Some(text.clone());
                    *task.lock().unwrap() = Task::Failed(text);
                }
            }
            ctx.request_repaint();
        });
    }

    /// The sheets the page opened. Call every frame (after the page).
    pub fn sheets(&mut self, ctx: &egui::Context, h: &AdminHandle) {
        if std::mem::take(&mut *self.close_sheet.lock().unwrap()) {
            self.sheet = None;
            self.loaded = None;
        }
        if let Some(s) = self.secret_in.lock().unwrap().take() {
            self.secret = Some(s);
            self.copied_at = None;
        }
        let (working, failed) = match &*self.task.lock().unwrap() {
            Task::Working => (true, None),
            Task::Failed(e) => (false, Some(e.clone())),
            Task::Idle => (false, None),
        };
        match &self.sheet {
            Some(Sheet::NewAccount) => {
                let roles = h.assignable();
                match admin_views::new_account_sheet(
                    ctx,
                    &mut self.form,
                    &roles,
                    working,
                    failed.as_deref(),
                ) {
                    Some(SheetAction::Submit) => {
                        let req = CreateAccountRequest {
                            email: self.form.email.trim().to_owned(),
                            display_name: self.form.name.trim().to_owned(),
                            role: self.form.role,
                        };
                        self.run(h, ctx, move |h, access| async move {
                            let created = h.api.create_account(&access, h.org, &req).await?;
                            Ok(Some(Secret {
                                title: "Conta criada",
                                name: created.member.display_name,
                                email: created.member.email,
                                password: created.temporary_password,
                            }))
                        });
                    }
                    Some(SheetAction::Cancel) => self.sheet = None,
                    None => {}
                }
            }
            Some(Sheet::Confirm(op)) => {
                let (title, body, confirm, danger) = match op {
                    Op::Reset { name, .. } => (
                        format!("Redefinir a senha de {name}?"),
                        "A senha atual deixa de valer, todos os acessos dessa conta são desconectados e você recebe uma senha temporária para enviar.".to_owned(),
                        "Redefinir senha",
                        false,
                    ),
                    Op::SignOut { name, .. } => (
                        format!("Desconectar {name}?"),
                        "Todos os computadores em que essa conta entrou são desconectados e as sessões dela são encerradas. A pessoa pode entrar de novo com a senha.".to_owned(),
                        "Desconectar",
                        false,
                    ),
                    Op::Disable { name, .. } => (
                        format!("Desativar a conta de {name}?"),
                        "A pessoa é desconectada, as sessões dela são encerradas e ela não consegue mais entrar. Você pode reativar depois.".to_owned(),
                        "Desativar conta",
                        true,
                    ),
                    Op::EndSession { host, .. } => (
                        format!("Encerrar a sessão em {host}?"),
                        "Os dois computadores são avisados e a conexão termina na hora.".to_owned(),
                        "Encerrar sessão",
                        true,
                    ),
                };
                match admin_views::confirm_sheet(ctx, &title, &body, confirm, danger, working) {
                    Some(SheetAction::Submit) => {
                        let op = match op {
                            Op::Reset { user, name, email } => Op::Reset {
                                user: *user,
                                name: name.clone(),
                                email: email.clone(),
                            },
                            Op::SignOut { user, name } => Op::SignOut {
                                user: *user,
                                name: name.clone(),
                            },
                            Op::Disable { user, name } => Op::Disable {
                                user: *user,
                                name: name.clone(),
                            },
                            Op::EndSession { id, host } => Op::EndSession {
                                id: *id,
                                host: host.clone(),
                            },
                        };
                        self.run(h, ctx, move |h, access| async move {
                            match op {
                                Op::Reset { user, name, email } => {
                                    let t =
                                        h.api.reset_member_password(&access, h.org, user).await?;
                                    Ok(Some(Secret {
                                        title: "Senha redefinida",
                                        name,
                                        email,
                                        password: t.temporary_password,
                                    }))
                                }
                                Op::SignOut { user, .. } => {
                                    h.api.sign_out_member(&access, h.org, user).await?;
                                    Ok(None)
                                }
                                Op::Disable { user, .. } => {
                                    h.api.disable_member(&access, h.org, user).await?;
                                    Ok(None)
                                }
                                Op::EndSession { id, .. } => {
                                    h.api.terminate_session(&access, h.org, id).await?;
                                    Ok(None)
                                }
                            }
                        });
                    }
                    Some(SheetAction::Cancel) => {
                        self.sheet = None;
                        *self.task.lock().unwrap() = Task::Idle;
                    }
                    None => {}
                }
            }
            None => {
                // Closed: the next one animates in again.
                crate::views::reset_sheet(ctx, "new-account");
                crate::views::reset_sheet(ctx, "confirm");
            }
        }
        if let Some(s) = &self.secret {
            let copied = self
                .copied_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
            match admin_views::secret_sheet(ctx, s.title, &s.name, &s.email, &s.password, copied) {
                Some(SecretAction::Copy) => {
                    ctx.copy_text(format!(
                        "E-mail: {}\nSenha temporária: {}",
                        s.email, s.password
                    ));
                    self.copied_at = Some(Instant::now());
                    ctx.request_repaint_after(Duration::from_millis(2050));
                }
                Some(SecretAction::Done) => {
                    self.secret = None;
                    crate::views::reset_sheet(ctx, "secret");
                }
                None => {}
            }
        }
    }
}

/// pt-BR text for a refused admin request.
fn error_text(e: &ClientError) -> String {
    match e {
        ClientError::Api {
            status: 404, code, ..
        } if *code != ErrorCode::NotFound => {
            "O servidor ainda não tem o painel de administração.".into()
        }
        ClientError::Api {
            code: ErrorCode::Conflict,
            detail,
            ..
        } => {
            if detail.contains("e-mail") {
                "Já existe uma conta com este e-mail.".into()
            } else {
                "A sessão já tinha terminado.".into()
            }
        }
        ClientError::Api {
            code: ErrorCode::Forbidden,
            detail,
            ..
        } => {
            if detail.contains("another organization") {
                "Essa conta também pertence a outra organização; ela só pode ser gerenciada lá."
                    .into()
            } else if detail.contains("last owner") {
                "A organização precisa de pelo menos um dono ativo.".into()
            } else if detail.contains("above your own") {
                "Você não pode dar um papel acima do seu.".into()
            } else {
                "Você não tem permissão para isso.".into()
            }
        }
        ClientError::Api {
            code: ErrorCode::BadRequest,
            ..
        } => "Confira o nome e o e-mail.".into(),
        other => common::client_error_text(other),
    }
}

// ------------------------------------------------------------------ rows

fn local_offset() -> UtcOffset {
    static OFFSET: OnceLock<UtcOffset> = OnceLock::new();
    *OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

const DAY_TIME: &[FormatItem<'static>] = format_description!("[day]/[month] [hour]:[minute]");
const TIME: &[FormatItem<'static>] = format_description!("[hour]:[minute]");

/// "hoje às 09:12", "ontem às 18:40", "03/10 14:05".
fn when(t: OffsetDateTime) -> String {
    let local = t.to_offset(local_offset());
    let today = OffsetDateTime::now_utc().to_offset(local_offset()).date();
    let time = local.format(TIME).unwrap_or_default();
    if local.date() == today {
        format!("hoje às {time}")
    } else if today.previous_day() == Some(local.date()) {
        format!("ontem às {time}")
    } else {
        local.format(DAY_TIME).unwrap_or_default()
    }
}

fn account_rows(members: &[Member], h: &AdminHandle) -> Vec<AccountRow> {
    members
        .iter()
        .map(|m| AccountRow {
            name: m.display_name.clone(),
            email: m.email.clone(),
            role: m.role,
            disabled: m.disabled,
            temporary: m.must_change_password,
            last_login: m.last_login_at.map(when).unwrap_or_else(|| "nunca".into()),
            is_you: m.user_id == h.me,
            manageable: h.role == OrgRole::Owner || m.role < h.role,
        })
        .collect()
}

fn session_rows(sessions: &[SessionRecord]) -> Vec<SessionRow> {
    sessions
        .iter()
        .map(|s| {
            let (state, tone, live) = match s.status.as_str() {
                "requested" | "offered" | "accepted" => {
                    ("Aguardando autorização", Tone::Warning, true)
                }
                "approved" => ("Conectando", Tone::Warning, true),
                "connected" => ("Conectado", Tone::Success, true),
                "ended" => ("Encerrada", Tone::Neutral, false),
                "rejected" | "denied" => ("Recusada", Tone::Danger, false),
                "expired" => ("Sem resposta", Tone::Neutral, false),
                _ => ("Falhou", Tone::Danger, false),
            };
            let mut at = when(s.requested_at);
            if let Some(start) = s.connected_at {
                let end = s.ended_at.unwrap_or_else(OffsetDateTime::now_utc);
                let mins = (end - start).whole_minutes().max(0);
                at.push_str(&if mins < 1 {
                    " · menos de 1 min".to_owned()
                } else {
                    format!(" · {mins} min")
                });
            }
            SessionRow {
                host: s.host_name.clone(),
                host_id: s.host_friendly_id.to_string(),
                technician: s.technician_name.clone(),
                state: state.into(),
                tone,
                live,
                when: at,
            }
        })
        .collect()
}

fn history_rows(events: &[AuditEvent]) -> Vec<HistoryRow> {
    let local = local_offset();
    events
        .iter()
        .filter_map(|e| {
            let text = describe(e)?;
            let tone = match e.outcome.as_str() {
                "denied" | "failure" => Tone::Danger,
                _ if e.action.starts_with("session.") => Tone::Success,
                _ => Tone::Neutral,
            };
            Some(HistoryRow {
                when: e
                    .occurred_at
                    .to_offset(local)
                    .format(DAY_TIME)
                    .unwrap_or_default(),
                text,
                tone,
            })
        })
        .collect()
}

/// One sentence per event; `None` hides internal steps.
fn describe(e: &AuditEvent) -> Option<String> {
    let actor = e.actor_name.clone().unwrap_or_else(|| "Alguém".into());
    let target = e.target_name.clone().unwrap_or_else(|| "uma conta".into());
    let ok = e.outcome == "success";
    let role = e
        .details
        .get("role")
        .and_then(|v| v.as_str())
        .and_then(OrgRole::parse)
        .map(|r| admin_views::role_label(r).to_lowercase());
    Some(match e.action.as_str() {
        "auth.login" if ok => format!("{actor} entrou"),
        "auth.login" if e.outcome == "denied" => {
            format!("{actor} não conseguiu entrar (conta desativada ou bloqueada)")
        }
        "auth.login" => match &e.actor_name {
            Some(a) => format!("Senha errada ao entrar na conta de {a}"),
            None => "Tentativa de entrar numa conta que não existe".into(),
        },
        "auth.logout" => format!("{actor} saiu"),
        "auth.logout_all" => format!("{actor} saiu de todos os computadores"),
        "auth.account_locked" => format!("Conta de {target} bloqueada por senhas erradas"),
        "auth.refresh_reuse" => format!("Login de {target} cancelado por uso suspeito"),
        "auth.password_changed" if ok => format!("{actor} alterou a própria senha"),
        "auth.password_changed" => format!("{actor} errou a senha atual ao tentar alterá-la"),
        "user.created" => match &e.actor_name {
            Some(_) => format!("{actor} criou a conta de {target}"),
            None => format!("Conta de {target} criada pelo servidor"),
        },
        "user.disabled" => format!("{actor} desativou a conta de {target}"),
        "user.enabled" => format!("{actor} reativou a conta de {target}"),
        "user.signed_out" => format!("{actor} desconectou {target} de todos os computadores"),
        "user.password_reset" => format!("{actor} redefiniu a senha de {target}"),
        "user.email_changed" => format!("E-mail da conta de {target} alterado"),
        "member.upserted" => match role {
            Some(r) => format!("{actor} tornou {target} {r}"),
            None => format!("{actor} mudou o papel de {target}"),
        },
        "session.requested" if ok => format!("{actor} pediu acesso a {target}"),
        "session.requested" => format!("{actor} teve o acesso a {target} negado"),
        "session.approved" => format!("Acesso a {target} permitido"),
        "session.rejected" => format!("Acesso a {target} recusado"),
        "session.ended" => format!("Sessão em {target} encerrada"),
        "session.terminated" => format!("{actor} encerrou uma sessão em {target}"),
        "permission.denied" => format!("{actor} tentou algo sem permissão"),
        "device.revoked" => format!("{actor} removeu o computador {target}"),
        "org.created" => "Organização criada".into(),
        "session.ticket_issued"
        | "session.offer_answered"
        | "device.enrolled"
        | "enrollment.token_created" => return None,
        other => other.to_owned(),
    })
}
