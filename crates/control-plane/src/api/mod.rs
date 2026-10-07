//! REST API (`/v1/...`). Bodies are the DTOs of `aionix_control_api::rest`.

mod auth;
mod devices;
mod orgs;
mod server;
mod sessions;

pub use auth::normalize_email;
pub(crate) use orgs::{new_enrollment_secret, token_expiry};

use std::net::SocketAddr;

use aionix_control_api::claims::AccessClaims;
use aionix_server_common::ratelimit::{Decision, Limit};
use axum::{
    Router,
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{HeaderMap, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    error::{ApiFailure, ApiResult},
    ratelimit::limits,
    state::AppState,
};

/// The complete API router (without health routes).
pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/server/info", get(server::info))
        .route("/auth/login", post(auth::login))
        .route("/auth/refresh", post(auth::refresh))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/logout-all", post(auth::logout_all))
        .route("/me", get(auth::me))
        .route("/devices/challenge", post(devices::challenge))
        .route("/devices/register", post(devices::register))
        .route("/devices/login", post(devices::login))
        .route("/devices/lookup/{friendly_id}", get(devices::lookup))
        .route("/devices/{id}/revoke", post(devices::revoke))
        .route("/orgs/{org}/devices", get(orgs::devices))
        .route(
            "/orgs/{org}/enrollment-tokens",
            post(orgs::create_enrollment_token),
        )
        .route(
            "/orgs/{org}/members",
            get(orgs::members).put(orgs::upsert_member),
        )
        .route("/orgs/{org}/audit", get(orgs::audit))
        .route("/sessions", post(sessions::create).get(sessions::list))
        .route("/sessions/{id}/events", post(sessions::report_event));
    Router::new()
        .nest("/v1", v1)
        .layer(middleware::from_fn_with_state(state.clone(), global_limit))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}

/// Request origin used for rate limiting and audit.
#[derive(Debug, Clone)]
pub struct ClientCtx {
    pub ip: String,
    pub request_id: Option<String>,
    pub user_agent: Option<String>,
}

impl ClientCtx {
    fn from_parts(parts: &Parts, state: &AppState) -> Self {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0)
            .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
        Self {
            ip: state.proxies.client_ip(peer, &parts.headers).to_string(),
            request_id: aionix_server_common::http::request_id(&parts.headers),
            user_agent: parts
                .headers
                .get(axum::http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(200).collect()),
        }
    }

    pub fn ip(&self) -> Option<&str> {
        Some(self.ip.as_str())
    }

    pub fn rid(&self) -> Option<&str> {
        self.request_id.as_deref()
    }
}

impl FromRequestParts<AppState> for ClientCtx {
    type Rejection = ApiFailure;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        Ok(Self::from_parts(parts, state))
    }
}

/// An authenticated user: valid access token whose login session is still active and
/// whose account is enabled. Checked against PostgreSQL on every request so logout,
/// revocation and account disabling take effect immediately.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub sid: Uuid,
    pub email: String,
    pub display_name: String,
}

pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

#[derive(sqlx::FromRow)]
struct SessionCheck {
    email: String,
    display_name: String,
    disabled_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
    absolute_expires_at: OffsetDateTime,
}

pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthUser> {
    let token = bearer(headers).ok_or_else(|| ApiFailure::unauthorized("missing bearer token"))?;
    let v = state
        .tokens
        .verify::<AccessClaims>(token)
        .map_err(ApiFailure::from_token)?;
    let row: Option<SessionCheck> = sqlx::query_as(
        "SELECT u.email, u.display_name, u.disabled_at, s.revoked_at, s.absolute_expires_at
         FROM auth_sessions s JOIN users u ON u.id = s.user_id
         WHERE s.id = $1 AND s.user_id = $2",
    )
    .bind(v.claims.sid)
    .bind(v.claims.sub)
    .fetch_optional(&state.db)
    .await?;
    let row = row.ok_or_else(|| ApiFailure::unauthorized("session not found"))?;
    if row.disabled_at.is_some() {
        return Err(ApiFailure::unauthorized("account disabled"));
    }
    if row.revoked_at.is_some() {
        return Err(ApiFailure::unauthorized("session revoked"));
    }
    if row.absolute_expires_at <= OffsetDateTime::now_utc() {
        return Err(ApiFailure::from_token(
            aionix_crypto::token::TokenError::Expired,
        ));
    }
    Ok(AuthUser {
        user_id: v.claims.sub,
        sid: v.claims.sid,
        email: row.email,
        display_name: row.display_name,
    })
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiFailure;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        authenticate(state, &parts.headers).await
    }
}

/// Applies a named limit; `Err(429)` when exceeded.
pub async fn limit(state: &AppState, bucket: &str, key: &str, l: Limit) -> ApiResult<()> {
    match state.limiter.check(bucket, key, l).await {
        Decision::Allowed => Ok(()),
        Decision::Limited { retry_after } => {
            metrics::counter!("aionix_api_rate_limited_total", "bucket" => bucket.to_owned())
                .increment(1);
            Err(ApiFailure::rate_limited(retry_after))
        }
    }
}

async fn global_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let ctx = ClientCtx::from_parts(&parts, &state);
    if let Err(f) = limit(&state, "global", &ctx.ip, limits::GLOBAL_PER_IP).await {
        return f.into_response();
    }
    metrics::counter!("aionix_api_requests_total").increment(1);
    next.run(Request::from_parts(parts, body)).await
}

/// Trims and bounds free text coming from clients (names, details).
pub(crate) fn clean_text(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}
