//! Session requests: real authorization and short-lived ticket issuance (ADR-0006,
//! ADR-0012).
//!
//! ```text
//! technician (bearer access token + workstation device token)
//!   ─▶ authenticate user (active login session, enabled account)
//!   ─▶ authenticate workstation (device token, not revoked, owned by the user)
//!   ─▶ resolve target friendly id ─▶ device id + key (not revoked, can host)
//!   ─▶ organization permission session.request (+ control / clipboard)
//!   ─▶ presence must be Online and not busy
//!   ─▶ sign SessionTicketClaims (90 s) ─▶ store session row + audit
//!   ─▶ SessionOffer to the host over signaling ─▶ host answers with its address
//!   ─▶ ticket + host endpoint returned to the technician
//! ```

use aionix_control_api::{
    DEVICE_TOKEN_HEADER, ErrorCode,
    claims::{DeviceClaims, SessionPermission, SessionTicketClaims, TicketHost, TicketViewer},
    rest::{
        CreateSessionRequest, CreateSessionResponse, HostEndpoint, PresenceState,
        SessionEventReport, SessionList, SessionRecord,
    },
    signal::{OfferViewer, ServerMessage},
};
use aionix_crypto::sha256_hex;
use aionix_shared::{AccessMode, DeviceKind, FriendlyId, TransportKind};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{AuthUser, ClientCtx, limit};
use crate::{
    audit::{AuditRecord, Outcome, action},
    authz::{self, Permission},
    bus::DeviceBusMessage,
    devices::{self, DeviceRow},
    error::{ApiFailure, ApiResult},
    ratelimit::limits,
    sessions::{self, EventOutcome, Reporter},
    state::AppState,
};

struct Denial<'a> {
    state: &'a AppState,
    ctx: &'a ClientCtx,
    user: &'a AuthUser,
}

impl Denial<'_> {
    async fn deny(
        &self,
        target: Option<&DeviceRow>,
        org: Option<Uuid>,
        reason: &'static str,
        failure: ApiFailure,
    ) -> ApiFailure {
        let mut rec = AuditRecord::new(action::SESSION_REQUESTED, Outcome::Denied)
            .user(self.user.user_id)
            .org(org.or(target.and_then(|t| t.org_id)))
            .client(self.ctx.ip(), self.ctx.rid())
            .details(json!({"reason": reason}));
        if let Some(t) = target {
            rec = rec.target_device(t.id);
        }
        if matches!(reason, "missing_permission" | "workstation_not_owned") {
            rec.action = action::PERMISSION_DENIED;
        }
        match rec.write(&self.state.db).await {
            Ok(()) => failure,
            Err(e) => e.into(),
        }
    }
}

/// `POST /v1/sessions`
pub async fn create(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<CreateSessionRequest>,
) -> ApiResult<Json<CreateSessionResponse>> {
    limit(
        &state,
        "session-user",
        &user.user_id.to_string(),
        limits::SESSION_REQUEST_PER_USER,
    )
    .await?;
    let d = Denial {
        state: &state,
        ctx: &ctx,
        user: &user,
    };
    // A temporary password must be replaced before the account can reach anyone.
    super::auth::require_own_password(&state.db, user.user_id).await?;

    // 1. Technician workstation: proves which endpoint key will connect.
    let device_token = headers
        .get(DEVICE_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiFailure::unauthorized("missing workstation device token"))?;
    let ws_claims = state
        .tokens
        .verify::<DeviceClaims>(device_token)
        .map_err(ApiFailure::from_token)?
        .claims;
    let workstation = devices::by_id(&state.db, ws_claims.sub).await?;
    let workstation = match workstation {
        Some(w)
            if !w.is_revoked()
                && w.kind() == DeviceKind::Technician
                && w.owner_user_id == Some(user.user_id)
                && w.key() == Some(ws_claims.key) =>
        {
            w
        }
        _ => {
            return Err(d
                .deny(
                    None,
                    None,
                    "workstation_not_owned",
                    ApiFailure::forbidden("workstation not registered to this user or revoked"),
                )
                .await);
        }
    };

    // 2. Target device.
    let target = devices::by_friendly(&state.db, req.target).await?;
    let Some(target) = target else {
        return Err(d
            .deny(
                None,
                None,
                "unknown_device",
                ApiFailure::not_found("device not found"),
            )
            .await);
    };
    if target.is_revoked() {
        return Err(d
            .deny(
                Some(&target),
                None,
                "device_revoked",
                ApiFailure::new(
                    StatusCode::FORBIDDEN,
                    ErrorCode::DeviceRevoked,
                    "device revoked",
                ),
            )
            .await);
    }
    if !target.kind().can_host() {
        return Err(d
            .deny(
                Some(&target),
                None,
                "not_a_host",
                ApiFailure::bad_request("this device cannot host sessions"),
            )
            .await);
    }
    let host_key = target.key().ok_or_else(ApiFailure::internal)?;

    // 3. Authority: the organization owning the device, or — for unmanaged quick
    //    support devices — an organization where the technician may request sessions
    //    (the host user's visible consent is then the only grant).
    let (org_id, org_name) = match target.org_id {
        Some(org) => {
            if !authz::has_permission(&state.db, user.user_id, org, Permission::SessionRequest)
                .await?
            {
                return Err(d
                    .deny(
                        Some(&target),
                        Some(org),
                        "missing_permission",
                        ApiFailure::forbidden("missing permission session.request for this device"),
                    )
                    .await);
            }
            let name: String = sqlx::query_scalar("SELECT name FROM organizations WHERE id = $1")
                .bind(org)
                .fetch_one(&state.db)
                .await?;
            (org, name)
        }
        None => {
            let orgs =
                authz::orgs_with_permission(&state.db, user.user_id, Permission::SessionRequest)
                    .await?;
            match orgs.into_iter().next() {
                Some(o) => o,
                None => {
                    return Err(d
                        .deny(
                            Some(&target),
                            None,
                            "missing_permission",
                            ApiFailure::forbidden("missing permission session.request"),
                        )
                        .await);
                }
            }
        }
    };

    // 4. Permissions = requested ∩ allowed by role. View is mandatory.
    let role_perms = authz::permissions_in(&state.db, user.user_id, org_id).await?;
    let allowed = |p: SessionPermission| match p {
        SessionPermission::View => true,
        SessionPermission::Control => role_perms.iter().any(|r| r == "session.control"),
        SessionPermission::Clipboard => role_perms.iter().any(|r| r == "session.clipboard"),
        // Not implemented by the session engine yet; never granted.
        SessionPermission::FileTransfer | SessionPermission::Restart => false,
    };
    let requested = req.permissions.clone().unwrap_or_else(|| {
        vec![
            SessionPermission::View,
            SessionPermission::Control,
            SessionPermission::Clipboard,
        ]
    });
    if !requested.contains(&SessionPermission::View) {
        return Err(ApiFailure::bad_request("the view permission is required"));
    }
    let mut perms: Vec<SessionPermission> = requested.into_iter().filter(|p| allowed(*p)).collect();
    perms.dedup();

    // 5. Presence: truthfully online and not busy.
    let presence = state.presence.get(target.id).await?;
    let pstate = crate::presence::state_of(presence.as_ref());
    if pstate != PresenceState::Online {
        return Err(d
            .deny(
                Some(&target),
                Some(org_id),
                "device_offline",
                ApiFailure::new(
                    StatusCode::CONFLICT,
                    ErrorCode::DeviceOffline,
                    format!("device is {}", pstate.as_str()),
                ),
            )
            .await);
    }
    let presence = presence.expect("online implies a record");
    if presence.busy {
        return Err(d
            .deny(
                Some(&target),
                Some(org_id),
                "device_busy",
                ApiFailure::new(StatusCode::CONFLICT, ErrorCode::DeviceBusy, "device busy"),
            )
            .await);
    }

    // 6. Ticket.
    let sid = Uuid::new_v4();
    let claims = SessionTicketClaims {
        sid,
        host: TicketHost {
            device_id: target.id,
            key: host_key,
        },
        viewer: TicketViewer {
            device_id: workstation.id,
            key: ws_claims.key,
            user_id: user.user_id,
            display_name: user.display_name.clone(),
            email: user.email.clone(),
            org_name: Some(org_name.clone()),
        },
        mode: AccessMode::Assisted,
        perms: perms.clone(),
    };
    let issued = state.tokens.session_ticket(&claims);
    let perm_strs: Vec<&str> = perms.iter().map(perm_str).collect();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO sessions (id, host_device_id, technician_user_id, technician_device_id, org_id,
             mode, permissions, status, ticket_jti_hash, ticket_expires_at)
         VALUES ($1, $2, $3, $4, $5, 'assisted', $6, 'offered', $7, $8)",
    )
    .bind(sid)
    .bind(target.id)
    .bind(user.user_id)
    .bind(workstation.id)
    .bind(org_id)
    .bind(&perm_strs)
    .bind(sha256_hex(issued.jti.as_bytes()))
    .bind(issued.expires_at)
    .execute(&mut *tx)
    .await?;
    AuditRecord::new(action::SESSION_REQUESTED, Outcome::Success)
        .user(user.user_id)
        .device(workstation.id)
        .org(Some(org_id))
        .target_device(target.id)
        .session(sid)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"friendly_id": req.target.compact()}))
        .write(&mut *tx)
        .await?;
    AuditRecord::new(action::TICKET_ISSUED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org_id))
        .target_device(target.id)
        .session(sid)
        .client(ctx.ip(), ctx.rid())
        .details(json!({
            "permissions": perm_strs,
            "mode": "assisted",
            "expires_at": issued.expires_at.unix_timestamp(),
        }))
        .write(&mut *tx)
        .await?;
    tx.commit().await?;
    metrics::counter!("aionix_session_tickets_issued_total").increment(1);

    // 7. Offer to the host and wait for its answer.
    let waiter = state.bus.await_offer_answer(sid);
    let offer = DeviceBusMessage::Deliver {
        message: ServerMessage::SessionOffer {
            session_id: sid,
            ticket: issued.token.clone(),
            mode: AccessMode::Assisted,
            viewer: Box::new(OfferViewer {
                display_name: user.display_name.clone(),
                email: user.email.clone(),
                org_name: Some(org_name),
                key: ws_claims.key,
            }),
        },
    };
    state.bus.publish_device(target.id, &offer).await?;
    let answer = waiter.wait(state.settings.offer_timeout).await;
    let finish = |status: &'static str, reason: &'static str| {
        let db = state.db.clone();
        async move {
            sqlx::query(
                "UPDATE sessions SET status = $2, answered_at = now(),
                     ended_at = CASE WHEN $2 IN ('rejected', 'failed') THEN now() ELSE NULL END,
                     end_reason = NULLIF($3, '')
                 WHERE id = $1 AND status = 'offered'",
            )
            .bind(sid)
            .bind(status)
            .bind(reason)
            .execute(&db)
            .await
        }
    };
    let audit_answer = |accepted: bool, reason: &str| {
        AuditRecord::new(
            action::SESSION_OFFER_ANSWERED,
            if accepted {
                Outcome::Success
            } else {
                Outcome::Denied
            },
        )
        .org(Some(org_id))
        .device(target.id)
        .session(sid)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"accepted": accepted, "reason": reason}))
    };
    match answer {
        Some(a) if a.accept => {
            finish("accepted", "").await?;
            audit_answer(true, "").write(&state.db).await?;
            let addr = if a.addr.relay_urls.is_empty() && a.addr.direct_addrs.is_empty() {
                presence.addr.clone()
            } else {
                a.addr
            };
            Ok(Json(CreateSessionResponse {
                session_id: sid,
                ticket: issued.token,
                mode: AccessMode::Assisted,
                host: HostEndpoint {
                    device_id: target.id,
                    key: host_key,
                    addr,
                    display_name: target.name(),
                    kind: target.kind(),
                },
                permissions: perms,
                expires_at: Some(issued.expires_at),
            }))
        }
        Some(a) => {
            let reason = match a.reason {
                Some(aionix_control_api::signal::OfferRejectReason::Policy) => "policy",
                Some(aionix_control_api::signal::OfferRejectReason::ShuttingDown) => {
                    "shutting_down"
                }
                _ => "busy",
            };
            finish("rejected", reason).await?;
            audit_answer(false, reason).write(&state.db).await?;
            Err(ApiFailure::new(
                StatusCode::CONFLICT,
                ErrorCode::DeviceBusy,
                format!("host refused the offer ({reason})"),
            ))
        }
        None => {
            finish("failed", "offer_timeout").await?;
            audit_answer(false, "timeout").write(&state.db).await?;
            Err(ApiFailure::new(
                StatusCode::GATEWAY_TIMEOUT,
                ErrorCode::DeviceTimeout,
                "the device did not answer",
            ))
        }
    }
}

fn perm_str(p: &SessionPermission) -> &'static str {
    match p {
        SessionPermission::View => "view",
        SessionPermission::Control => "control",
        SessionPermission::Clipboard => "clipboard",
        SessionPermission::FileTransfer => "file_transfer",
        SessionPermission::Restart => "restart",
    }
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

/// `GET /v1/sessions` — the caller's own sessions, newest first.
pub async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<SessionList>> {
    let rows: Vec<SessionRow> = sqlx::query_as(
        "SELECT s.id, s.host_device_id,
                COALESCE(NULLIF(d.display_name, ''), d.hostname) AS host_name,
                d.friendly_id AS host_friendly_id, u.display_name AS technician_name,
                s.mode, s.status, s.transport, s.requested_at, s.connected_at, s.ended_at
         FROM sessions s
         JOIN devices d ON d.id = s.host_device_id
         JOIN users u ON u.id = s.technician_user_id
         WHERE s.technician_user_id = $1
         ORDER BY s.requested_at DESC LIMIT 100",
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(SessionList {
        sessions: rows
            .into_iter()
            .map(|r| SessionRecord {
                id: r.id,
                host_device_id: r.host_device_id,
                host_name: r.host_name,
                host_friendly_id: FriendlyId::new(r.host_friendly_id as u32)
                    .expect("CHECK constraint keeps 9 digits"),
                technician_name: r.technician_name,
                mode: AccessMode::parse(&r.mode).unwrap_or(AccessMode::Assisted),
                status: r.status,
                transport: r.transport.as_deref().and_then(parse_transport),
                requested_at: r.requested_at,
                connected_at: r.connected_at,
                ended_at: r.ended_at,
            })
            .collect(),
    }))
}

pub(crate) fn parse_transport(s: &str) -> Option<TransportKind> {
    match s {
        "lan" => Some(TransportKind::Lan),
        "direct" => Some(TransportKind::Direct),
        "relay" => Some(TransportKind::Relay),
        "unknown" => Some(TransportKind::Unknown),
        _ => None,
    }
}

/// `POST /v1/sessions/{id}/events` — technician-side lifecycle report.
pub async fn report_event(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(report): Json<SessionEventReport>,
) -> ApiResult<StatusCode> {
    match sessions::record_event(
        &state.db,
        id,
        Reporter::Technician { user: user.user_id },
        report.kind,
        report.transport,
        &report.detail,
        (ctx.ip(), ctx.rid()),
    )
    .await?
    {
        EventOutcome::Recorded => Ok(StatusCode::NO_CONTENT),
        EventOutcome::NotAllowed => Err(ApiFailure::not_found("session not found")),
    }
}
