//! Host-side offer handling: connects signaling offers to the session engine.
//!
//! Accepting an offer only announces the session id to the host's `TicketValidator`
//! (which then refuses any other session id) and returns the current address. The
//! ticket is still fully validated when the technician connects. Assisted sessions still
//! need the local user to click "Permitir" (ADR-0007); unattended sessions are offered
//! only while the local owner enabled them (ADR-0015). Nothing here grants access.

use std::sync::Arc;

use aionix_control_api::signal::OfferRejectReason;
use aionix_remote_core::session::ticket::TicketValidator;
use aionix_shared::AccessMode;
use async_trait::async_trait;
use tokio::sync::watch;
use tracing::info;
use uuid::Uuid;

use crate::signaling::{OfferDecision, PresenceInfo, SessionOffer, SignalHandler};

/// Called with the id of a session the server ended (admin, disabled account...).
pub type RevocationHook = Arc<dyn Fn(Uuid) + Send + Sync>;

pub struct HostOfferHandler {
    validator: Arc<TicketValidator>,
    presence: watch::Receiver<PresenceInfo>,
    on_revoked: Option<RevocationHook>,
}

impl HostOfferHandler {
    pub fn new(validator: Arc<TicketValidator>, presence: watch::Receiver<PresenceInfo>) -> Self {
        Self {
            validator,
            presence,
            on_revoked: None,
        }
    }

    /// `hook` ends the local session with that id, if it is the one running.
    pub fn with_revocation(mut self, hook: RevocationHook) -> Self {
        self.on_revoked = Some(hook);
        self
    }
}

#[async_trait]
impl SignalHandler for HostOfferHandler {
    async fn on_offer(&self, offer: SessionOffer) -> OfferDecision {
        let p = self.presence.borrow().clone();
        if p.busy {
            return OfferDecision {
                accept: false,
                reason: Some(OfferRejectReason::Busy),
                addr: p.addr,
            };
        }
        // Unattended offers only while the local owner enabled unattended access (ADR-0015).
        let mode_allowed = match offer.mode {
            AccessMode::Assisted => true,
            AccessMode::Unattended => p.unattended_enabled,
        };
        if !mode_allowed {
            return OfferDecision {
                accept: false,
                reason: Some(OfferRejectReason::Policy),
                addr: p.addr,
            };
        }
        info!(session_id = %offer.session_id, technician = %offer.viewer.display_name, "session offer accepted; awaiting connection and local approval");
        self.validator.expect_session(offer.session_id);
        OfferDecision {
            accept: true,
            reason: None,
            addr: p.addr,
        }
    }

    async fn on_session_revoked(&self, session_id: Uuid) {
        info!(%session_id, "server ended the session");
        if let Some(hook) = &self.on_revoked {
            hook(session_id);
        }
    }
}
