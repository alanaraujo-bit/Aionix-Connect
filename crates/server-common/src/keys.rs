//! Token key material.
//!
//! The Control Plane signs every token (access, device, relay, session ticket) with one
//! Ed25519 key. Its secret seed lives in a file mounted read-only into the API
//! container (`AIONIX_TOKEN_SIGNING_KEY_FILE`, created by `aionix-admin keygen`); it is
//! never passed through environment variables, never logged and never leaves the server.
//! Services that only verify tokens (signaling, relay) receive the public keys.

use std::path::Path;

use aionix_crypto::{IdentityKey, PublicKey, token::TokenVerifier};
use base64::Engine;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("cannot read key file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("key file {0} does not contain a base64url encoded 32-byte Ed25519 seed")]
    Format(String),
    #[error("invalid public key {0:?}")]
    PublicKey(String),
    #[error("key file {0} already exists; refusing to overwrite it")]
    Exists(String),
    #[error("cannot write key file {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
}

/// Encodes a secret seed for a key file.
pub fn encode_secret(key: &IdentityKey) -> String {
    B64.encode(*key.secret_bytes())
}

pub fn decode_secret(text: &str) -> Option<IdentityKey> {
    let bytes = B64.decode(text.trim()).ok()?;
    let seed: [u8; 32] = bytes.as_slice().try_into().ok()?;
    Some(IdentityKey::from_secret_bytes(&seed))
}

pub fn load_signing_key(path: &Path) -> Result<IdentityKey, KeyError> {
    let text = std::fs::read_to_string(path).map_err(|source| KeyError::Read {
        path: path.display().to_string(),
        source,
    })?;
    decode_secret(&text).ok_or_else(|| KeyError::Format(path.display().to_string()))
}

/// Writes a new key file (mode 0600 on Unix). Never overwrites an existing key.
pub fn write_new_signing_key(path: &Path) -> Result<IdentityKey, KeyError> {
    if path.exists() {
        return Err(KeyError::Exists(path.display().to_string()));
    }
    let key = IdentityKey::generate();
    let werr = |source| KeyError::Write {
        path: path.display().to_string(),
        source,
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(werr)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut f = opts.open(path).map_err(werr)?;
    f.write_all(format!("{}\n", encode_secret(&key)).as_bytes())
        .map_err(werr)?;
    Ok(key)
}

/// Parses a comma separated list of base64url public keys.
pub fn parse_public_keys(list: &[String]) -> Result<Vec<PublicKey>, KeyError> {
    list.iter()
        .map(|s| PublicKey::from_base64(s).map_err(|_| KeyError::PublicKey(s.clone())))
        .collect()
}

pub fn verifier_for(keys: &[PublicKey]) -> TokenVerifier {
    keys.iter()
        .fold(TokenVerifier::new(), |v, k| v.with_key(*k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_roundtrip_and_no_overwrite() {
        let dir = std::env::temp_dir().join(format!("aionix-key-{}", uuid::Uuid::new_v4()));
        let path = dir.join("token-signing.key");
        let key = write_new_signing_key(&path).unwrap();
        let loaded = load_signing_key(&path).unwrap();
        assert_eq!(key.public(), loaded.public());
        assert!(matches!(
            write_new_signing_key(&path),
            Err(KeyError::Exists(_))
        ));
        std::fs::write(&path, "garbage").unwrap();
        assert!(matches!(load_signing_key(&path), Err(KeyError::Format(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_public_key_lists() {
        let k = IdentityKey::generate().public();
        let keys = parse_public_keys(&[k.to_base64()]).unwrap();
        assert_eq!(keys, vec![k]);
        assert!(parse_public_keys(&["nope".into()]).is_err());
    }
}
