//! Explicit session lifecycle.
//!
//! Both coordinators (host and technician) drive the same machine. Every change goes
//! through [`transition`], which is the single table of legal moves; anything else is an
//! [`InvalidTransition`] and leaves the state untouched.
//!
//! ```text
//! Idle ─▶ Requesting ─▶ Authenticating ─▶ AwaitingApproval ─▶ Establishing ─▶ Connected ⇄ Degraded
//!             │               │                  │                                │  ▲
//!             └──────────────┴─▶ Rejected ◀─────┘                    ConnectionLost  Reconnected
//!                                                                                ▼  │
//!   any non-terminal ─ Fail ─▶ Failed                                        Reconnecting
//!   any non-terminal ─ End ──▶ Ending ─ Closed ─▶ Ended
//! ```
//!
//! `Rejected`, `Failed` and `Ended` are terminal.

use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Nothing happened yet.
    Idle,
    /// Technician: asking the authority for a ticket and reaching the host.
    /// Host: a connection arrived and announced a session request.
    Requesting,
    /// The host is validating the session ticket.
    Authenticating,
    /// The ticket is valid; the local user on the host has to answer the prompt.
    AwaitingApproval,
    /// Approved; media, input and clipboard channels are being started.
    Establishing,
    /// Media flows.
    Connected,
    /// Connected, but the link is congested (quality reduced).
    Degraded,
    /// The transport dropped; the session is kept for a bounded resume window.
    Reconnecting,
    /// Authorization or local approval refused the session (terminal).
    Rejected,
    /// Unrecoverable error (terminal).
    Failed,
    /// Orderly shutdown in progress.
    Ending,
    /// Orderly shutdown completed (terminal).
    Ended,
}

impl SessionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Rejected | Self::Failed | Self::Ended)
    }

    /// Media and input may flow.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Connected | Self::Degraded)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Requesting => "requesting",
            Self::Authenticating => "authenticating",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Establishing => "establishing",
            Self::Connected => "connected",
            Self::Degraded => "degraded",
            Self::Reconnecting => "reconnecting",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Ending => "ending",
            Self::Ended => "ended",
        }
    }
}

/// Why a session was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum RejectReason {
    /// Ticket missing, forged, expired, replayed or not meant for this pair of peers.
    InvalidTicket(String),
    /// The local user clicked "Recusar" (or closed the prompt).
    UserRejected,
    /// Nobody answered the prompt in time.
    ApprovalTimeout,
    /// The prompt could not be shown; access is never granted without it.
    ApprovalUnavailable(String),
    /// The host already serves a session.
    Busy,
    /// Refused by policy (authority or host configuration).
    Policy(String),
}

/// Inputs of the state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// Idle → Requesting.
    Start,
    /// Requesting → Authenticating.
    Authenticate,
    /// Authenticating → AwaitingApproval.
    AwaitApproval,
    /// AwaitingApproval → Establishing.
    Approved,
    /// Establishing → Connected.
    Established,
    /// Connected → Degraded.
    Degrade,
    /// Degraded → Connected.
    Recover,
    /// Connected | Degraded → Reconnecting.
    ConnectionLost,
    /// Reconnecting → Connected.
    Reconnected,
    /// Requesting | Authenticating | AwaitingApproval → Rejected.
    Reject(RejectReason),
    /// Any non-terminal → Failed.
    Fail(String),
    /// Any non-terminal state except Ending → Ending.
    End,
    /// Ending → Ended.
    Closed,
}

impl SessionEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Authenticate => "authenticate",
            Self::AwaitApproval => "await_approval",
            Self::Approved => "approved",
            Self::Established => "established",
            Self::Degrade => "degrade",
            Self::Recover => "recover",
            Self::ConnectionLost => "connection_lost",
            Self::Reconnected => "reconnected",
            Self::Reject(_) => "reject",
            Self::Fail(_) => "fail",
            Self::End => "end",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid session transition: {event} in state {from:?}")]
pub struct InvalidTransition {
    pub from: SessionState,
    pub event: &'static str,
}

/// The transition table. Pure function: no side effects.
pub fn transition(
    from: SessionState,
    event: &SessionEvent,
) -> Result<SessionState, InvalidTransition> {
    use SessionEvent as E;
    use SessionState as S;
    let to = match (from, event) {
        (S::Idle, E::Start) => S::Requesting,
        (S::Requesting, E::Authenticate) => S::Authenticating,
        (S::Authenticating, E::AwaitApproval) => S::AwaitingApproval,
        (S::AwaitingApproval, E::Approved) => S::Establishing,
        (S::Establishing, E::Established) => S::Connected,
        (S::Connected, E::Degrade) => S::Degraded,
        (S::Degraded, E::Recover) => S::Connected,
        (S::Connected | S::Degraded, E::ConnectionLost) => S::Reconnecting,
        (S::Reconnecting, E::Reconnected) => S::Connected,
        (S::Requesting | S::Authenticating | S::AwaitingApproval, E::Reject(_)) => S::Rejected,
        (s, E::Fail(_)) if !s.is_terminal() => S::Failed,
        (s, E::End) if !s.is_terminal() && s != S::Ending => S::Ending,
        (S::Ending, E::Closed) => S::Ended,
        _ => {
            return Err(InvalidTransition {
                from,
                event: event.name(),
            });
        }
    };
    Ok(to)
}

/// Externally visible summary of a session's lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionStatus {
    pub state: SessionState,
    /// Times the session entered `Reconnecting`.
    pub reconnect_count: u32,
    /// Resume attempts made in the current (or last) reconnection episode.
    pub reconnect_attempts: u32,
    pub reject_reason: Option<RejectReason>,
    pub failure: Option<String>,
}

/// State plus bookkeeping derived from the transitions.
#[derive(Debug, Clone)]
pub struct StateMachine {
    state: SessionState,
    entered_at: Instant,
    connected_at: Option<Instant>,
    reconnect_count: u32,
    reconnect_attempts: u32,
    reject_reason: Option<RejectReason>,
    failure: Option<String>,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
            entered_at: Instant::now(),
            connected_at: None,
            reconnect_count: 0,
            reconnect_attempts: 0,
            reject_reason: None,
            failure: None,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Applies `event`. On error the state is unchanged.
    pub fn apply(&mut self, event: SessionEvent) -> Result<SessionState, InvalidTransition> {
        let to = transition(self.state, &event)?;
        match event {
            SessionEvent::Established => self.connected_at = Some(Instant::now()),
            SessionEvent::ConnectionLost => {
                self.reconnect_count += 1;
                self.reconnect_attempts = 0;
            }
            SessionEvent::Reject(reason) => self.reject_reason = Some(reason),
            SessionEvent::Fail(reason) => self.failure = Some(reason),
            _ => {}
        }
        self.state = to;
        self.entered_at = Instant::now();
        Ok(to)
    }

    pub fn record_reconnect_attempt(&mut self) {
        self.reconnect_attempts += 1;
    }

    pub fn time_in_state(&self) -> Duration {
        self.entered_at.elapsed()
    }

    /// Time since the session first became `Connected`.
    pub fn connected_for(&self) -> Option<Duration> {
        self.connected_at.map(|t| t.elapsed())
    }

    pub fn status(&self) -> SessionStatus {
        SessionStatus {
            state: self.state,
            reconnect_count: self.reconnect_count,
            reconnect_attempts: self.reconnect_attempts,
            reject_reason: self.reject_reason.clone(),
            failure: self.failure.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SessionEvent as E;
    use SessionState as S;

    fn drive(events: &[SessionEvent]) -> StateMachine {
        let mut m = StateMachine::new();
        for e in events {
            m.apply(e.clone()).unwrap_or_else(|err| panic!("{err}"));
        }
        m
    }

    const HAPPY: &[SessionEvent] = &[
        E::Start,
        E::Authenticate,
        E::AwaitApproval,
        E::Approved,
        E::Established,
    ];

    #[test]
    fn happy_path_reaches_connected_and_ends() {
        let mut m = drive(HAPPY);
        assert_eq!(m.state(), S::Connected);
        assert!(m.connected_for().is_some());
        assert_eq!(m.apply(E::End), Ok(S::Ending));
        assert_eq!(m.apply(E::Closed), Ok(S::Ended));
        assert!(m.state().is_terminal());
    }

    #[test]
    fn degraded_and_recovered() {
        let mut m = drive(HAPPY);
        assert_eq!(m.apply(E::Degrade), Ok(S::Degraded));
        assert!(m.state().is_live());
        assert_eq!(m.apply(E::Recover), Ok(S::Connected));
    }

    #[test]
    fn reconnect_cycle_counts_episodes() {
        let mut m = drive(HAPPY);
        assert_eq!(m.apply(E::ConnectionLost), Ok(S::Reconnecting));
        m.record_reconnect_attempt();
        m.record_reconnect_attempt();
        assert_eq!(m.status().reconnect_attempts, 2);
        assert_eq!(m.apply(E::Reconnected), Ok(S::Connected));
        m.apply(E::Degrade).unwrap();
        assert_eq!(m.apply(E::ConnectionLost), Ok(S::Reconnecting));
        let st = m.status();
        assert_eq!(st.reconnect_count, 2);
        assert_eq!(st.reconnect_attempts, 0, "reset per episode");
    }

    #[test]
    fn reconnect_failure_is_terminal() {
        let mut m = drive(HAPPY);
        m.apply(E::ConnectionLost).unwrap();
        assert_eq!(m.apply(E::Fail("attempts exhausted".into())), Ok(S::Failed));
        assert_eq!(m.status().failure.as_deref(), Some("attempts exhausted"));
        assert!(m.apply(E::Reconnected).is_err());
        assert!(m.apply(E::End).is_err(), "terminal states accept nothing");
    }

    #[test]
    fn rejection_records_reason() {
        let mut m = drive(&[E::Start, E::Authenticate, E::AwaitApproval]);
        assert_eq!(
            m.apply(E::Reject(RejectReason::UserRejected)),
            Ok(S::Rejected)
        );
        assert_eq!(m.status().reject_reason, Some(RejectReason::UserRejected));
        assert!(m.apply(E::Approved).is_err());
    }

    #[test]
    fn invalid_transitions_are_rejected_and_leave_state_unchanged() {
        let mut m = StateMachine::new();
        // Cannot skip authentication or approval.
        for e in [
            E::Approved,
            E::Established,
            E::AwaitApproval,
            E::Reconnected,
        ] {
            let err = m.apply(e).unwrap_err();
            assert_eq!(err.from, S::Idle);
            assert_eq!(m.state(), S::Idle);
        }
        let mut m = drive(&[E::Start, E::Authenticate]);
        // A valid ticket alone never establishes an assisted session.
        assert!(m.apply(E::Approved).is_err());
        assert!(m.apply(E::Established).is_err());
        assert_eq!(m.state(), S::Authenticating);
        // Live sessions cannot be "rejected" after the fact.
        let mut m = drive(HAPPY);
        assert!(m.apply(E::Reject(RejectReason::Busy)).is_err());
        // Reconnection only from a live session.
        let mut m = drive(&[E::Start]);
        assert!(m.apply(E::ConnectionLost).is_err());
        // Ending twice is invalid; Closed only from Ending.
        let mut m = drive(HAPPY);
        assert!(m.apply(E::Closed).is_err());
        m.apply(E::End).unwrap();
        assert!(m.apply(E::End).is_err());
    }

    #[test]
    fn every_non_terminal_state_can_fail_or_end() {
        let all = [
            S::Idle,
            S::Requesting,
            S::Authenticating,
            S::AwaitingApproval,
            S::Establishing,
            S::Connected,
            S::Degraded,
            S::Reconnecting,
            S::Ending,
        ];
        for s in all {
            assert_eq!(transition(s, &E::Fail("x".into())), Ok(S::Failed), "{s:?}");
            if s != S::Ending {
                assert_eq!(transition(s, &E::End), Ok(S::Ending), "{s:?}");
            }
        }
        for s in [S::Rejected, S::Failed, S::Ended] {
            assert!(transition(s, &E::Fail("x".into())).is_err());
            assert!(transition(s, &E::End).is_err());
        }
    }
}
