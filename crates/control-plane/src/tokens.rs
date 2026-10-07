//! Token issuance and verification with the Control Plane signing key.
//!
//! Reuses the existing `axt1` format and claim sets unchanged (ADR-0006): access tokens
//! (15 min), device tokens (1 h), relay grants (12 h) and session tickets (90 s), each
//! with a distinct type so one can never be accepted as another.

use aionix_control_api::claims::{
    AccessClaims, DeviceClaims, RelayClaims, SessionTicketClaims,
};
use aionix_crypto::{
    IdentityKey, PublicKey,
    token::{Issued, TokenClaims, TokenError, TokenIssuer, TokenVerifier, Verified},
};

pub struct Tokens {
    issuer: TokenIssuer,
    verifier: TokenVerifier,
    published: Vec<PublicKey>,
}

impl Tokens {
    /// `previous` are retired keys still accepted (and published) during a rotation.
    pub fn new(signing: IdentityKey, previous: Vec<PublicKey>) -> Self {
        let issuer = TokenIssuer::new(signing);
        let mut published = vec![issuer.public_key()];
        published.extend(previous.into_iter().filter(|k| *k != issuer.public_key()));
        let verifier = published
            .iter()
            .fold(TokenVerifier::new(), |v, k| v.with_key(*k));
        Self {
            issuer,
            verifier,
            published,
        }
    }

    /// Keys clients must trust (current first).
    pub fn public_keys(&self) -> &[PublicKey] {
        &self.published
    }

    pub fn verifier(&self) -> &TokenVerifier {
        &self.verifier
    }

    fn issue<T: TokenClaims>(&self, claims: &T, ttl: time::Duration) -> Issued {
        self.issuer.issue(claims, ttl)
    }

    pub fn access(&self, claims: &AccessClaims) -> Issued {
        self.issue(claims, AccessClaims::TTL)
    }

    pub fn device(&self, claims: &DeviceClaims) -> Issued {
        self.issue(claims, DeviceClaims::TTL)
    }

    pub fn relay(&self, claims: &RelayClaims) -> Issued {
        self.issue(claims, RelayClaims::TTL)
    }

    pub fn session_ticket(&self, claims: &SessionTicketClaims) -> Issued {
        self.issue(claims, SessionTicketClaims::TTL)
    }

    pub fn verify<T: TokenClaims>(&self, token: &str) -> Result<Verified<T>, TokenError> {
        self.verifier.verify(token)
    }
}
