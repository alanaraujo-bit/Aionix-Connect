//! Uniform HTTP error responses (`aionix_control_api::ApiError`).

use aionix_control_api::{ApiError, ErrorCode};
use aionix_crypto::token::TokenError;
use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
};
use tracing::error;

#[derive(Debug, Clone)]
pub struct ApiFailure {
    pub status: StatusCode,
    pub body: ApiError,
}

impl ApiFailure {
    pub fn new(status: StatusCode, code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiError {
                code,
                detail: detail.into(),
                retry_after: None,
            },
        }
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, detail)
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized, detail)
    }

    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, detail)
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, detail)
    }

    pub fn rate_limited(retry_after: u64) -> Self {
        let mut f = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            "too many requests",
        );
        f.body.retry_after = Some(retry_after);
        f
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            detail,
        )
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "internal error",
        )
    }

    /// Maps a token verification failure: expired tokens get their own code so clients
    /// know to refresh instead of asking the user to log in again.
    pub fn from_token(e: TokenError) -> Self {
        match e {
            TokenError::Expired => Self::new(
                StatusCode::UNAUTHORIZED,
                ErrorCode::TokenExpired,
                "token expired",
            ),
            _ => Self::unauthorized("invalid token"),
        }
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let retry = self.body.retry_after;
        let mut resp = (self.status, Json(self.body)).into_response();
        if let Some(secs) = retry
            && let Ok(v) = HeaderValue::from_str(&secs.to_string())
        {
            resp.headers_mut().insert(RETRY_AFTER, v);
        }
        resp
    }
}

impl From<sqlx::Error> for ApiFailure {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::PoolTimedOut | sqlx::Error::Io(_) | sqlx::Error::PoolClosed => {
                error!(error = %e, "database unavailable");
                Self::unavailable("database unavailable")
            }
            other => {
                error!(error = %other, "database error");
                Self::internal()
            }
        }
    }
}

impl From<redis::RedisError> for ApiFailure {
    fn from(e: redis::RedisError) -> Self {
        error!(error = %e, "redis error");
        Self::unavailable("presence store unavailable")
    }
}

impl From<anyhow::Error> for ApiFailure {
    fn from(e: anyhow::Error) -> Self {
        error!(error = %e, "internal error");
        Self::internal()
    }
}

pub type ApiResult<T> = Result<T, ApiFailure>;
