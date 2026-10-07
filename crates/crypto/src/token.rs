//! Compact Ed25519-signed tokens.
//!
//! Format: `axt1.<kid>.<payload>.<signature>` where `payload` is base64url JSON and the
//! signature covers `axt1.<kid>.<payload>` prefixed by a domain tag.
//!
//! Every token carries a mandatory `typ` so an access token can never be accepted as a
//! session ticket (or vice-versa), plus `iat`, `exp` and a unique `jti` usable for replay
//! detection. Verification is strict: unknown key id, bad signature, wrong type,
//! expired or not-yet-valid tokens are all rejected.

use crate::{B64, IdentityKey, PublicKey};
use base64::Engine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use time::{Duration, OffsetDateTime};

const PREFIX: &str = "axt1";
const SIGNING_TAG: &[u8] = b"aionix/v1/token\0";
/// Hard upper bound to avoid spending work on absurd inputs.
pub const MAX_TOKEN_LEN: usize = 8 * 1024;
/// Allowed clock difference between issuer and verifier.
pub const DEFAULT_LEEWAY: Duration = Duration::seconds(30);

/// Implemented by every claim set that can be carried in a token.
pub trait TokenClaims: Serialize + DeserializeOwned {
    /// Unique token type name, e.g. `"access"`, `"session_ticket"`.
    const TYPE: &'static str;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("token signed by unknown key")]
    UnknownKey,
    #[error("invalid token signature")]
    BadSignature,
    #[error("unexpected token type")]
    WrongType,
    #[error("token expired")]
    Expired,
    #[error("token not yet valid")]
    NotYetValid,
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    typ: String,
    iat: i64,
    exp: i64,
    jti: String,
    #[serde(flatten)]
    claims: T,
}

/// A freshly issued token.
#[derive(Debug, Clone)]
pub struct Issued {
    pub token: String,
    pub jti: String,
    pub issued_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// A successfully verified token.
#[derive(Debug, Clone)]
pub struct Verified<T> {
    pub claims: T,
    pub jti: String,
    pub issued_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// Signs tokens with a single active key.
pub struct TokenIssuer {
    key: IdentityKey,
    kid: String,
}

impl TokenIssuer {
    pub fn new(key: IdentityKey) -> Self {
        let kid = key.public().key_id();
        Self { key, kid }
    }

    pub fn public_key(&self) -> PublicKey {
        self.key.public()
    }

    pub fn key_id(&self) -> &str {
        &self.kid
    }

    pub fn issue<T: TokenClaims>(&self, claims: &T, ttl: Duration) -> Issued {
        self.issue_at(claims, ttl, OffsetDateTime::now_utc())
    }

    pub fn issue_at<T: TokenClaims>(
        &self,
        claims: &T,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Issued {
        let jti = crate::random_token(16);
        let expires_at = now + ttl;
        let env = Envelope {
            typ: T::TYPE.to_owned(),
            iat: now.unix_timestamp(),
            exp: expires_at.unix_timestamp(),
            jti: jti.clone(),
            claims,
        };
        let payload = B64.encode(serde_json::to_vec(&env).expect("claims serialize"));
        let signed_part = format!("{PREFIX}.{}.{payload}", self.kid);
        let sig = self.key.sign(&signing_message(&signed_part));
        Issued {
            token: format!("{signed_part}.{}", B64.encode(sig)),
            jti,
            issued_at: now,
            expires_at,
        }
    }
}

/// Verifies tokens against a set of trusted issuer keys (supports key rotation).
#[derive(Clone, Debug, Default)]
pub struct TokenVerifier {
    keys: HashMap<String, PublicKey>,
    leeway: Duration,
}

impl TokenVerifier {
    pub fn new() -> Self {
        Self {
            keys: HashMap::new(),
            leeway: DEFAULT_LEEWAY,
        }
    }

    pub fn with_key(mut self, key: PublicKey) -> Self {
        self.add_key(key);
        self
    }

    pub fn add_key(&mut self, key: PublicKey) {
        self.keys.insert(key.key_id(), key);
    }

    pub fn with_leeway(mut self, leeway: Duration) -> Self {
        self.leeway = leeway;
        self
    }

    pub fn verify<T: TokenClaims>(&self, token: &str) -> Result<Verified<T>, TokenError> {
        self.verify_at(token, OffsetDateTime::now_utc())
    }

    pub fn verify_at<T: TokenClaims>(
        &self,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<Verified<T>, TokenError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(TokenError::Malformed);
        }
        let (signed_part, sig_b64) = token.rsplit_once('.').ok_or(TokenError::Malformed)?;
        let mut parts = signed_part.split('.');
        let (Some(prefix), Some(kid), Some(payload), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        if prefix != PREFIX {
            return Err(TokenError::Malformed);
        }
        let key = self.keys.get(kid).ok_or(TokenError::UnknownKey)?;
        let sig = B64.decode(sig_b64).map_err(|_| TokenError::Malformed)?;
        key.verify(&signing_message(signed_part), &sig)
            .map_err(|_| TokenError::BadSignature)?;

        // Signature is valid: payload was produced by a trusted issuer.
        let raw = B64.decode(payload).map_err(|_| TokenError::Malformed)?;
        let typed: Envelope<serde_json::Value> =
            serde_json::from_slice(&raw).map_err(|_| TokenError::Malformed)?;
        if typed.typ != T::TYPE {
            return Err(TokenError::WrongType);
        }
        let issued_at =
            OffsetDateTime::from_unix_timestamp(typed.iat).map_err(|_| TokenError::Malformed)?;
        let expires_at =
            OffsetDateTime::from_unix_timestamp(typed.exp).map_err(|_| TokenError::Malformed)?;
        if now > expires_at + self.leeway {
            return Err(TokenError::Expired);
        }
        if now + self.leeway < issued_at {
            return Err(TokenError::NotYetValid);
        }
        let claims: T = serde_json::from_value(typed.claims).map_err(|_| TokenError::Malformed)?;
        Ok(Verified {
            claims,
            jti: typed.jti,
            issued_at,
            expires_at,
        })
    }
}

fn signing_message(signed_part: &str) -> Vec<u8> {
    let mut msg = Vec::with_capacity(SIGNING_TAG.len() + signed_part.len());
    msg.extend_from_slice(SIGNING_TAG);
    msg.extend_from_slice(signed_part.as_bytes());
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Access {
        sub: String,
    }
    impl TokenClaims for Access {
        const TYPE: &'static str = "test_access";
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Other {
        sub: String,
    }
    impl TokenClaims for Other {
        const TYPE: &'static str = "test_other";
    }

    fn setup() -> (TokenIssuer, TokenVerifier) {
        let issuer = TokenIssuer::new(IdentityKey::generate());
        let verifier = TokenVerifier::new().with_key(issuer.public_key());
        (issuer, verifier)
    }

    #[test]
    fn roundtrip() {
        let (iss, ver) = setup();
        let t = iss.issue(&Access { sub: "u1".into() }, Duration::minutes(5));
        let v: Verified<Access> = ver.verify(&t.token).unwrap();
        assert_eq!(v.claims.sub, "u1");
        assert_eq!(v.jti, t.jti);
    }

    #[test]
    fn rejects_wrong_type() {
        let (iss, ver) = setup();
        let t = iss.issue(&Access { sub: "u1".into() }, Duration::minutes(5));
        assert_eq!(
            ver.verify::<Other>(&t.token).unwrap_err(),
            TokenError::WrongType
        );
    }

    #[test]
    fn rejects_expired_and_future() {
        let (iss, ver) = setup();
        let now = OffsetDateTime::now_utc();
        let old = iss.issue_at(
            &Access { sub: "u".into() },
            Duration::minutes(1),
            now - Duration::hours(1),
        );
        assert_eq!(
            ver.verify::<Access>(&old.token).unwrap_err(),
            TokenError::Expired
        );
        let future = iss.issue_at(
            &Access { sub: "u".into() },
            Duration::minutes(1),
            now + Duration::hours(1),
        );
        assert_eq!(
            ver.verify::<Access>(&future.token).unwrap_err(),
            TokenError::NotYetValid
        );
        // Within leeway is accepted.
        let skewed = iss.issue_at(
            &Access { sub: "u".into() },
            Duration::minutes(1),
            now + Duration::seconds(10),
        );
        assert!(ver.verify::<Access>(&skewed.token).is_ok());
    }

    #[test]
    fn rejects_unknown_key_and_tampering() {
        let (iss, ver) = setup();
        let (other_iss, _) = setup();
        let t = other_iss.issue(&Access { sub: "u".into() }, Duration::minutes(5));
        assert_eq!(
            ver.verify::<Access>(&t.token).unwrap_err(),
            TokenError::UnknownKey
        );

        let t = iss.issue(&Access { sub: "user".into() }, Duration::minutes(5));
        let parts: Vec<&str> = t.token.split('.').collect();
        // Swap payload for a forged one, keep the signature.
        let forged_payload = B64.encode(
            serde_json::to_vec(&serde_json::json!({
                "typ": "test_access", "iat": 0, "exp": i64::MAX / 2, "jti": "x", "sub": "admin"
            }))
            .unwrap(),
        );
        let forged = format!("{}.{}.{}.{}", parts[0], parts[1], forged_payload, parts[3]);
        assert_eq!(
            ver.verify::<Access>(&forged).unwrap_err(),
            TokenError::BadSignature
        );
        assert_eq!(
            ver.verify::<Access>("garbage").unwrap_err(),
            TokenError::Malformed
        );
        assert_eq!(
            ver.verify::<Access>(&"a".repeat(MAX_TOKEN_LEN + 1))
                .unwrap_err(),
            TokenError::Malformed
        );
    }

    #[test]
    fn supports_key_rotation() {
        let (old, mut ver) = setup();
        let new = TokenIssuer::new(IdentityKey::generate());
        ver.add_key(new.public_key());
        let a = old.issue(&Access { sub: "a".into() }, Duration::minutes(5));
        let b = new.issue(&Access { sub: "b".into() }, Duration::minutes(5));
        assert!(ver.verify::<Access>(&a.token).is_ok());
        assert!(ver.verify::<Access>(&b.token).is_ok());
    }
}
