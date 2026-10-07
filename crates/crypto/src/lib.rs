//! Cryptographic primitives used across Aionix Connect.
//!
//! * [`IdentityKey`]: Ed25519 key pair. Device identities, technician workstations and
//!   the control plane's token issuer all use this type. A device's public key is also
//!   its transport endpoint id, so the peer authenticated by the QUIC/TLS handshake is
//!   exactly the identity the control plane knows.
//! * [`token`]: compact Ed25519-signed tokens (access tokens, session tickets, relay
//!   grants) with mandatory type, expiry and key id checks.
//! * [`sign_domain`]/[`verify_domain`]: domain separated signatures for challenges so a
//!   signature produced for one purpose can never be replayed for another.

pub mod token;

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use zeroize::Zeroizing;

pub use ed25519_dalek::{PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH};

pub(crate) const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("invalid key encoding")]
    InvalidKey,
    #[error("invalid signature encoding")]
    InvalidSignatureEncoding,
    #[error("signature verification failed")]
    BadSignature,
}

/// An Ed25519 key pair. The secret is zeroized on drop.
pub struct IdentityKey {
    signing: SigningKey,
}

impl IdentityKey {
    /// Generates a new key from the operating system CSPRNG.
    pub fn generate() -> Self {
        let mut seed = Zeroizing::new([0u8; 32]);
        rand::fill(&mut seed[..]);
        Self { signing: SigningKey::from_bytes(&seed) }
    }

    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Self {
        Self { signing: SigningKey::from_bytes(bytes) }
    }

    /// Raw secret seed. Callers must store it protected (DPAPI, secrets file with
    /// restrictive permissions) and should keep the returned value short-lived.
    pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.signing.to_bytes())
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.signing.verifying_key())
    }

    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LENGTH] {
        self.signing.sign(message).to_bytes()
    }
}

impl fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentityKey").field("public", &self.public()).finish_non_exhaustive()
    }
}

/// An Ed25519 public key. Serialized as unpadded base64url.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        let arr: [u8; PUBLIC_KEY_LENGTH] = bytes.try_into().map_err(|_| CryptoError::InvalidKey)?;
        VerifyingKey::from_bytes(&arr).map(Self).map_err(|_| CryptoError::InvalidKey)
    }

    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_LENGTH] {
        self.0.to_bytes()
    }

    pub fn to_base64(&self) -> String {
        B64.encode(self.0.as_bytes())
    }

    pub fn from_base64(s: &str) -> Result<Self, CryptoError> {
        let bytes = B64.decode(s).map_err(|_| CryptoError::InvalidKey)?;
        Self::from_bytes(&bytes)
    }

    /// Short stable identifier used as token key id: first 8 bytes of SHA-256, hex.
    pub fn key_id(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        hex::encode(&digest[..8])
    }

    /// Strict verification (rejects small-order / malleable signatures).
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        let sig = Signature::from_slice(signature).map_err(|_| CryptoError::InvalidSignatureEncoding)?;
        self.0.verify_strict(message, &sig).map_err(|_| CryptoError::BadSignature)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_base64())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64())
    }
}

impl Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_base64(&s).map_err(serde::de::Error::custom)
    }
}

/// Purposes for which domain-separated signatures are produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureDomain {
    /// Proof of key possession when registering a new device identity.
    DeviceRegistration,
    /// Answer to the control plane's signaling login challenge.
    DeviceLogin,
}

impl SignatureDomain {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::DeviceRegistration => b"aionix/v1/device-registration",
            Self::DeviceLogin => b"aionix/v1/device-login",
        }
    }
}

fn domain_message(domain: SignatureDomain, payload: &[u8]) -> Vec<u8> {
    let tag = domain.tag();
    let mut msg = Vec::with_capacity(tag.len() + 1 + payload.len());
    msg.extend_from_slice(tag);
    msg.push(0);
    msg.extend_from_slice(payload);
    msg
}

/// Signs `payload` bound to `domain`; returns base64url signature.
pub fn sign_domain(key: &IdentityKey, domain: SignatureDomain, payload: &[u8]) -> String {
    B64.encode(key.sign(&domain_message(domain, payload)))
}

pub fn verify_domain(
    key: &PublicKey,
    domain: SignatureDomain,
    payload: &[u8],
    signature_b64: &str,
) -> Result<(), CryptoError> {
    let sig = B64.decode(signature_b64).map_err(|_| CryptoError::InvalidSignatureEncoding)?;
    key.verify(&domain_message(domain, payload), &sig)
}

/// Random bytes encoded as base64url, for nonces and opaque secrets.
pub fn random_token(bytes: usize) -> String {
    let mut buf = Zeroizing::new(vec![0u8; bytes]);
    rand::fill(&mut buf[..]);
    B64.encode(&buf[..])
}

/// SHA-256 as lowercase hex. Used to store opaque secrets (refresh tokens,
/// enrollment tokens) without keeping the secret itself.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Constant-time equality for secrets.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip_and_sign() {
        let k = IdentityKey::generate();
        let restored = IdentityKey::from_secret_bytes(&k.secret_bytes());
        assert_eq!(k.public(), restored.public());
        let sig = k.sign(b"hello");
        assert!(k.public().verify(b"hello", &sig).is_ok());
        assert_eq!(k.public().verify(b"hellO", &sig), Err(CryptoError::BadSignature));
    }

    #[test]
    fn public_key_base64_roundtrip() {
        let k = IdentityKey::generate().public();
        assert_eq!(PublicKey::from_base64(&k.to_base64()).unwrap(), k);
        assert!(PublicKey::from_base64("not-a-key").is_err());
    }

    #[test]
    fn domain_signatures_do_not_cross_domains() {
        let k = IdentityKey::generate();
        let sig = sign_domain(&k, SignatureDomain::DeviceRegistration, b"nonce");
        assert!(verify_domain(&k.public(), SignatureDomain::DeviceRegistration, b"nonce", &sig).is_ok());
        assert!(verify_domain(&k.public(), SignatureDomain::DeviceLogin, b"nonce", &sig).is_err());
        assert!(verify_domain(&k.public(), SignatureDomain::DeviceRegistration, b"nonce2", &sig).is_err());
    }

    #[test]
    fn ct_eq_works() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }
}
