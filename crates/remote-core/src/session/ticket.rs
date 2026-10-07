//! Host-side validation of session tickets.
//!
//! A session ticket is an `axt1` token of type `session_ticket`
//! ([`SessionTicketClaims`]) signed by the authority (control plane). It binds:
//! session id (`sid`), technician identity (`viewer`: user, device and endpoint key),
//! target device (`host`: device id and endpoint key), issue/expiry time, the access
//! mode and the granted permissions.
//!
//! The host accepts a ticket only if all of these hold:
//! * signature by a pinned authority key, correct token type, not expired;
//! * `host.key` is this device's key and `host.device_id` its device id;
//! * `viewer.key` is the key the transport handshake authenticated for the peer;
//! * `sid` is a proper id (and the expected one when the authority announced it);
//! * the mode is allowed (assisted only, for now) and `view` is granted;
//! * neither the token id nor the session id was consumed before (replay guard).
//!
//! Friendly 9-digit device ids play no role here: they are a lookup handle for humans
//! and never appear in, or substitute for, any check above.

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use aionix_control_api::claims::{SessionPermission, SessionTicketClaims, TicketViewer};
use aionix_crypto::{
    PublicKey,
    token::{DEFAULT_LEEWAY, TokenError, TokenVerifier},
};
use aionix_shared::AccessMode;
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TicketRejection {
    #[error("ticket rejected: {0}")]
    Token(#[from] TokenError),
    #[error("ticket is for another device")]
    WrongTargetDevice,
    #[error("ticket was issued to another technician")]
    WrongTechnician,
    #[error("invalid session id")]
    InvalidSessionId,
    #[error("session id was not announced to this device")]
    UnexpectedSession,
    #[error("ticket or session id already used")]
    Replayed,
    #[error("access mode {0:?} is not allowed")]
    ModeNotAllowed(AccessMode),
    #[error("ticket grants no view permission")]
    MissingPermission,
}

/// What a session is allowed to do, as granted by the ticket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Permissions {
    pub view: bool,
    pub control: bool,
    pub clipboard: bool,
}

impl Permissions {
    pub fn from_claims(perms: &[SessionPermission]) -> Self {
        Self {
            view: perms.contains(&SessionPermission::View),
            control: perms.contains(&SessionPermission::Control),
            clipboard: perms.contains(&SessionPermission::Clipboard),
        }
    }

    pub fn to_claims(self) -> Vec<SessionPermission> {
        let mut v = Vec::new();
        if self.view {
            v.push(SessionPermission::View);
        }
        if self.control {
            v.push(SessionPermission::Control);
        }
        if self.clipboard {
            v.push(SessionPermission::Clipboard);
        }
        v
    }

    pub fn all() -> Self {
        Self {
            view: true,
            control: true,
            clipboard: true,
        }
    }
}

/// A validated, consumed ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedSession {
    pub session_id: Uuid,
    pub technician: TicketViewer,
    pub mode: AccessMode,
    pub permissions: Permissions,
    pub issued_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// Remembers consumed token ids and session ids until they could no longer verify.
///
/// In memory only: after a host restart a ticket could be presented again within its
/// (90 s) lifetime; it would still need the matching technician key and, for assisted
/// sessions, a fresh local approval. See PROJECT_STATUS.md.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    consumed: HashMap<String, OffsetDateTime>,
}

impl ReplayGuard {
    fn prune(&mut self, now: OffsetDateTime) {
        self.consumed.retain(|_, until| *until > now);
    }

    fn is_consumed(&self, key: &str) -> bool {
        self.consumed.contains_key(key)
    }

    fn consume(&mut self, key: String, until: OffsetDateTime) {
        self.consumed.insert(key, until);
    }

    pub fn len(&self) -> usize {
        self.consumed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.consumed.is_empty()
    }
}

/// Validates tickets presented to this host.
#[derive(Debug)]
pub struct TicketValidator {
    verifier: TokenVerifier,
    host_device_id: Uuid,
    host_key: PublicKey,
    allowed_modes: Vec<AccessMode>,
    /// Session ids announced by the authority (session offers). Empty = no pinning.
    expected: Mutex<HashSet<Uuid>>,
    replay: Mutex<ReplayGuard>,
}

impl TicketValidator {
    /// `verifier` must hold the authority keys pinned at enrollment.
    pub fn new(verifier: TokenVerifier, host_device_id: Uuid, host_key: PublicKey) -> Self {
        Self {
            verifier,
            host_device_id,
            host_key,
            allowed_modes: vec![AccessMode::Assisted],
            expected: Mutex::new(HashSet::new()),
            replay: Mutex::new(ReplayGuard::default()),
        }
    }

    pub fn host_device_id(&self) -> Uuid {
        self.host_device_id
    }

    pub fn host_key(&self) -> PublicKey {
        self.host_key
    }

    /// Restricts accepted sessions to ids announced by the authority. Once at least one
    /// id is expected, any other id is refused.
    pub fn expect_session(&self, session_id: Uuid) {
        self.expected.lock().unwrap().insert(session_id);
    }

    pub fn validate(
        &self,
        ticket: &str,
        peer_key: &PublicKey,
    ) -> Result<AuthorizedSession, TicketRejection> {
        self.validate_at(ticket, peer_key, OffsetDateTime::now_utc())
    }

    pub fn validate_at(
        &self,
        ticket: &str,
        peer_key: &PublicKey,
        now: OffsetDateTime,
    ) -> Result<AuthorizedSession, TicketRejection> {
        self.replay.lock().unwrap().prune(now);
        let verified = self
            .verifier
            .verify_at::<SessionTicketClaims>(ticket, now)?;
        let claims = verified.claims;
        if claims.host.key != self.host_key || claims.host.device_id != self.host_device_id {
            return Err(TicketRejection::WrongTargetDevice);
        }
        if claims.viewer.key != *peer_key {
            return Err(TicketRejection::WrongTechnician);
        }
        if claims.sid.is_nil() {
            return Err(TicketRejection::InvalidSessionId);
        }
        {
            let expected = self.expected.lock().unwrap();
            if !expected.is_empty() && !expected.contains(&claims.sid) {
                return Err(TicketRejection::UnexpectedSession);
            }
        }
        if !self.allowed_modes.contains(&claims.mode) {
            return Err(TicketRejection::ModeNotAllowed(claims.mode));
        }
        let permissions = Permissions::from_claims(&claims.perms);
        if !permissions.view {
            return Err(TicketRejection::MissingPermission);
        }

        // Consume only fully valid tickets, atomically for both keys.
        let jti_key = format!("jti:{}", verified.jti);
        let sid_key = format!("sid:{}", claims.sid);
        {
            let mut replay = self.replay.lock().unwrap();
            if replay.is_consumed(&jti_key) || replay.is_consumed(&sid_key) {
                return Err(TicketRejection::Replayed);
            }
            // Keep entries for as long as the token could still verify.
            let until = verified.expires_at + DEFAULT_LEEWAY + time::Duration::seconds(1);
            replay.consume(jti_key, until);
            replay.consume(sid_key, until);
        }
        self.expected.lock().unwrap().remove(&claims.sid);

        Ok(AuthorizedSession {
            session_id: claims.sid,
            technician: claims.viewer,
            mode: claims.mode,
            permissions,
            issued_at: verified.issued_at,
            expires_at: verified.expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionix_control_api::claims::TicketHost;
    use aionix_crypto::{IdentityKey, token::TokenIssuer};
    use time::Duration;

    struct Fixture {
        issuer: TokenIssuer,
        validator: TicketValidator,
        host: IdentityKey,
        host_id: Uuid,
        tech: IdentityKey,
    }

    fn fixture() -> Fixture {
        let issuer = TokenIssuer::new(IdentityKey::generate());
        let host = IdentityKey::generate();
        let host_id = Uuid::new_v4();
        let validator = TicketValidator::new(
            TokenVerifier::new().with_key(issuer.public_key()),
            host_id,
            host.public(),
        );
        Fixture {
            issuer,
            validator,
            host,
            host_id,
            tech: IdentityKey::generate(),
        }
    }

    fn claims(f: &Fixture) -> SessionTicketClaims {
        SessionTicketClaims {
            sid: Uuid::new_v4(),
            host: TicketHost {
                device_id: f.host_id,
                key: f.host.public(),
            },
            viewer: TicketViewer {
                device_id: Uuid::new_v4(),
                key: f.tech.public(),
                user_id: Uuid::new_v4(),
                display_name: "Técnica Ana".into(),
                email: "ana@example.com".into(),
                org_name: Some("Suporte Exemplo".into()),
            },
            mode: AccessMode::Assisted,
            perms: vec![
                SessionPermission::View,
                SessionPermission::Control,
                SessionPermission::Clipboard,
            ],
        }
    }

    fn issue(f: &Fixture, c: &SessionTicketClaims) -> String {
        f.issuer.issue(c, SessionTicketClaims::TTL).token
    }

    #[test]
    fn valid_ticket_is_accepted() {
        let f = fixture();
        let c = claims(&f);
        let s = f
            .validator
            .validate(&issue(&f, &c), &f.tech.public())
            .unwrap();
        assert_eq!(s.session_id, c.sid);
        assert_eq!(s.technician.display_name, "Técnica Ana");
        assert_eq!(s.permissions, Permissions::all());
        assert!(s.expires_at - s.issued_at <= SessionTicketClaims::TTL);
    }

    #[test]
    fn expired_ticket_is_rejected() {
        let f = fixture();
        let c = claims(&f);
        let t = f
            .issuer
            .issue_at(
                &c,
                SessionTicketClaims::TTL,
                OffsetDateTime::now_utc() - Duration::minutes(10),
            )
            .token;
        assert_eq!(
            f.validator.validate(&t, &f.tech.public()),
            Err(TicketRejection::Token(TokenError::Expired))
        );
    }

    #[test]
    fn tampered_ticket_is_rejected() {
        let f = fixture();
        let t = issue(&f, &claims(&f));
        // Flip one character of the payload.
        let mut parts: Vec<String> = t.split('.').map(str::to_owned).collect();
        let mut payload: Vec<char> = parts[2].chars().collect();
        payload[10] = if payload[10] == 'A' { 'B' } else { 'A' };
        parts[2] = payload.into_iter().collect();
        let forged = parts.join(".");
        assert_eq!(
            f.validator.validate(&forged, &f.tech.public()),
            Err(TicketRejection::Token(TokenError::BadSignature))
        );
        // Ticket signed by an unknown authority.
        let rogue = TokenIssuer::new(IdentityKey::generate());
        let t = rogue.issue(&claims(&f), SessionTicketClaims::TTL).token;
        assert_eq!(
            f.validator.validate(&t, &f.tech.public()),
            Err(TicketRejection::Token(TokenError::UnknownKey))
        );
    }

    #[test]
    fn wrong_target_device_is_rejected() {
        let f = fixture();
        let mut c = claims(&f);
        c.host.key = IdentityKey::generate().public();
        assert_eq!(
            f.validator.validate(&issue(&f, &c), &f.tech.public()),
            Err(TicketRejection::WrongTargetDevice)
        );
        let mut c = claims(&f);
        c.host.device_id = Uuid::new_v4();
        assert_eq!(
            f.validator.validate(&issue(&f, &c), &f.tech.public()),
            Err(TicketRejection::WrongTargetDevice)
        );
    }

    #[test]
    fn wrong_technician_is_rejected() {
        let f = fixture();
        let t = issue(&f, &claims(&f));
        // A different (authenticated) peer presents someone else's ticket.
        let thief = IdentityKey::generate();
        assert_eq!(
            f.validator.validate(&t, &thief.public()),
            Err(TicketRejection::WrongTechnician)
        );
        // ...and the failed attempt did not burn the legitimate ticket.
        assert!(f.validator.validate(&t, &f.tech.public()).is_ok());
    }

    #[test]
    fn replayed_ticket_is_rejected() {
        let f = fixture();
        let c = claims(&f);
        let t = issue(&f, &c);
        assert!(f.validator.validate(&t, &f.tech.public()).is_ok());
        assert_eq!(
            f.validator.validate(&t, &f.tech.public()),
            Err(TicketRejection::Replayed)
        );
        // A fresh token for the same session id is a replay of the session too.
        let again = issue(&f, &c);
        assert_eq!(
            f.validator.validate(&again, &f.tech.public()),
            Err(TicketRejection::Replayed)
        );
    }

    #[test]
    fn replay_entries_expire_with_the_ticket() {
        let f = fixture();
        let t = issue(&f, &claims(&f));
        f.validator.validate(&t, &f.tech.public()).unwrap();
        assert_eq!(f.validator.replay.lock().unwrap().len(), 2);
        // Far in the future the token is expired anyway; the guard forgets it.
        let later = OffsetDateTime::now_utc() + Duration::hours(1);
        let other = issue(&f, &claims(&f));
        let _ = f.validator.validate_at(&other, &f.tech.public(), later);
        assert!(f.validator.replay.lock().unwrap().is_empty());
    }

    #[test]
    fn session_id_is_checked() {
        let f = fixture();
        let mut c = claims(&f);
        c.sid = Uuid::nil();
        assert_eq!(
            f.validator.validate(&issue(&f, &c), &f.tech.public()),
            Err(TicketRejection::InvalidSessionId)
        );
        // When the authority announced a session, only that one is accepted.
        let announced = claims(&f);
        f.validator.expect_session(announced.sid);
        assert_eq!(
            f.validator
                .validate(&issue(&f, &claims(&f)), &f.tech.public()),
            Err(TicketRejection::UnexpectedSession)
        );
        assert!(
            f.validator
                .validate(&issue(&f, &announced), &f.tech.public())
                .is_ok()
        );
    }

    #[test]
    fn unattended_and_viewless_tickets_are_rejected() {
        let f = fixture();
        let mut c = claims(&f);
        c.mode = AccessMode::Unattended;
        assert_eq!(
            f.validator.validate(&issue(&f, &c), &f.tech.public()),
            Err(TicketRejection::ModeNotAllowed(AccessMode::Unattended))
        );
        let mut c = claims(&f);
        c.perms = vec![SessionPermission::Control];
        assert_eq!(
            f.validator.validate(&issue(&f, &c), &f.tech.public()),
            Err(TicketRejection::MissingPermission)
        );
    }

    #[test]
    fn other_token_types_are_not_tickets() {
        use aionix_control_api::claims::RelayClaims;
        let f = fixture();
        let relay = f
            .issuer
            .issue(
                &RelayClaims {
                    sub: Uuid::new_v4(),
                    key: f.tech.public(),
                },
                RelayClaims::TTL,
            )
            .token;
        assert_eq!(
            f.validator.validate(&relay, &f.tech.public()),
            Err(TicketRejection::Token(TokenError::WrongType))
        );
    }
}
