//! Organization-scoped endpoints. Each checks one permission in the path's organization.

use aionix_control_api::rest::{
    AuditEvent, AuditList, CreateEnrollmentTokenRequest, DeviceList, EnrollmentToken, Member,
    MemberList, OrgRole, UpsertMemberRequest,
};
use aionix_crypto::{random_token, sha256_hex};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::Deserialize;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{AuthUser, ClientCtx};
use crate::{
    audit::{AuditRecord, Outcome, action},
    authz::{self, Permission},
    devices,
    error::{ApiFailure, ApiResult},
    state::AppState,
};

/// Fails with 403 (and a `permission.denied` audit record) unless the caller holds
/// `perm` in `org`.
pub(crate) async fn require(
    state: &AppState,
    ctx: &ClientCtx,
    user: &AuthUser,
    org: Uuid,
    perm: Permission,
) -> ApiResult<()> {
    if authz::has_permission(&state.db, user.user_id, org, perm).await? {
        return Ok(());
    }
    AuditRecord::new(action::PERMISSION_DENIED, Outcome::Denied)
        .user(user.user_id)
        .org(Some(org))
        .client(ctx.ip(), ctx.rid())
        .details(json!({"permission": perm.as_str()}))
        .write(&state.db)
        .await?;
    Err(ApiFailure::forbidden(format!(
        "missing permission {}",
        perm.as_str()
    )))
}

/// `GET /v1/orgs/{org}/devices`
pub async fn devices(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<DeviceList>> {
    require(&state, &ctx, &user, org, Permission::DeviceView).await?;
    let rows = devices::by_org(&state.db, org).await?;
    let ids: Vec<Uuid> = rows.iter().map(|d| d.id).collect();
    let presence = state.presence.get_many(&ids).await?;
    Ok(Json(DeviceList {
        devices: rows
            .iter()
            .map(|d| d.summary(presence.get(&d.id)))
            .collect(),
    }))
}

/// `POST /v1/orgs/{org}/enrollment-tokens` — the token is returned once; only its
/// hash is stored.
pub async fn create_enrollment_token(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
    Json(req): Json<CreateEnrollmentTokenRequest>,
) -> ApiResult<Json<EnrollmentToken>> {
    require(&state, &ctx, &user, org, Permission::EnrollmentCreate).await?;
    if !(1..=720).contains(&req.valid_hours) || !(1..=1000).contains(&req.max_uses) {
        return Err(ApiFailure::bad_request(
            "valid_hours must be 1..=720 and max_uses 1..=1000",
        ));
    }
    if let Some(c) = req.customer_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM customers WHERE id = $1 AND org_id = $2)",
        )
        .bind(c)
        .bind(org)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(ApiFailure::bad_request("unknown customer"));
        }
    }
    let (id, token, expires_at) = crate::admin::insert_enrollment_token(
        &state.db,
        org,
        req.customer_id,
        req.valid_hours,
        req.max_uses,
        Some(user.user_id),
    )
    .await?;
    AuditRecord::new(action::ENROLLMENT_TOKEN_CREATED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .client(ctx.ip(), ctx.rid())
        .details(json!({"token_id": id, "valid_hours": req.valid_hours, "max_uses": req.max_uses}))
        .write(&state.db)
        .await?;
    Ok(Json(EnrollmentToken {
        id,
        token,
        expires_at,
        max_uses: req.max_uses,
    }))
}

/// Generates and stores an enrollment token (hash only).
pub(crate) fn new_enrollment_secret() -> (String, String) {
    let token = format!("axe1_{}", random_token(24));
    let hash = sha256_hex(token.as_bytes());
    (token, hash)
}

pub(crate) fn token_expiry(hours: i64) -> OffsetDateTime {
    OffsetDateTime::now_utc() + Duration::hours(hours)
}

/// `GET /v1/orgs/{org}/members`
pub async fn members(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<MemberList>> {
    require(&state, &ctx, &user, org, Permission::MemberView).await?;
    let rows: Vec<(Uuid, String, String, String, Option<OffsetDateTime>)> = sqlx::query_as(
        "SELECT u.id, u.email, u.display_name, m.role, u.disabled_at
         FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.org_id = $1 ORDER BY u.display_name, u.email",
    )
    .bind(org)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(MemberList {
        members: rows
            .into_iter()
            .filter_map(|(id, email, name, role, disabled)| {
                OrgRole::parse(&role).map(|role| Member {
                    user_id: id,
                    email,
                    display_name: name,
                    role,
                    disabled: disabled.is_some(),
                })
            })
            .collect(),
    }))
}

/// `PUT /v1/orgs/{org}/members` — adds an existing account or changes its role.
/// Nobody changes their own role; only owners grant or remove `owner`.
pub async fn upsert_member(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
    Json(req): Json<UpsertMemberRequest>,
) -> ApiResult<Json<Member>> {
    require(&state, &ctx, &user, org, Permission::MemberManage).await?;
    let email = super::auth::normalize_email(&req.email);
    let target: Option<(Uuid, String, String, Option<OffsetDateTime>)> = sqlx::query_as(
        "SELECT id, email, display_name, disabled_at FROM users WHERE email_normalized = $1",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    let (target_id, target_email, target_name, disabled) =
        target.ok_or_else(|| ApiFailure::not_found("no account with this e-mail"))?;
    if target_id == user.user_id {
        return Err(ApiFailure::forbidden("cannot change your own role"));
    }
    let caller_role: Option<String> =
        sqlx::query_scalar("SELECT role FROM memberships WHERE org_id = $1 AND user_id = $2")
            .bind(org)
            .bind(user.user_id)
            .fetch_optional(&state.db)
            .await?;
    let current: Option<String> =
        sqlx::query_scalar("SELECT role FROM memberships WHERE org_id = $1 AND user_id = $2")
            .bind(org)
            .bind(target_id)
            .fetch_optional(&state.db)
            .await?;
    let caller_is_owner = caller_role.as_deref() == Some("owner");
    if (req.role == OrgRole::Owner || current.as_deref() == Some("owner")) && !caller_is_owner {
        return Err(ApiFailure::forbidden("only owners manage owners"));
    }
    crate::admin::set_membership(&state.db, org, target_id, req.role).await?;
    AuditRecord::new(action::MEMBER_UPSERTED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_user(target_id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"role": req.role.as_str(), "previous_role": current}))
        .write(&state.db)
        .await?;
    Ok(Json(Member {
        user_id: target_id,
        email: target_email,
        display_name: target_name,
        role: req.role,
        disabled: disabled.is_some(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    /// Only events with `id < before` (pagination).
    #[serde(default)]
    pub before: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct AuditRow {
    id: i64,
    occurred_at: OffsetDateTime,
    category: String,
    action: String,
    outcome: String,
    actor_user_id: Option<Uuid>,
    actor_device_id: Option<Uuid>,
    target_device_id: Option<Uuid>,
    session_id: Option<Uuid>,
    request_id: Option<String>,
    details: serde_json::Value,
}

/// `GET /v1/orgs/{org}/audit?limit=&before=`
pub async fn audit(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<AuditList>> {
    require(&state, &ctx, &user, org, Permission::AuditView).await?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows: Vec<AuditRow> = sqlx::query_as(
        "SELECT id, occurred_at, category, action, outcome, actor_user_id, actor_device_id,
                target_device_id, session_id, request_id, details
         FROM audit_events WHERE org_id = $1 AND ($2::bigint IS NULL OR id < $2)
         ORDER BY id DESC LIMIT $3",
    )
    .bind(org)
    .bind(q.before)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(AuditList {
        events: rows
            .into_iter()
            .map(|r| AuditEvent {
                id: r.id,
                occurred_at: r.occurred_at,
                category: r.category,
                action: r.action,
                outcome: r.outcome,
                actor_user_id: r.actor_user_id,
                actor_device_id: r.actor_device_id,
                target_device_id: r.target_device_id,
                session_id: r.session_id,
                request_id: r.request_id,
                details: r.details,
            })
            .collect(),
    }))
}
