//! Technician side: user login and the production `AuthorizationProvider`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use aionix_control_api::{
    ErrorCode,
    claims::SessionTicketClaims,
    rest::{AuthTokens, CreateSessionRequest, UserProfile},
};
use aionix_remote_core::session::authority::{
    AuthorityError, AuthorizationProvider, DeviceLookup, SessionGrant, SessionRequest,
};
use aionix_shared::AccessMode;
use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::Mutex;

use crate::{ApiClient, ClientError, enroll::DeviceSession};

struct Tokens {
    tokens: AuthTokens,
    access_renew_at: Instant,
}

/// Called with the new refresh token every time it rotates, so a "stay signed in"
/// store always holds the current one (presenting a rotated token revokes the login).
type PersistFn = Box<dyn Fn(&str) + Send + Sync>;

/// A signed-in user with automatic access-token renewal (rotating refresh tokens).
pub struct UserSession {
    api: ApiClient,
    inner: Mutex<Tokens>,
    persist: Option<PersistFn>,
}

impl UserSession {
    pub async fn login(api: ApiClient, email: &str, password: &str) -> Result<Self, ClientError> {
        let tokens = api.login(email, password).await?;
        Ok(Self {
            api,
            inner: Mutex::new(Self::wrap(tokens)),
            persist: None,
        })
    }

    /// Restores a login from a stored refresh token (rotating it immediately).
    pub async fn resume(api: ApiClient, refresh_token: &str) -> Result<Self, ClientError> {
        let tokens = api.refresh(refresh_token).await?;
        Ok(Self {
            api,
            inner: Mutex::new(Self::wrap(tokens)),
            persist: None,
        })
    }

    /// Keeps a store up to date: `f` receives the current refresh token now and after
    /// every rotation.
    pub async fn persist_with(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        f(&self.inner.lock().await.tokens.refresh_token);
        self.persist = Some(Box::new(f));
        self
    }

    fn wrap(tokens: AuthTokens) -> Tokens {
        let lifetime = Duration::from_secs(tokens.expires_in.max(0) as u64);
        Tokens {
            access_renew_at: Instant::now() + lifetime.saturating_sub(Duration::from_secs(60)),
            tokens,
        }
    }

    /// The API client this login talks to.
    pub fn api(&self) -> &ApiClient {
        &self.api
    }

    pub async fn profile(&self) -> UserProfile {
        self.inner.lock().await.tokens.user.clone()
    }

    /// A valid access token, refreshing it when close to expiry.
    pub async fn access_token(&self) -> Result<String, ClientError> {
        let mut inner = self.inner.lock().await;
        if Instant::now() >= inner.access_renew_at {
            let fresh = self.api.refresh(&inner.tokens.refresh_token).await?;
            *inner = Self::wrap(fresh);
            if let Some(persist) = &self.persist {
                persist(&inner.tokens.refresh_token);
            }
        }
        Ok(inner.tokens.access_token.clone())
    }

    pub async fn logout(&self) -> Result<(), ClientError> {
        let refresh = self.inner.lock().await.tokens.refresh_token.clone();
        self.api.logout(&refresh).await
    }
}

/// Production session authority: asks the Control Plane, which authenticates the
/// user and workstation, checks organization permissions and presence, and signs the
/// ticket. Replaces `DevelopmentAuthority` outside tests.
pub struct ControlPlaneAuthority {
    user: Arc<UserSession>,
    workstation: Arc<DeviceSession>,
}

impl ControlPlaneAuthority {
    pub fn new(user: Arc<UserSession>, workstation: Arc<DeviceSession>) -> Self {
        Self { user, workstation }
    }
}

fn map_error(e: ClientError) -> AuthorityError {
    match e {
        ClientError::Api { code, detail, .. } => match code {
            ErrorCode::NotFound => AuthorityError::DeviceNotFound,
            ErrorCode::DeviceOffline => AuthorityError::DeviceOffline,
            ErrorCode::Unavailable | ErrorCode::Internal | ErrorCode::RateLimited => {
                AuthorityError::Unavailable(detail)
            }
            ErrorCode::DeviceBusy => AuthorityError::Forbidden(format!("device busy: {detail}")),
            ErrorCode::DeviceTimeout => {
                AuthorityError::Unavailable(format!("device did not answer: {detail}"))
            }
            other => AuthorityError::Forbidden(format!("{other:?}: {detail}")),
        },
        other => AuthorityError::Unavailable(other.to_string()),
    }
}

#[async_trait]
impl AuthorizationProvider for ControlPlaneAuthority {
    async fn request_session(
        &self,
        request: SessionRequest,
    ) -> Result<SessionGrant, AuthorityError> {
        // The identity in the request is informational: the server derives the real
        // one from the signed-in user and the workstation token. It must still name
        // this workstation's key, since the host checks the transport peer against it.
        if request.technician.key != self.workstation.key().public() {
            return Err(AuthorityError::Forbidden(
                "technician key does not belong to this workstation".into(),
            ));
        }
        // Unattended access needs the device password, and only that mode takes one.
        match (request.mode, request.unattended_password.is_some()) {
            (AccessMode::Unattended, false) => {
                return Err(AuthorityError::Forbidden(
                    "unattended access needs the device password".into(),
                ));
            }
            (AccessMode::Assisted, true) => {
                return Err(AuthorityError::Forbidden(
                    "a password is only used for unattended access".into(),
                ));
            }
            _ => {}
        }
        let target = match request.target {
            DeviceLookup::Friendly(f) => f,
            DeviceLookup::Id(_) => {
                return Err(AuthorityError::Forbidden(
                    "sessions are requested by friendly id".into(),
                ));
            }
        };
        let access = self.user.access_token().await.map_err(map_error)?;
        let device_token = self.workstation.token().await.map_err(map_error)?;
        let resp = self
            .workstation
            .api()
            .create_session(
                &access,
                &device_token,
                &CreateSessionRequest {
                    target,
                    permissions: Some(request.permissions.clone()),
                    unattended_password: request
                        .unattended_password
                        .as_ref()
                        .map(|p| p.expose().to_owned()),
                },
            )
            .await
            .map_err(map_error)?;
        // Fail closed: the server must have granted the mode that was asked for.
        if resp.mode != request.mode {
            return Err(AuthorityError::Forbidden(format!(
                "the server granted {} access, not {}",
                resp.mode.as_str(),
                request.mode.as_str()
            )));
        }
        Ok(SessionGrant {
            session_id: resp.session_id,
            ticket: resp.ticket,
            expires_at: resp
                .expires_at
                .unwrap_or_else(|| OffsetDateTime::now_utc() + SessionTicketClaims::TTL),
            host_device_id: resp.host.device_id,
            host_key: resp.host.key,
            host_addr: resp.host.addr,
            permissions: resp.permissions,
        })
    }
}
