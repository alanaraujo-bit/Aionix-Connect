//! User authentication: password login, rotating refresh tokens with reuse detection,
//! logout and revocation (ADR-0009).

use aionix_control_api::{
    ErrorCode,
    claims::AccessClaims,
    rest::{
        AuthTokens, ChangePasswordRequest, LoginRequest, Membership, OrgRole, RefreshRequest,
        UserProfile,
    },
};
use aionix_crypto::{ct_eq, random_token, sha256_hex};
use axum::{Json, extract::State, http::StatusCode};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{AuthUser, ClientCtx, limit};
use crate::{
    audit::{AuditRecord, Outcome, action},
    error::{ApiFailure, ApiResult},
    password,
    ratelimit::limits,
    state::AppState,
};

/// Refresh tokens slide by this much on every use…
pub const REFRESH_TTL: Duration = Duration::days(14);
/// …but a login never lives longer than this.
pub const ABSOLUTE_SESSION_TTL: Duration = Duration::days(30);
/// Consecutive failed logins before the account is locked.
pub const MAX_FAILED_LOGINS: i32 = 10;
pub const LOCKOUT: Duration = Duration::minutes(15);

const REFRESH_PREFIX: &str = "axr1";

pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn new_refresh(sid: Uuid) -> (String, String) {
    let secret = random_token(32);
    let hash = sha256_hex(secret.as_bytes());
    (format!("{REFRESH_PREFIX}.{sid}.{secret}"), hash)
}

fn parse_refresh(token: &str) -> Option<(Uuid, String)> {
    let mut parts = token.trim().splitn(3, '.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(REFRESH_PREFIX), Some(sid), Some(secret))
            if !secret.is_empty() && secret.len() < 200 =>
        {
            Some((sid.parse().ok()?, sha256_hex(secret.as_bytes())))
        }
        _ => None,
    }
}

pub async fn load_profile(db: &PgPool, user_id: Uuid) -> Result<UserProfile, sqlx::Error> {
    let (email, display_name, must_change_password): (String, String, bool) = sqlx::query_as(
        "SELECT email, display_name, must_change_password FROM users WHERE id = $1",
    )
            .bind(user_id)
            .fetch_one(db)
            .await?;
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT o.id, o.name, m.role FROM memberships m
         JOIN organizations o ON o.id = m.org_id AND o.disabled_at IS NULL
         WHERE m.user_id = $1 ORDER BY o.name",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(UserProfile {
        id: user_id,
        email,
        display_name,
        must_change_password,
        memberships: rows
            .into_iter()
            .filter_map(|(org_id, org_name, role)| {
                OrgRole::parse(&role).map(|role| Membership {
                    org_id,
                    org_name,
                    role,
                })
            })
            .collect(),
    })
}

#[derive(sqlx::FromRow)]
struct LoginRow {
    id: Uuid,
    password_hash: Option<String>,
    disabled_at: Option<OffsetDateTime>,
    locked_until: Option<OffsetDateTime>,
    failed_login_count: i32,
}

fn invalid_credentials() -> ApiFailure {
    ApiFailure::new(
        StatusCode::UNAUTHORIZED,
        ErrorCode::InvalidCredentials,
        "invalid e-mail or password",
    )
}

/// `POST /v1/auth/login`
pub async fn login(
    State(state): State<AppState>,
    ctx: ClientCtx,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Json<AuthTokens>> {
    let email = normalize_email(&req.email);
    if email.len() < 3 || email.len() > 320 || req.password.len() > password::MAX_PASSWORD_LEN {
        return Err(invalid_credentials());
    }
    limit(&state, "login-ip", &ctx.ip, limits::LOGIN_PER_IP).await?;
    // Keyed by a hash: no e-mail addresses in Redis.
    limit(
        &state,
        "login-account",
        &sha256_hex(email.as_bytes())[..32],
        limits::LOGIN_PER_ACCOUNT,
    )
    .await?;

    let user: Option<LoginRow> = sqlx::query_as(
        "SELECT id, password_hash, disabled_at, locked_until, failed_login_count
         FROM users WHERE email_normalized = $1",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    let now = OffsetDateTime::now_utc();

    if let Some(u) = &user
        && let Some(until) = u.locked_until
        && until > now
    {
        AuditRecord::new(action::LOGIN, Outcome::Denied)
            .user(u.id)
            .client(ctx.ip(), ctx.rid())
            .details(json!({"reason": "account_locked"}))
            .write(&state.db)
            .await?;
        let mut f = ApiFailure::new(
            StatusCode::LOCKED,
            ErrorCode::AccountLocked,
            "too many failed attempts",
        );
        f.body.retry_after = Some((until - now).whole_seconds().max(1) as u64);
        return Err(f);
    }

    let ok = password::verify(
        req.password,
        user.as_ref().and_then(|u| u.password_hash.clone()),
    )
    .await;

    let Some(u) = user else {
        AuditRecord::new(action::LOGIN, Outcome::Failure)
            .client(ctx.ip(), ctx.rid())
            .details(json!({"reason": "unknown_account"}))
            .write(&state.db)
            .await?;
        return Err(invalid_credentials());
    };

    if !ok {
        let failures = u.failed_login_count + 1;
        if failures >= MAX_FAILED_LOGINS {
            sqlx::query("UPDATE users SET failed_login_count = 0, locked_until = $2 WHERE id = $1")
                .bind(u.id)
                .bind(now + LOCKOUT)
                .execute(&state.db)
                .await?;
            AuditRecord::security(action::ACCOUNT_LOCKED, Outcome::Denied)
                .target_user(u.id)
                .client(ctx.ip(), ctx.rid())
                .details(json!({"failed_attempts": failures, "minutes": LOCKOUT.whole_minutes()}))
                .write(&state.db)
                .await?;
        } else {
            sqlx::query("UPDATE users SET failed_login_count = $2 WHERE id = $1")
                .bind(u.id)
                .bind(failures)
                .execute(&state.db)
                .await?;
        }
        AuditRecord::new(action::LOGIN, Outcome::Failure)
            .user(u.id)
            .client(ctx.ip(), ctx.rid())
            .details(json!({"reason": "bad_password"}))
            .write(&state.db)
            .await?;
        return Err(invalid_credentials());
    }

    if u.disabled_at.is_some() {
        AuditRecord::new(action::LOGIN, Outcome::Denied)
            .user(u.id)
            .client(ctx.ip(), ctx.rid())
            .details(json!({"reason": "account_disabled"}))
            .write(&state.db)
            .await?;
        return Err(invalid_credentials());
    }

    let sid = Uuid::now_v7();
    let (refresh, refresh_hash) = new_refresh(sid);
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE users SET failed_login_count = 0, locked_until = NULL, last_login_at = now()
         WHERE id = $1",
    )
    .bind(u.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO auth_sessions (id, user_id, refresh_hash, expires_at, absolute_expires_at,
             client_ip, user_agent)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(sid)
    .bind(u.id)
    .bind(&refresh_hash)
    .bind(now + REFRESH_TTL)
    .bind(now + ABSOLUTE_SESSION_TTL)
    .bind(&ctx.ip)
    .bind(&ctx.user_agent)
    .execute(&mut *tx)
    .await?;
    AuditRecord::new(action::LOGIN, Outcome::Success)
        .user(u.id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"auth_session": sid}))
        .write(&mut *tx)
        .await?;
    tx.commit().await?;

    let access = state.tokens.access(&AccessClaims { sub: u.id, sid });
    Ok(Json(AuthTokens {
        access_token: access.token,
        refresh_token: refresh,
        expires_in: AccessClaims::TTL.whole_seconds(),
        user: load_profile(&state.db, u.id).await?,
    }))
}

#[derive(sqlx::FromRow)]
struct RefreshRow {
    user_id: Uuid,
    refresh_hash: String,
    previous_refresh_hash: Option<String>,
    expires_at: OffsetDateTime,
    absolute_expires_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
    disabled_at: Option<OffsetDateTime>,
}

/// `POST /v1/auth/refresh` — rotates the refresh token. Presenting an already rotated
/// token revokes the whole login (it was copied by someone).
pub async fn refresh(
    State(state): State<AppState>,
    ctx: ClientCtx,
    Json(req): Json<RefreshRequest>,
) -> ApiResult<Json<AuthTokens>> {
    limit(&state, "refresh-ip", &ctx.ip, limits::REFRESH_PER_IP).await?;
    let (sid, hash) = parse_refresh(&req.refresh_token)
        .ok_or_else(|| ApiFailure::unauthorized("invalid refresh token"))?;
    let mut tx = state.db.begin().await?;
    let row: Option<RefreshRow> = sqlx::query_as(
        "SELECT s.user_id, s.refresh_hash, s.previous_refresh_hash, s.expires_at,
                s.absolute_expires_at, s.revoked_at, u.disabled_at
         FROM auth_sessions s JOIN users u ON u.id = s.user_id
         WHERE s.id = $1 FOR UPDATE OF s",
    )
    .bind(sid)
    .fetch_optional(&mut *tx)
    .await?;
    let row = row.ok_or_else(|| ApiFailure::unauthorized("invalid refresh token"))?;
    if row.revoked_at.is_some() || row.disabled_at.is_some() {
        return Err(ApiFailure::unauthorized("session revoked"));
    }
    let now = OffsetDateTime::now_utc();
    if !ct_eq(row.refresh_hash.as_bytes(), hash.as_bytes()) {
        if row
            .previous_refresh_hash
            .as_deref()
            .is_some_and(|p| ct_eq(p.as_bytes(), hash.as_bytes()))
        {
            sqlx::query(
                "UPDATE auth_sessions SET revoked_at = now(), revoked_reason = 'refresh_reuse'
                 WHERE id = $1",
            )
            .bind(sid)
            .execute(&mut *tx)
            .await?;
            AuditRecord::security(action::REFRESH_REUSE, Outcome::Denied)
                .target_user(row.user_id)
                .client(ctx.ip(), ctx.rid())
                .details(json!({"auth_session": sid, "effect": "session_revoked"}))
                .write(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        return Err(ApiFailure::unauthorized("invalid refresh token"));
    }
    if row.expires_at <= now || row.absolute_expires_at <= now {
        return Err(ApiFailure::from_token(
            aionix_crypto::token::TokenError::Expired,
        ));
    }
    let (refresh, new_hash) = new_refresh(sid);
    let expires = (now + REFRESH_TTL).min(row.absolute_expires_at);
    sqlx::query(
        "UPDATE auth_sessions SET previous_refresh_hash = refresh_hash, refresh_hash = $2,
             expires_at = $3, last_used_at = now()
         WHERE id = $1",
    )
    .bind(sid)
    .bind(&new_hash)
    .bind(expires)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let access = state.tokens.access(&AccessClaims {
        sub: row.user_id,
        sid,
    });
    Ok(Json(AuthTokens {
        access_token: access.token,
        refresh_token: refresh,
        expires_in: AccessClaims::TTL.whole_seconds(),
        user: load_profile(&state.db, row.user_id).await?,
    }))
}

/// `POST /v1/auth/logout` — revokes the login the refresh token belongs to.
pub async fn logout(
    State(state): State<AppState>,
    ctx: ClientCtx,
    Json(req): Json<RefreshRequest>,
) -> ApiResult<StatusCode> {
    let (sid, hash) = parse_refresh(&req.refresh_token)
        .ok_or_else(|| ApiFailure::unauthorized("invalid refresh token"))?;
    let user: Option<Uuid> = sqlx::query_scalar(
        "UPDATE auth_sessions SET revoked_at = now(), revoked_reason = 'logout'
         WHERE id = $1 AND revoked_at IS NULL AND (refresh_hash = $2 OR previous_refresh_hash = $2)
         RETURNING user_id",
    )
    .bind(sid)
    .bind(&hash)
    .fetch_optional(&state.db)
    .await?;
    let user = user.ok_or_else(|| ApiFailure::unauthorized("invalid refresh token"))?;
    AuditRecord::new(action::LOGOUT, Outcome::Success)
        .user(user)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"auth_session": sid}))
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/auth/logout-all` — revokes every login of the caller.
pub async fn logout_all(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let n = sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now(), revoked_reason = 'logout_all'
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user.user_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    AuditRecord::new(action::LOGOUT_ALL, Outcome::Success)
        .user(user.user_id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"revoked_sessions": n}))
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/auth/password` — the caller's new password. Requires the current one;
/// clears the temporary-password flag and signs out every other login of the account
/// (the one making the request stays signed in).
pub async fn change_password(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Json(req): Json<ChangePasswordRequest>,
) -> ApiResult<StatusCode> {
    limit(
        &state,
        "password-change",
        &user.user_id.to_string(),
        limits::PASSWORD_CHANGE_PER_USER,
    )
    .await?;
    password::check_policy(&req.new_password)
        .map_err(|e| ApiFailure::bad_request(e.to_string()))?;
    if req.current_password.len() > password::MAX_PASSWORD_LEN {
        return Err(invalid_credentials());
    }
    let current: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(user.user_id)
            .fetch_one(&state.db)
            .await?;
    if !password::verify(req.current_password, current).await {
        AuditRecord::new(action::PASSWORD_CHANGED, Outcome::Failure)
            .user(user.user_id)
            .client(ctx.ip(), ctx.rid())
            .details(json!({"reason": "bad_current_password"}))
            .write(&state.db)
            .await?;
        return Err(invalid_credentials());
    }
    let hash = password::hash(req.new_password).await;
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE users SET password_hash = $2, must_change_password = false, updated_at = now()
         WHERE id = $1",
    )
    .bind(user.user_id)
    .bind(hash)
    .execute(&mut *tx)
    .await?;
    let others = sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now(), revoked_reason = 'password_changed'
         WHERE user_id = $1 AND id <> $2 AND revoked_at IS NULL",
    )
    .bind(user.user_id)
    .bind(user.sid)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    AuditRecord::new(action::PASSWORD_CHANGED, Outcome::Success)
        .user(user.user_id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"other_logins_signed_out": others}))
        .write(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Refuses work that needs a real password while the account is still on a temporary
/// one. Old clients do not show the change-password screen, so the server decides.
pub async fn require_own_password(db: &PgPool, user: Uuid) -> ApiResult<()> {
    let must: bool = sqlx::query_scalar("SELECT must_change_password FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(db)
        .await?;
    if must {
        return Err(ApiFailure::forbidden(
            "password change required: choose your own password first",
        ));
    }
    Ok(())
}

/// `GET /v1/me`
pub async fn me(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<UserProfile>> {
    Ok(Json(load_profile(&state.db, user.user_id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_token_format() {
        let sid = Uuid::now_v7();
        let (token, hash) = new_refresh(sid);
        assert!(token.starts_with("axr1."));
        assert_eq!(parse_refresh(&token), Some((sid, hash)));
        assert_eq!(parse_refresh("axr1.not-a-uuid.x"), None);
        assert_eq!(parse_refresh(&format!("axr1.{sid}.")), None);
        assert_eq!(parse_refresh("garbage"), None);
    }

    #[test]
    fn emails_are_normalized() {
        assert_eq!(normalize_email("  Ana@Example.COM "), "ana@example.com");
    }
}
