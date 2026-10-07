//! Device enrollment, device login (proof of key possession), lookup and revocation.
//!
//! The device's Ed25519 key is generated and kept on the device. The server only ever
//! receives the public key plus a domain-separated signature over a fresh single-use
//! nonce, proving possession. The friendly 9-digit id is allocated here and is a
//! lookup handle only.

use aionix_control_api::{
    ErrorCode,
    claims::{DeviceClaims, RelayClaims},
    rest::{
        Challenge, DeviceInfo, DeviceLoginRequest, DeviceLoginResponse, DeviceLookup,
        RegisterDeviceRequest, RegisterDeviceResponse, RelayGrant, RevokeDeviceRequest,
    },
};
use aionix_crypto::{SignatureDomain, sha256_hex, verify_domain};
use aionix_shared::{DeviceKind, FriendlyId};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde_json::json;
use uuid::Uuid;

use super::{AuthUser, ClientCtx, authenticate, clean_text, limit};
use crate::{
    audit::{AuditRecord, Outcome, action},
    authz::{self, Permission},
    db::is_unique_violation,
    devices,
    error::{ApiFailure, ApiResult},
    ratelimit::limits,
    state::AppState,
};

/// `POST /v1/devices/challenge`
pub async fn challenge(State(state): State<AppState>, ctx: ClientCtx) -> ApiResult<Json<Challenge>> {
    limit(&state, "challenge-ip", &ctx.ip, limits::CHALLENGE_PER_IP).await?;
    let nonce = devices::new_challenge(&state.db).await?;
    Ok(Json(Challenge {
        nonce,
        expires_in: devices::CHALLENGE_TTL.whole_seconds(),
    }))
}

fn challenge_expired() -> ApiFailure {
    ApiFailure::new(
        StatusCode::BAD_REQUEST,
        ErrorCode::ChallengeExpired,
        "unknown, used or expired challenge",
    )
}

fn invalid_signature() -> ApiFailure {
    ApiFailure::new(
        StatusCode::UNAUTHORIZED,
        ErrorCode::InvalidSignature,
        "signature does not prove possession of the key",
    )
}

fn revoked() -> ApiFailure {
    ApiFailure::new(
        StatusCode::FORBIDDEN,
        ErrorCode::DeviceRevoked,
        "device identity revoked",
    )
}

fn clean_info(info: &DeviceInfo) -> DeviceInfo {
    DeviceInfo {
        hostname: clean_text(&info.hostname, 128),
        os_name: clean_text(&info.os_name, 64),
        os_version: clean_text(&info.os_version, 64),
        app_version: clean_text(&info.app_version, 32),
        username: clean_text(&info.username, 128),
    }
}

/// `POST /v1/devices/register`
pub async fn register(
    State(state): State<AppState>,
    ctx: ClientCtx,
    headers: HeaderMap,
    Json(req): Json<RegisterDeviceRequest>,
) -> ApiResult<Json<RegisterDeviceResponse>> {
    limit(&state, "register-ip", &ctx.ip, limits::REGISTER_PER_IP).await?;
    let refuse = |reason: &'static str| {
        AuditRecord::security(action::DEVICE_ENROLL_REFUSED, Outcome::Denied)
            .client(ctx.ip(), ctx.rid())
            .details(json!({"reason": reason, "kind": req.kind.as_str()}))
    };

    if !devices::consume_challenge(&state.db, &req.nonce).await? {
        refuse("challenge").write(&state.db).await?;
        return Err(challenge_expired());
    }
    if verify_domain(
        &req.public_key,
        SignatureDomain::DeviceRegistration,
        req.nonce.as_bytes(),
        &req.signature,
    )
    .is_err()
    {
        refuse("bad_signature").write(&state.db).await?;
        return Err(invalid_signature());
    }
    if let Some(existing) = devices::by_key(&state.db, &req.public_key).await? {
        if existing.is_revoked() {
            refuse("revoked_identity")
                .target_device(existing.id)
                .write(&state.db)
                .await?;
            return Err(revoked());
        }
        return Err(ApiFailure::conflict(
            "identity already registered; use /v1/devices/login",
        ));
    }

    let info = clean_info(&req.info);
    let mut tx = state.db.begin().await?;
    let (org_id, customer_id, owner, token_id) = match req.kind {
        DeviceKind::Technician => {
            let user = match authenticate(&state, &headers).await {
                Ok(u) => u,
                Err(e) => {
                    refuse("technician_without_login").write(&state.db).await?;
                    return Err(e);
                }
            };
            (None, None, Some(user.user_id), None)
        }
        DeviceKind::Agent => {
            let Some(token) = req.enrollment_token.as_deref().filter(|t| t.len() < 200) else {
                refuse("missing_enrollment_token").write(&state.db).await?;
                return Err(ApiFailure::new(
                    StatusCode::FORBIDDEN,
                    ErrorCode::EnrollmentInvalid,
                    "enrollment token required",
                ));
            };
            let row: Option<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
                "UPDATE enrollment_tokens SET uses = uses + 1
                 WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now() AND uses < max_uses
                 RETURNING id, org_id, customer_id",
            )
            .bind(sha256_hex(token.trim().as_bytes()))
            .fetch_optional(&mut *tx)
            .await?;
            let Some((tid, org, customer)) = row else {
                drop(tx);
                refuse("invalid_enrollment_token").write(&state.db).await?;
                return Err(ApiFailure::new(
                    StatusCode::FORBIDDEN,
                    ErrorCode::EnrollmentInvalid,
                    "invalid, expired or exhausted enrollment token",
                ));
            };
            (Some(org), customer, None, Some(tid))
        }
        DeviceKind::QuickSupport => {
            if !state.settings.allow_quick_support {
                drop(tx);
                refuse("quick_support_disabled").write(&state.db).await?;
                return Err(ApiFailure::forbidden("quick support registration disabled"));
            }
            limit(
                &state,
                "register-qs-ip",
                &ctx.ip,
                limits::QUICK_SUPPORT_REGISTER_PER_IP,
            )
            .await?;
            (None, None, None, None)
        }
    };

    let id = Uuid::now_v7();
    let mut friendly = None;
    for _ in 0..16 {
        let candidate = FriendlyId::random();
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO devices (id, friendly_id, public_key, kind, org_id, customer_id, owner_user_id,
                 enrollment_token_id, hostname, os_name, os_version, app_version, last_login_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NULL)
             ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(id)
        .bind(candidate.value() as i32)
        .bind(req.public_key.to_base64())
        .bind(req.kind.as_str())
        .bind(org_id)
        .bind(customer_id)
        .bind(owner)
        .bind(token_id)
        .bind(&info.hostname)
        .bind(&info.os_name)
        .bind(&info.os_version)
        .bind(&info.app_version)
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_some() {
            friendly = Some(candidate);
            break;
        }
        // Conflict: the key raced with another registration, or the friendly id is taken.
        let key_taken: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM devices WHERE public_key = $1)")
                .bind(req.public_key.to_base64())
                .fetch_one(&mut *tx)
                .await?;
        if key_taken {
            return Err(ApiFailure::conflict("identity already registered"));
        }
    }
    let friendly = friendly.ok_or_else(ApiFailure::internal)?;
    let mut rec = AuditRecord::new(action::DEVICE_ENROLLED, Outcome::Success)
        .org(org_id)
        .target_device(id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({
            "kind": req.kind.as_str(),
            "friendly_id": friendly.compact(),
            "hostname": info.hostname,
            "enrollment_token": token_id,
        }));
    if let Some(u) = owner {
        rec = rec.user(u);
    }
    rec.write(&mut *tx).await?;
    if let Err(e) = tx.commit().await {
        if is_unique_violation(&e, None) {
            return Err(ApiFailure::conflict("identity already registered"));
        }
        return Err(e.into());
    }
    metrics::counter!("aionix_devices_enrolled_total", "kind" => req.kind.as_str()).increment(1);
    Ok(Json(RegisterDeviceResponse {
        device_id: id,
        friendly_id: friendly,
    }))
}

/// `POST /v1/devices/login` — exchanges a fresh signature for a device token (1 h) and
/// relay grants (12 h).
pub async fn login(
    State(state): State<AppState>,
    ctx: ClientCtx,
    Json(req): Json<DeviceLoginRequest>,
) -> ApiResult<Json<DeviceLoginResponse>> {
    limit(&state, "device-login-ip", &ctx.ip, limits::DEVICE_LOGIN_PER_IP).await?;
    let refuse = |reason: &'static str| {
        AuditRecord::security(action::DEVICE_LOGIN_REFUSED, Outcome::Denied)
            .client(ctx.ip(), ctx.rid())
            .details(json!({"reason": reason}))
    };
    if !devices::consume_challenge(&state.db, &req.nonce).await? {
        return Err(challenge_expired());
    }
    if verify_domain(
        &req.public_key,
        SignatureDomain::DeviceLogin,
        req.nonce.as_bytes(),
        &req.signature,
    )
    .is_err()
    {
        refuse("bad_signature").write(&state.db).await?;
        return Err(invalid_signature());
    }
    let Some(dev) = devices::by_key(&state.db, &req.public_key).await? else {
        refuse("unknown_identity").write(&state.db).await?;
        return Err(ApiFailure::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "identity not registered",
        ));
    };
    if dev.is_revoked() {
        refuse("revoked_identity")
            .device(dev.id)
            .org(dev.org_id)
            .write(&state.db)
            .await?;
        return Err(revoked());
    }
    let info = req.info.as_ref().map(clean_info);
    sqlx::query(
        "UPDATE devices SET last_login_at = now(), updated_at = now(),
             hostname = COALESCE($2, hostname), os_name = COALESCE($3, os_name),
             os_version = COALESCE($4, os_version), app_version = COALESCE($5, app_version)
         WHERE id = $1",
    )
    .bind(dev.id)
    .bind(info.as_ref().map(|i| i.hostname.clone()))
    .bind(info.as_ref().map(|i| i.os_name.clone()))
    .bind(info.as_ref().map(|i| i.os_version.clone()))
    .bind(info.as_ref().map(|i| i.app_version.clone()))
    .execute(&state.db)
    .await?;

    let device_token = state.tokens.device(&DeviceClaims {
        sub: dev.id,
        key: req.public_key,
        kind: dev.kind(),
    });
    let relays = state
        .settings
        .relay_urls
        .iter()
        .map(|url| {
            let t = state.tokens.relay(&RelayClaims {
                sub: dev.id,
                key: req.public_key,
            });
            RelayGrant {
                url: url.clone(),
                token: t.token,
                expires_in: RelayClaims::TTL.whole_seconds(),
            }
        })
        .collect();
    let org_name: Option<String> = match dev.org_id {
        Some(org) => {
            sqlx::query_scalar("SELECT name FROM organizations WHERE id = $1")
                .bind(org)
                .fetch_optional(&state.db)
                .await?
        }
        None => None,
    };
    Ok(Json(DeviceLoginResponse {
        device_id: dev.id,
        friendly_id: dev.friendly(),
        device_token: device_token.token,
        expires_in: DeviceClaims::TTL.whole_seconds(),
        relays,
        org_name,
    }))
}

/// `GET /v1/devices/lookup/{friendly_id}` — existence and presence for any signed-in
/// user; details only for members allowed to view the device's organization.
pub async fn lookup(
    State(state): State<AppState>,
    user: AuthUser,
    Path(friendly): Path<String>,
) -> ApiResult<Json<DeviceLookup>> {
    limit(&state, "lookup-user", &user.user_id.to_string(), limits::LOOKUP_PER_USER).await?;
    let f: FriendlyId = friendly
        .parse()
        .map_err(|_| ApiFailure::bad_request("invalid friendly id"))?;
    let dev = devices::by_friendly(&state.db, f)
        .await?
        .filter(|d| !d.is_revoked())
        .ok_or_else(|| ApiFailure::not_found("device not found"))?;
    let presence = state.presence.get(dev.id).await?;
    let state_now = crate::presence::state_of(presence.as_ref());
    let can_view = match dev.org_id {
        Some(org) => authz::has_permission(&state.db, user.user_id, org, Permission::DeviceView).await?,
        None => dev.owner_user_id == Some(user.user_id),
    };
    Ok(Json(DeviceLookup {
        friendly_id: f,
        kind: dev.kind(),
        online: state_now == aionix_control_api::rest::PresenceState::Online,
        presence: state_now,
        device: can_view.then(|| dev.summary(presence.as_ref())),
    }))
}

/// `POST /v1/devices/{id}/revoke`
pub async fn revoke(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<RevokeDeviceRequest>>,
) -> ApiResult<StatusCode> {
    let dev = devices::by_id(&state.db, id)
        .await?
        .ok_or_else(|| ApiFailure::not_found("device not found"))?;
    let allowed = match dev.org_id {
        Some(org) => authz::has_permission(&state.db, user.user_id, org, Permission::DeviceRevoke).await?,
        None => dev.owner_user_id == Some(user.user_id),
    };
    if !allowed {
        AuditRecord::new(action::PERMISSION_DENIED, Outcome::Denied)
            .user(user.user_id)
            .org(dev.org_id)
            .target_device(dev.id)
            .client(ctx.ip(), ctx.rid())
            .details(json!({"permission": "device.revoke"}))
            .write(&state.db)
            .await?;
        return Err(ApiFailure::forbidden("missing permission device.revoke"));
    }
    let reason = body
        .map(|b| clean_text(&b.reason, 200))
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "revoked by administrator".into());
    devices::revoke(
        &state.db,
        state.bus.as_ref(),
        state.redis.as_ref(),
        dev.id,
        Some(user.user_id),
        &reason,
        (ctx.ip(), ctx.rid()),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
