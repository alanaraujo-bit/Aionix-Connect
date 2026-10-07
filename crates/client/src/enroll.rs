//! Enrollment and device login.
//!
//! ```text
//! load identity ── none ──▶ generate Ed25519 key, save it (before any network I/O)
//!      │
//!      └─ not registered ──▶ GET /v1/server/info (pin token keys)
//!                            POST /v1/devices/challenge → sign (DeviceRegistration)
//!                            POST /v1/devices/register (public key only) → save
//! DeviceSession::token() ──▶ challenge → sign (DeviceLogin) → device token + relay
//!                            grants, cached and renewed 5 minutes before expiry
//! ```

use std::time::{Duration, Instant};

use aionix_control_api::rest::{
    DeviceInfo, DeviceLoginRequest, DeviceLoginResponse, RegisterDeviceRequest, RelayGrant,
};
use aionix_crypto::{IdentityKey, SignatureDomain, sign_domain, token::TokenVerifier};
use aionix_shared::DeviceKind;
use tokio::sync::Mutex;

use crate::{
    ApiClient, ClientError,
    identity::{IdentityStore, Registration, StoredIdentity},
};

/// The credential that authorizes a new registration.
pub enum Enrollment<'a> {
    /// Permanent agent, bound to the organization of the token.
    Agent { enrollment_token: &'a str },
    /// Temporary support executable (no organization).
    QuickSupport,
    /// Technician workstation, bound to the signed-in user.
    Technician { access_token: &'a str },
}

impl Enrollment<'_> {
    fn kind(&self) -> DeviceKind {
        match self {
            Self::Agent { .. } => DeviceKind::Agent,
            Self::QuickSupport => DeviceKind::QuickSupport,
            Self::Technician { .. } => DeviceKind::Technician,
        }
    }
}

/// Loads the local identity, creating and registering it if needed.
pub async fn ensure_enrolled(
    api: &ApiClient,
    store: &dyn IdentityStore,
    enrollment: Enrollment<'_>,
    info: DeviceInfo,
) -> Result<(IdentityKey, Registration), ClientError> {
    let stored = match store.load()? {
        Some(s) => s,
        None => {
            let s = StoredIdentity {
                key: IdentityKey::generate(),
                registration: None,
            };
            store.save(&s)?;
            s
        }
    };
    if let Some(reg) = stored.registration {
        return Ok((stored.key, reg));
    }
    let server = api.server_info().await?;
    if server.token_keys.is_empty() {
        return Err(ClientError::Decode("server published no token keys".into()));
    }
    let challenge = api.challenge().await?;
    let signature = sign_domain(
        &stored.key,
        SignatureDomain::DeviceRegistration,
        challenge.nonce.as_bytes(),
    );
    let (token, bearer) = match &enrollment {
        Enrollment::Agent { enrollment_token } => (Some((*enrollment_token).to_owned()), None),
        Enrollment::QuickSupport => (None, None),
        Enrollment::Technician { access_token } => (None, Some(*access_token)),
    };
    let resp = api
        .register_device(
            &RegisterDeviceRequest {
                public_key: stored.key.public(),
                kind: enrollment.kind(),
                nonce: challenge.nonce,
                signature,
                info,
                enrollment_token: token,
            },
            bearer,
        )
        .await?;
    let reg = Registration {
        device_id: resp.device_id,
        friendly_id: resp.friendly_id,
        kind: enrollment.kind(),
        server_url: api.base().to_string(),
        token_keys: server.token_keys,
    };
    let saved = StoredIdentity {
        key: stored.key,
        registration: Some(reg.clone()),
    };
    store.save(&saved)?;
    Ok((saved.key, reg))
}

struct CachedLogin {
    response: DeviceLoginResponse,
    renew_at: Instant,
}

/// Authenticated device: issues device tokens and relay grants on demand.
pub struct DeviceSession {
    api: ApiClient,
    key: IdentityKey,
    registration: Registration,
    info: Option<DeviceInfo>,
    cache: Mutex<Option<CachedLogin>>,
}

/// Renew this long before the device token expires.
const RENEW_MARGIN: Duration = Duration::from_secs(300);

impl DeviceSession {
    pub fn new(api: ApiClient, key: IdentityKey, registration: Registration, info: Option<DeviceInfo>) -> Self {
        Self {
            api,
            key,
            registration,
            info,
            cache: Mutex::new(None),
        }
    }

    pub fn key(&self) -> &IdentityKey {
        &self.key
    }

    pub fn registration(&self) -> &Registration {
        &self.registration
    }

    pub fn api(&self) -> &ApiClient {
        &self.api
    }

    /// Verifier holding the token keys pinned at enrollment (for the host's
    /// `TicketValidator`).
    pub fn ticket_verifier(&self) -> TokenVerifier {
        self.registration
            .token_keys
            .iter()
            .fold(TokenVerifier::new(), |v, k| v.with_key(*k))
    }

    async fn login(&self) -> Result<DeviceLoginResponse, ClientError> {
        let challenge = self.api.challenge().await?;
        let signature = sign_domain(&self.key, SignatureDomain::DeviceLogin, challenge.nonce.as_bytes());
        self.api
            .device_login(&DeviceLoginRequest {
                public_key: self.key.public(),
                nonce: challenge.nonce,
                signature,
                info: self.info.clone(),
            })
            .await
    }

    async fn current(&self) -> Result<DeviceLoginResponse, ClientError> {
        let mut cache = self.cache.lock().await;
        if let Some(c) = cache.as_ref()
            && Instant::now() < c.renew_at
        {
            return Ok(c.response.clone());
        }
        let response = self.login().await?;
        let lifetime = Duration::from_secs(response.expires_in.max(0) as u64);
        *cache = Some(CachedLogin {
            renew_at: Instant::now() + lifetime.saturating_sub(RENEW_MARGIN),
            response: response.clone(),
        });
        Ok(response)
    }

    /// A valid device token (signaling, technician session requests).
    pub async fn token(&self) -> Result<String, ClientError> {
        Ok(self.current().await?.device_token)
    }

    /// Relay grants for the transport (`aionix_transport::RelayEndpoint`).
    pub async fn relays(&self) -> Result<Vec<RelayGrant>, ClientError> {
        Ok(self.current().await?.relays)
    }

    /// Forces a fresh login next time (token rejected or expired server-side).
    pub async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }
}
