//! Local device identity storage.
//!
//! The Ed25519 secret is generated on the device and stays there. On Windows it is
//! wrapped with DPAPI (user scope for Technician/Quick Support, machine scope for the
//! Agent, whose file must additionally be ACL-restricted by the installer). Elsewhere
//! (tests, development) it can be stored unprotected in a file with restrictive
//! permissions.

use std::{path::PathBuf, sync::Mutex};

use aionix_crypto::{IdentityKey, PublicKey};
use aionix_shared::{DeviceKind, FriendlyId};
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::ClientError;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// What the server assigned at enrollment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub device_id: Uuid,
    pub friendly_id: FriendlyId,
    pub kind: DeviceKind,
    pub server_url: String,
    /// Token keys published by the server at enrollment, pinned for verifying
    /// session tickets on this host.
    pub token_keys: Vec<PublicKey>,
}

pub struct StoredIdentity {
    pub key: IdentityKey,
    pub registration: Option<Registration>,
}

pub trait IdentityStore: Send + Sync {
    fn load(&self) -> Result<Option<StoredIdentity>, ClientError>;
    fn save(&self, identity: &StoredIdentity) -> Result<(), ClientError>;
}

/// How the secret is protected at rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protection {
    /// Development/tests only.
    None,
    #[cfg(windows)]
    DpapiUser,
    #[cfg(windows)]
    DpapiMachine,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    protection: Protection,
    secret: String,
    #[serde(default)]
    registration: Option<Registration>,
}

pub struct FileIdentityStore {
    path: PathBuf,
    protection: Protection,
}

impl FileIdentityStore {
    pub fn new(path: impl Into<PathBuf>, protection: Protection) -> Self {
        Self {
            path: path.into(),
            protection,
        }
    }

    fn protect(&self, secret: &[u8]) -> Result<Vec<u8>, ClientError> {
        match self.protection {
            Protection::None => Ok(secret.to_vec()),
            #[cfg(windows)]
            Protection::DpapiUser => {
                aionix_platform::windows::dpapi::protect(secret, aionix_platform::DataScope::User)
                    .map_err(|e| ClientError::Local(e.to_string()))
            }
            #[cfg(windows)]
            Protection::DpapiMachine => aionix_platform::windows::dpapi::protect(
                secret,
                aionix_platform::DataScope::Machine,
            )
            .map_err(|e| ClientError::Local(e.to_string())),
        }
    }

    fn unprotect(protection: Protection, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, ClientError> {
        match protection {
            Protection::None => Ok(Zeroizing::new(blob.to_vec())),
            #[cfg(windows)]
            Protection::DpapiUser | Protection::DpapiMachine => {
                aionix_platform::windows::dpapi::unprotect(blob)
                    .map(Zeroizing::new)
                    .map_err(|e| ClientError::Local(e.to_string()))
            }
        }
    }
}

impl IdentityStore for FileIdentityStore {
    fn load(&self) -> Result<Option<StoredIdentity>, ClientError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ClientError::Local(format!("cannot read identity: {e}"))),
        };
        let f: FileFormat = serde_json::from_str(&text)
            .map_err(|e| ClientError::Local(format!("corrupt identity file: {e}")))?;
        let blob = B64
            .decode(f.secret)
            .map_err(|_| ClientError::Local("corrupt identity secret".into()))?;
        let secret = Self::unprotect(f.protection, &blob)?;
        let seed: [u8; 32] = secret
            .as_slice()
            .try_into()
            .map_err(|_| ClientError::Local("identity secret has the wrong size".into()))?;
        Ok(Some(StoredIdentity {
            key: IdentityKey::from_secret_bytes(&seed),
            registration: f.registration,
        }))
    }

    fn save(&self, identity: &StoredIdentity) -> Result<(), ClientError> {
        let secret = identity.key.secret_bytes();
        let blob = self.protect(&secret[..])?;
        let f = FileFormat {
            version: 1,
            protection: self.protection,
            secret: B64.encode(blob),
            registration: identity.registration.clone(),
        };
        let json = serde_json::to_vec_pretty(&f).map_err(|e| ClientError::Local(e.to_string()))?;
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| ClientError::Local(e.to_string()))?;
        }
        // Write-then-rename so a crash never leaves a half-written identity.
        let tmp = self.path.with_extension("tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            use std::io::Write;
            let mut file = opts
                .open(&tmp)
                .map_err(|e| ClientError::Local(e.to_string()))?;
            file.write_all(&json)
                .and_then(|_| file.sync_all())
                .map_err(|e| ClientError::Local(e.to_string()))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| ClientError::Local(e.to_string()))?;
        Ok(())
    }
}

/// In-memory store (tests, ephemeral Quick Support runs).
#[derive(Default)]
pub struct MemoryIdentityStore {
    inner: Mutex<Option<([u8; 32], Option<Registration>)>>,
}

impl IdentityStore for MemoryIdentityStore {
    fn load(&self) -> Result<Option<StoredIdentity>, ClientError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .as_ref()
            .map(|(s, r)| StoredIdentity {
                key: IdentityKey::from_secret_bytes(s),
                registration: r.clone(),
            }))
    }

    fn save(&self, identity: &StoredIdentity) -> Result<(), ClientError> {
        *self.inner.lock().unwrap() =
            Some((*identity.key.secret_bytes(), identity.registration.clone()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(protection: Protection) {
        let dir = std::env::temp_dir().join(format!("aionix-id-{}", Uuid::new_v4()));
        let store = FileIdentityStore::new(dir.join("identity.json"), protection);
        assert!(store.load().unwrap().is_none());
        let key = IdentityKey::generate();
        let reg = Registration {
            device_id: Uuid::new_v4(),
            friendly_id: FriendlyId::new(482_193_771).unwrap(),
            kind: DeviceKind::Agent,
            server_url: "https://connect.example.com".into(),
            token_keys: vec![IdentityKey::generate().public()],
        };
        store
            .save(&StoredIdentity {
                key: IdentityKey::from_secret_bytes(&key.secret_bytes()),
                registration: Some(reg.clone()),
            })
            .unwrap();
        let back = store.load().unwrap().unwrap();
        assert_eq!(back.key.public(), key.public());
        assert_eq!(back.registration, Some(reg));
        let raw = std::fs::read_to_string(dir.join("identity.json")).unwrap();
        assert!(!raw.contains(&key.public().to_base64()) || protection == Protection::None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_store_roundtrip_unprotected() {
        roundtrip(Protection::None);
    }

    #[cfg(windows)]
    #[test]
    fn file_store_roundtrip_dpapi() {
        roundtrip(Protection::DpapiUser);
    }
}
