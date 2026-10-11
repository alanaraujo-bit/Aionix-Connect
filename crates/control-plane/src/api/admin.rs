//! Organization administration from the app's admin panel: accounts (create with a
//! temporary password, reset the password, sign out everywhere, disable, enable) and the
//! organization's sessions (list, terminate).
//!
//! Rules (each one covered by tests/admin.rs):
//! * every route checks one permission in the organization of the path;
//! * account-wide actions only reach members of that organization who belong to no other
//!   organization (an admin here cannot take over someone else's account), never the
//!   caller, and never someone of equal or higher role unless the caller is an owner;
//! * a new account's role cannot exceed the caller's; only owners grant `owner`;
//! * the last enabled owner of an organization can be neither disabled nor demoted;
//! * temporary passwords are generated here, returned once, and never logged or audited;
//! * disabling or signing out an account also ends its live remote sessions.

use aionix_control_api::{
    rest::{
        CreateAccountRequest, CreatedAccount, Member, OrgRole, SessionList, SessionRecord,
        SignOutResult, TemporaryPassword,
    },
    signal::ServerMessage,
};
use aionix_shared::{AccessMode, FriendlyId};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::{AssertSqlSafe, PgPool};
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use super::{AuthUser, ClientCtx, clean_text, limit, orgs::require};
use crate::{
    audit::{AuditRecord, Outcome, action},
    authz::Permission,
    bus::DeviceBusMessage,
    error::{ApiFailure, ApiResult},
    password,
    ratelimit::limits,
    state::AppState,
};

/// Session states that still hold (or may soon hold) a live connection.
const LIVE: &str = "'requested', 'offered', 'accepted', 'approved', 'connected'";

#[derive(sqlx::FromRow)]
struct MemberRow {
    id: Uuid,
    email: String,
    display_name: String,
    role: String,
    disabled_at: Option<OffsetDateTime>,
    must_change_password: bool,
    last_login_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

impl MemberRow {
    fn role(&self) -> OrgRole {
        OrgRole::parse(&self.role).unwrap_or(OrgRole::Viewer)
    }

    fn into_member(self) -> Member {
        Member {
            user_id: self.id,
            role: self.role(),
            email: self.email,
            display_name: self.display_name,
            disabled: self.disabled_at.is_some(),
            must_change_password: self.must_change_password,
            last_login_at: self.last_login_at,
            created_at: Some(self.created_at),
        }
    }
}

const MEMBER_COLUMNS: &str = "u.id, u.email, u.display_name, m.role, u.disabled_at,
    u.must_change_password, u.last_login_at, u.created_at";

/// Every member of `org`, for `GET /v1/orgs/{org}/members`.
pub(crate) async fn list_members(db: &PgPool, org: Uuid) -> Result<Vec<Member>, sqlx::Error> {
    let rows: Vec<MemberRow> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {MEMBER_COLUMNS} FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.org_id = $1 ORDER BY u.display_name, u.email"
    )))
    .bind(org)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(MemberRow::into_member).collect())
}

async fn member(db: &PgPool, org: Uuid, user: Uuid) -> Result<Option<MemberRow>, sqlx::Error> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {MEMBER_COLUMNS} FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.org_id = $1 AND m.user_id = $2"
    )))
    .bind(org)
    .bind(user)
    .fetch_optional(db)
    .await
}

pub(crate) async fn role_in(
    db: &PgPool,
    org: Uuid,
    user: Uuid,
) -> Result<Option<OrgRole>, sqlx::Error> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM memberships WHERE org_id = $1 AND user_id = $2")
            .bind(org)
            .bind(user)
            .fetch_optional(db)
            .await?;
    Ok(role.as_deref().and_then(OrgRole::parse))
}

/// Enabled owners of `org` other than `except`.
pub(crate) async fn other_enabled_owners(
    db: &PgPool,
    org: Uuid,
    except: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.org_id = $1 AND m.role = 'owner' AND m.user_id <> $2 AND u.disabled_at IS NULL",
    )
    .bind(org)
    .bind(except)
    .fetch_one(db)
    .await
}

/// The target of an account-wide action, after every rule in the module header.
async fn manageable(
    state: &AppState,
    ctx: &ClientCtx,
    caller: &AuthUser,
    org: Uuid,
    target: Uuid,
) -> ApiResult<MemberRow> {
    limit(
        state,
        "admin-user",
        &caller.user_id.to_string(),
        limits::ADMIN_PER_USER,
    )
    .await?;
    require(state, ctx, caller, org, Permission::MemberManage).await?;
    if target == caller.user_id {
        return Err(ApiFailure::forbidden(
            "use your own account settings for your account",
        ));
    }
    let row = member(&state.db, org, target)
        .await?
        .ok_or_else(|| ApiFailure::not_found("no such member"))?;
    let elsewhere: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM memberships WHERE user_id = $1 AND org_id <> $2)",
    )
    .bind(target)
    .bind(org)
    .fetch_one(&state.db)
    .await?;
    if elsewhere {
        return Err(ApiFailure::forbidden(
            "this account also belongs to another organization",
        ));
    }
    let caller_role = role_in(&state.db, org, caller.user_id)
        .await?
        .unwrap_or(OrgRole::Viewer);
    if caller_role != OrgRole::Owner && row.role() >= caller_role {
        return Err(ApiFailure::forbidden(
            "cannot manage an account of equal or higher role",
        ));
    }
    Ok(row)
}

/// Ends live sessions selected by `filter` (a WHERE fragment over `sessions` with `$1`
/// bound to `arg`), then tells both devices of each session to stop. Returns how many.
async fn end_sessions(
    state: &AppState,
    filter: &str,
    arg: Uuid,
    reason: &str,
) -> Result<Vec<(Uuid, Uuid, Uuid)>, sqlx::Error> {
    let ended: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(AssertSqlSafe(format!(
        "UPDATE sessions SET status = 'ended', ended_at = now(), end_reason = $2
         WHERE {filter} AND status IN ({LIVE})
         RETURNING id, host_device_id, technician_device_id"
    )))
    .bind(arg)
    .bind(reason)
    .fetch_all(&state.db)
    .await?;
    notify_ended(state, &ended).await;
    Ok(ended)
}

async fn notify_ended(state: &AppState, ended: &[(Uuid, Uuid, Uuid)]) {
    for &(sid, host, tech) in ended {
        let msg = DeviceBusMessage::Deliver {
            message: ServerMessage::SessionRevoked { session_id: sid },
        };
        for device in [host, tech] {
            if let Err(e) = state.bus.publish_device(device, &msg).await {
                warn!(error = %e, session_id = %sid, %device, "could not notify a device that its session ended");
            }
        }
    }
}

/// Revokes every login of `user`. Returns how many.
async fn revoke_logins(db: &PgPool, user: Uuid, reason: &str) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now(), revoked_reason = $2
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user)
    .bind(reason)
    .execute(db)
    .await?
    .rows_affected())
}

// ------------------------------------------------------------------ accounts

/// `POST /v1/orgs/{org}/accounts`
pub async fn create_account(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
    Json(req): Json<CreateAccountRequest>,
) -> ApiResult<Json<CreatedAccount>> {
    limit(
        &state,
        "admin-user",
        &user.user_id.to_string(),
        limits::ADMIN_PER_USER,
    )
    .await?;
    require(&state, &ctx, &user, org, Permission::MemberManage).await?;
    let caller_role = role_in(&state.db, org, user.user_id)
        .await?
        .ok_or_else(|| ApiFailure::forbidden("not a member"))?;
    if req.role > caller_role {
        return Err(ApiFailure::forbidden("cannot grant a role above your own"));
    }
    let email = req.email.trim().to_owned();
    let normalized = super::auth::normalize_email(&email);
    if !normalized.contains('@') || normalized.len() < 3 || normalized.len() > 320 {
        return Err(ApiFailure::bad_request("invalid e-mail"));
    }
    let name = clean_text(&req.display_name, 120);
    if name.is_empty() {
        return Err(ApiFailure::bad_request("the name cannot be empty"));
    }
    let temporary = password::temporary();
    let hash = password::hash(temporary.clone()).await;
    let id = Uuid::now_v7();
    let mut tx = state.db.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO users (id, email, email_normalized, display_name, password_hash, must_change_password)
         VALUES ($1, $2, $3, $4, $5, true)",
    )
    .bind(id)
    .bind(&email)
    .bind(&normalized)
    .bind(&name)
    .bind(hash)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        return Err(if crate::db::is_unique_violation(&e, None) {
            ApiFailure::conflict("an account with this e-mail already exists")
        } else {
            e.into()
        });
    }
    sqlx::query("INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(org)
        .bind(id)
        .bind(req.role.as_str())
        .execute(&mut *tx)
        .await?;
    AuditRecord::new(action::USER_CREATED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_user(id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"role": req.role.as_str(), "via": "admin_panel"}))
        .write(&mut *tx)
        .await?;
    tx.commit().await?;
    let member = member(&state.db, org, id)
        .await?
        .ok_or_else(ApiFailure::internal)?
        .into_member();
    Ok(Json(CreatedAccount {
        member,
        temporary_password: temporary,
    }))
}

/// `POST /v1/orgs/{org}/members/{user}/reset-password` — new temporary password, every
/// login of the account signed out.
pub async fn reset_password(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, target)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<TemporaryPassword>> {
    let row = manageable(&state, &ctx, &user, org, target).await?;
    let temporary = password::temporary();
    let hash = password::hash(temporary.clone()).await;
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE users SET password_hash = $2, must_change_password = true,
             failed_login_count = 0, locked_until = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(row.id)
    .bind(hash)
    .execute(&mut *tx)
    .await?;
    let revoked = revoke_logins_tx(&mut tx, row.id, "password_reset").await?;
    AuditRecord::new(action::USER_PASSWORD_RESET, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_user(row.id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"revoked_logins": revoked}))
        .write(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(TemporaryPassword {
        temporary_password: temporary,
    }))
}

async fn revoke_logins_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: Uuid,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now(), revoked_reason = $2
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user)
    .bind(reason)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// `POST /v1/orgs/{org}/members/{user}/sign-out` — every login of the account and its
/// live sessions end; the account stays enabled.
pub async fn sign_out(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, target)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<SignOutResult>> {
    let row = manageable(&state, &ctx, &user, org, target).await?;
    let revoked = revoke_logins(&state.db, row.id, "signed_out_by_admin").await?;
    let ended = end_sessions(
        &state,
        "technician_user_id = $1",
        row.id,
        "signed_out_by_admin",
    )
    .await?;
    AuditRecord::new(action::USER_SIGNED_OUT, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_user(row.id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"revoked_logins": revoked, "ended_sessions": ended.len()}))
        .write(&state.db)
        .await?;
    Ok(Json(SignOutResult {
        revoked_logins: revoked,
        ended_sessions: ended.len() as u64,
    }))
}

/// `POST /v1/orgs/{org}/members/{user}/disable`
pub async fn disable(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, target)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<SignOutResult>> {
    let row = manageable(&state, &ctx, &user, org, target).await?;
    if row.role() == OrgRole::Owner && other_enabled_owners(&state.db, org, row.id).await? == 0 {
        return Err(ApiFailure::forbidden("the last owner cannot be disabled"));
    }
    sqlx::query(
        "UPDATE users SET disabled_at = COALESCE(disabled_at, now()), updated_at = now() WHERE id = $1",
    )
    .bind(row.id)
    .execute(&state.db)
    .await?;
    let revoked = revoke_logins(&state.db, row.id, "account_disabled").await?;
    let ended = end_sessions(
        &state,
        "technician_user_id = $1",
        row.id,
        "account_disabled",
    )
    .await?;
    AuditRecord::new(action::USER_DISABLED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_user(row.id)
        .client(ctx.ip(), ctx.rid())
        .details(
            json!({"revoked_logins": revoked, "ended_sessions": ended.len(), "via": "admin_panel"}),
        )
        .write(&state.db)
        .await?;
    Ok(Json(SignOutResult {
        revoked_logins: revoked,
        ended_sessions: ended.len() as u64,
    }))
}

/// `POST /v1/orgs/{org}/members/{user}/enable`
pub async fn enable(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, target)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Member>> {
    let row = manageable(&state, &ctx, &user, org, target).await?;
    sqlx::query("UPDATE users SET disabled_at = NULL, updated_at = now() WHERE id = $1")
        .bind(row.id)
        .execute(&state.db)
        .await?;
    AuditRecord::new(action::USER_ENABLED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_user(row.id)
        .client(ctx.ip(), ctx.rid())
        .write(&state.db)
        .await?;
    let m = member(&state.db, org, row.id)
        .await?
        .ok_or_else(ApiFailure::internal)?
        .into_member();
    Ok(Json(m))
}

// ------------------------------------------------------------------ sessions

#[derive(Debug, Deserialize)]
pub struct SessionsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    host_device_id: Uuid,
    host_name: String,
    host_friendly_id: i32,
    technician_name: String,
    mode: String,
    status: String,
    transport: Option<String>,
    requested_at: OffsetDateTime,
    connected_at: Option<OffsetDateTime>,
    ended_at: Option<OffsetDateTime>,
}

/// `GET /v1/orgs/{org}/sessions?limit=` — the organization's sessions, live first.
pub async fn sessions(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
    Query(q): Query<SessionsQuery>,
) -> ApiResult<Json<SessionList>> {
    require(&state, &ctx, &user, org, Permission::SessionViewAll).await?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows: Vec<SessionRow> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT s.id, s.host_device_id,
                COALESCE(NULLIF(d.display_name, ''), NULLIF(d.hostname, ''), 'Computador') AS host_name,
                d.friendly_id AS host_friendly_id, u.display_name AS technician_name,
                s.mode, s.status, s.transport, s.requested_at, s.connected_at, s.ended_at
         FROM sessions s
         JOIN devices d ON d.id = s.host_device_id
         JOIN users u ON u.id = s.technician_user_id
         WHERE s.org_id = $1
         ORDER BY (s.status IN ({LIVE})) DESC, s.requested_at DESC
         LIMIT $2"
    )))
    .bind(org)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(SessionList {
        sessions: rows
            .into_iter()
            .filter_map(|r| {
                Some(SessionRecord {
                    id: r.id,
                    host_device_id: r.host_device_id,
                    host_name: r.host_name,
                    host_friendly_id: FriendlyId::new(r.host_friendly_id as u32).ok()?,
                    technician_name: r.technician_name,
                    mode: AccessMode::parse(&r.mode).unwrap_or(AccessMode::Assisted),
                    status: r.status,
                    transport: r
                        .transport
                        .as_deref()
                        .and_then(super::sessions::parse_transport),
                    requested_at: r.requested_at,
                    connected_at: r.connected_at,
                    ended_at: r.ended_at,
                })
            })
            .collect(),
    }))
}

/// `POST /v1/orgs/{org}/sessions/{id}/terminate` — ends a live session of the
/// organization; both computers are told to stop.
pub async fn terminate(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, session)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    limit(
        &state,
        "admin-user",
        &user.user_id.to_string(),
        limits::ADMIN_PER_USER,
    )
    .await?;
    require(&state, &ctx, &user, org, Permission::SessionTerminate).await?;
    let owner: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT org_id FROM sessions WHERE id = $1")
            .bind(session)
            .fetch_optional(&state.db)
            .await?;
    if owner.flatten() != Some(org) {
        return Err(ApiFailure::not_found("no such session"));
    }
    let ended = end_sessions(&state, "id = $1", session, "terminated_by_admin").await?;
    if ended.is_empty() {
        return Err(ApiFailure::conflict("the session has already ended"));
    }
    AuditRecord::new(action::SESSION_TERMINATED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .session(session)
        .target_device(ended[0].1)
        .client(ctx.ip(), ctx.rid())
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
