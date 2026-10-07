//! Consent-based remote session foundation.
//!
//! * [`state`]: explicit lifecycle state machine shared by both sides.
//! * [`ticket`]: host-side validation of signed session tickets, with replay protection.
//! * [`authority`]: technician-side authorization provider interface (the Control Plane
//!   will implement it); [`dev_authority`] is a DEVELOPMENT ONLY stand-in.
//! * [`approval`]: visible local approval ("Permitir" / "Recusar") on the host.
//! * [`host`] / [`technician`]: coordinators that orchestrate transport, authorization,
//!   approval, media, input, clipboard, reconnection and teardown.
//! * [`media`]: boundaries to the capture/codec/input/clipboard subsystems;
//!   `windows_media` holds the real Windows implementations.
//! * [`diagnostics`]: measured session metrics, with explicit "unavailable" values.

pub mod approval;
pub mod authority;
#[cfg(feature = "dev-authority")]
pub mod dev_authority;
pub mod diagnostics;
pub mod host;
mod link;
pub mod media;
pub mod reconnect;
pub mod state;
pub mod technician;
pub mod ticket;
#[cfg(windows)]
pub mod windows_media;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::watch;
use tracing::{info, warn};

use diagnostics::{Metric, SessionDiagnostics, SessionRole, Unavailable};
use state::{SessionEvent, SessionState, SessionStatus, StateMachine};

pub use approval::{ApprovalDecision, ApprovalPrompt, ApprovalRequest};
pub use authority::{
    AuthorityError, AuthorizationProvider, DeviceLookup, SessionGrant, SessionRequest,
    TechnicianIdentity,
};
pub use host::{HostConfig, HostService, HostServices, HostSessionHandle};
pub use reconnect::ReconnectPolicy;
pub use technician::{
    InputRejected, TechnicianConfig, TechnicianEvent, TechnicianServices, TechnicianSession,
    TechnicianSessionHandle,
};
pub use ticket::{AuthorizedSession, Permissions, TicketRejection, TicketValidator};

/// The state machine of one session plus its published status and diagnostics.
pub(crate) struct StateCell {
    machine: Mutex<StateMachine>,
    status: watch::Sender<SessionStatus>,
    diagnostics: Mutex<SessionDiagnostics>,
    role: SessionRole,
}

impl StateCell {
    pub fn new(role: SessionRole) -> Arc<Self> {
        let machine = StateMachine::new();
        let (status, _) = watch::channel(machine.status());
        Arc::new(Self {
            machine: Mutex::new(machine),
            status,
            diagnostics: Mutex::new(SessionDiagnostics::new(role)),
            role,
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<SessionStatus> {
        self.status.subscribe()
    }

    /// Applies `event`; invalid transitions are logged and ignored.
    pub fn apply(&self, event: SessionEvent) -> bool {
        let mut m = self.machine.lock().unwrap();
        let from = m.state();
        match m.apply(event) {
            Ok(to) => {
                let st = m.status();
                drop(m);
                info!(role = ?self.role, from = from.as_str(), to = to.as_str(), "session state");
                self.status.send_replace(st);
                true
            }
            Err(e) => {
                warn!(role = ?self.role, error = %e, "ignored session event");
                false
            }
        }
    }

    pub fn state(&self) -> SessionState {
        self.machine.lock().unwrap().state()
    }

    pub fn record_reconnect_attempt(&self) {
        let mut m = self.machine.lock().unwrap();
        m.record_reconnect_attempt();
        self.status.send_replace(m.status());
    }

    /// Ends the session from any non-terminal state (Ending → Ended).
    pub fn finish(&self) {
        let state = self.state();
        if state.is_terminal() {
            return;
        }
        if state != SessionState::Ending {
            self.apply(SessionEvent::End);
        }
        self.apply(SessionEvent::Closed);
    }

    pub fn update_diagnostics(&self, f: impl FnOnce(&mut SessionDiagnostics)) {
        f(&mut self.diagnostics.lock().unwrap());
    }

    pub fn diagnostics(&self) -> SessionDiagnostics {
        let mut d = self.diagnostics.lock().unwrap().clone();
        let m = self.machine.lock().unwrap();
        d.state = m.state();
        d.reconnect_count = m.status().reconnect_count;
        d.duration = match m.connected_for() {
            Some(t) if !d.state.is_terminal() => Metric::Available(t),
            _ => Metric::Unavailable(Unavailable::NotConnected),
        };
        if !d.state.is_live() {
            d.link_down();
        }
        d
    }
}

/// Waits until `status` satisfies `pred` or `timeout` elapses; returns the last status
/// seen either way (`Err` on timeout or when the session task is gone).
pub(crate) async fn wait_for_status(
    mut rx: watch::Receiver<SessionStatus>,
    timeout: Duration,
    pred: impl Fn(&SessionStatus) -> bool,
) -> Result<SessionStatus, SessionStatus> {
    let reached = match tokio::time::timeout(timeout, rx.wait_for(|s| pred(s))).await {
        Ok(Ok(s)) => Some(s.clone()),
        _ => None,
    };
    reached.ok_or_else(|| rx.borrow().clone())
}
