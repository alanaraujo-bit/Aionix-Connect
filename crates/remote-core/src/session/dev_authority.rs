//! **DEVELOPMENT ONLY** in-process session authority.
//!
//! Stands in for the Control Plane in tests and local experiments: it holds its own
//! signing key, a table of hosts registered by the caller, and issues session tickets
//! to whoever asks, trusting the technician identity it is given. It performs **no**
//! account authentication, organization policy or audit logging and must never be
//! shipped in a product build. It is compiled only with the `dev-authority` feature.

use std::{collections::HashMap, sync::Mutex};

use aionix_control_api::{
    claims::{SessionTicketClaims, TicketHost, TicketViewer},
    signal::EndpointAddrDto,
};
use aionix_crypto::{
    IdentityKey, PublicKey,
    token::{TokenIssuer, TokenVerifier},
};
use aionix_shared::{AccessMode, FriendlyId};
use async_trait::async_trait;
use tracing::warn;
use uuid::Uuid;

use super::authority::{
    AuthorityError, AuthorizationProvider, DeviceLookup, SessionGrant, SessionRequest,
};

#[derive(Debug, Clone)]
struct DevHost {
    device_id: Uuid,
    friendly_id: FriendlyId,
    key: PublicKey,
    addr: EndpointAddrDto,
}

/// DEVELOPMENT ONLY. See the module documentation.
pub struct DevelopmentAuthority {
    issuer: TokenIssuer,
    hosts: Mutex<HashMap<Uuid, DevHost>>,
}

impl DevelopmentAuthority {
    pub fn new() -> Self {
        warn!(
            "DEVELOPMENT ONLY session authority in use: tickets are issued without real authorization"
        );
        Self {
            issuer: TokenIssuer::new(IdentityKey::generate()),
            hosts: Mutex::new(HashMap::new()),
        }
    }

    /// The verifier a host must pin to accept this authority's tickets.
    pub fn verifier(&self) -> TokenVerifier {
        TokenVerifier::new().with_key(self.issuer.public_key())
    }

    pub fn register_host(
        &self,
        device_id: Uuid,
        friendly_id: FriendlyId,
        key: PublicKey,
        addr: EndpointAddrDto,
    ) {
        self.hosts.lock().unwrap().insert(
            device_id,
            DevHost {
                device_id,
                friendly_id,
                key,
                addr,
            },
        );
    }

    fn find(&self, target: DeviceLookup) -> Option<DevHost> {
        let hosts = self.hosts.lock().unwrap();
        match target {
            DeviceLookup::Id(id) => hosts.get(&id).cloned(),
            DeviceLookup::Friendly(f) => hosts.values().find(|h| h.friendly_id == f).cloned(),
        }
    }
}

impl Default for DevelopmentAuthority {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AuthorizationProvider for DevelopmentAuthority {
    async fn request_session(
        &self,
        request: SessionRequest,
    ) -> Result<SessionGrant, AuthorityError> {
        if request.mode != AccessMode::Assisted {
            return Err(AuthorityError::Forbidden(
                "only assisted sessions are supported".into(),
            ));
        }
        let host = self
            .find(request.target)
            .ok_or(AuthorityError::DeviceNotFound)?;
        let t = request.technician;
        let claims = SessionTicketClaims {
            sid: Uuid::new_v4(),
            host: TicketHost {
                device_id: host.device_id,
                key: host.key,
            },
            viewer: TicketViewer {
                device_id: t.device_id,
                key: t.key,
                user_id: t.user_id,
                display_name: t.display_name,
                email: t.email,
                org_name: t.org_name,
            },
            mode: request.mode,
            perms: request.permissions.clone(),
        };
        let issued = self.issuer.issue(&claims, SessionTicketClaims::TTL);
        Ok(SessionGrant {
            session_id: claims.sid,
            ticket: issued.token,
            expires_at: issued.expires_at,
            host_device_id: host.device_id,
            host_key: host.key,
            host_addr: host.addr,
            permissions: request.permissions,
        })
    }
}
