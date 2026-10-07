//! Host-side offer handling: connects signaling offers to the session engine.
//!
//! Accepting an offer only announces the session id to the host's `TicketValidator`
//! (which then refuses any other session id) and returns the current address. The
//! ticket is still fully validated when the technician connects, and the local user
//! must still click "Permitir" (ADR-0007). Nothing here grants access.

use std::sync::Arc;

use aionix_control_api::signal::OfferRejectReason;
use aionix_remote_core::session::ticket::TicketValidator;
use aionix_shared::AccessMode;
use async_trait::async_trait;
use tokio::sync::watch;
use tracing::info;

use crate::signaling::{OfferDecision, PresenceInfo, SessionOffer, SignalHandler};

pub struct HostOfferHandler {
    validator: Arc<TicketValidator>,
    presence: watch::Receiver<PresenceInfo>,
}

impl HostOfferHandler {
    pub fn new(validator: Arc<TicketValidator>, presence: watch::Receiver<PresenceInfo>) -> Self {
        Self {
            validator,
            presence,
        }
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
        if offer.mode != AccessMode::Assisted {
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
}
