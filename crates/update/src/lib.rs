//! Desktop app updates.
//!
//! Trust model: every release publishes, per product,
//!
//! ```text
//! <base>/<product>/latest.json      manifest (version, installer URL, size, sha256)
//! <base>/<product>/latest.json.sig  ed25519 signature of the exact manifest bytes
//! ```
//!
//! The signature uses the domain-separated `ReleaseManifest` tag and must come from one
//! of the keys compiled in from `branding/release-keys.txt`. The apps accept a manifest
//! only if the signature verifies, the product matches and the version is strictly newer
//! than the running one (no downgrade/replay); the downloaded installer must then match
//! the signed size and SHA-256 before it is ever executed. Compromising the website or
//! DNS therefore cannot push code: only the offline release key can.

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use aionix_crypto::{PublicKey, SignatureDomain};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Trusted release keys, one base64url ed25519 public key per non-comment line.
const RELEASE_KEYS: &str = include_str!("../../../branding/release-keys.txt");

/// Upper bound for a manifest (it is a few hundred bytes).
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Upper bound for an installer download.
pub const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;

/// The installable desktop products.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Product {
    /// "Aionix Connect": the computer being supported.
    Host,
    /// "Aionix Connect Técnico".
    Technician,
}

impl Product {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Technician => "technician",
        }
    }
}

impl FromStr for Product {
    type Err = UpdateError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "host" => Ok(Self::Host),
            "technician" => Ok(Self::Technician),
            _ => Err(UpdateError::Invalid(format!("produto desconhecido: {s}"))),
        }
    }
}

/// `major.minor.patch` (pre-release/build suffixes are not used by this project).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u32, pub u32, pub u32);

impl FromStr for Version {
    type Err = UpdateError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || UpdateError::Invalid(format!("versão inválida: {s}"));
        let mut it = s.trim().trim_start_matches('v').split('.');
        let mut part = || -> Result<u32, UpdateError> {
            it.next().ok_or_else(bad)?.parse().map_err(|_| bad())
        };
        let v = Self(part()?, part()?, part()?);
        if it.next().is_some() {
            return Err(bad());
        }
        Ok(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

impl Serialize for Version {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// The installer referenced by a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installer {
    /// Absolute https URL of the setup executable.
    pub url: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the setup executable.
    pub sha256: String,
}

/// `latest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub product: Product,
    pub version: Version,
    /// RFC 3339 UTC.
    pub published_at: String,
    /// Short release notes (pt-BR), one item per entry.
    #[serde(default)]
    pub notes: Vec<String>,
    pub installer: Installer,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("{0}")]
    Invalid(String),
    #[error("assinatura da atualização inválida")]
    BadSignature,
    #[error("nenhuma chave de release configurada neste build")]
    NoTrustedKeys,
    #[error("o arquivo baixado não confere com a atualização assinada")]
    HashMismatch,
    #[error("falha de rede: {0}")]
    Network(String),
    #[error("falha ao gravar: {0}")]
    Io(#[from] std::io::Error),
}

/// Keys compiled into this build.
pub fn trusted_keys() -> Vec<PublicKey> {
    RELEASE_KEYS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| PublicKey::from_base64(l).ok())
        .collect()
}

/// Verifies `signature_b64` over the exact `manifest_bytes` with any of `keys`, then
/// parses and checks the manifest for `product`.
pub fn verify_manifest(
    manifest_bytes: &[u8],
    signature_b64: &str,
    keys: &[PublicKey],
    product: Product,
) -> Result<Manifest, UpdateError> {
    if keys.is_empty() {
        return Err(UpdateError::NoTrustedKeys);
    }
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(UpdateError::Invalid("manifesto grande demais".into()));
    }
    let sig = signature_b64.trim();
    let ok = keys.iter().any(|k| {
        aionix_crypto::verify_domain(k, SignatureDomain::ReleaseManifest, manifest_bytes, sig)
            .is_ok()
    });
    if !ok {
        return Err(UpdateError::BadSignature);
    }
    let m: Manifest = serde_json::from_slice(manifest_bytes)
        .map_err(|e| UpdateError::Invalid(format!("manifesto ilegível: {e}")))?;
    if m.product != product {
        return Err(UpdateError::Invalid("manifesto de outro produto".into()));
    }
    if !m.installer.url.starts_with("https://") {
        return Err(UpdateError::Invalid("instalador sem https".into()));
    }
    if m.installer.size == 0 || m.installer.size > MAX_INSTALLER_BYTES {
        return Err(UpdateError::Invalid(
            "tamanho de instalador inválido".into(),
        ));
    }
    if m.installer.sha256.len() != 64 || !m.installer.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(UpdateError::Invalid("sha256 inválido".into()));
    }
    Ok(m)
}

/// Signs manifest bytes (release tooling).
pub fn sign_manifest(key: &aionix_crypto::IdentityKey, manifest_bytes: &[u8]) -> String {
    aionix_crypto::sign_domain(key, SignatureDomain::ReleaseManifest, manifest_bytes)
}

/// True when `candidate` should replace `current`.
pub fn is_newer(current: Version, candidate: Version) -> bool {
    candidate.cmp(&current) == Ordering::Greater
}

/// Lowercase hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Client for one product's update channel.
#[derive(Clone)]
pub struct UpdateClient {
    http: reqwest::Client,
    base: String,
    product: Product,
    current: Version,
    keys: Vec<PublicKey>,
}

impl UpdateClient {
    /// `base` is e.g. `https://aionixconnect.aionixdev.com/updates`.
    pub fn new(base: &str, product: Product, current: Version) -> Result<Self, UpdateError> {
        Self::with_keys(base, product, current, trusted_keys())
    }

    pub fn with_keys(
        base: &str,
        product: Product,
        current: Version,
        keys: Vec<PublicKey>,
    ) -> Result<Self, UpdateError> {
        let http = reqwest::Client::builder()
            .user_agent(format!("aionix-updater/{current} ({})", product.slug()))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            product,
            current,
            keys,
        })
    }

    pub fn current(&self) -> Version {
        self.current
    }

    async fn get_bytes(&self, url: &str, limit: usize) -> Result<Vec<u8>, UpdateError> {
        let resp = self
            .http
            .get(url)
            .header("Cache-Control", "no-cache")
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        if body.len() > limit {
            return Err(UpdateError::Invalid("resposta grande demais".into()));
        }
        Ok(body.to_vec())
    }

    /// Fetches and verifies the channel; `Some` only for a strictly newer version.
    pub async fn check(&self) -> Result<Option<Manifest>, UpdateError> {
        let url = format!("{}/{}/latest.json", self.base, self.product.slug());
        let manifest = self.get_bytes(&url, MAX_MANIFEST_BYTES).await?;
        let sig = self.get_bytes(&format!("{url}.sig"), 1024).await?;
        let sig = String::from_utf8(sig).map_err(|_| UpdateError::BadSignature)?;
        let m = verify_manifest(&manifest, &sig, &self.keys, self.product)?;
        Ok(is_newer(self.current, m.version).then_some(m))
    }

    /// Downloads the installer of a verified manifest into `dir`, checking size and
    /// SHA-256. `progress(done, total)` is called as bytes arrive. Returns the path of a
    /// verified file; anything that does not match is deleted.
    pub async fn download(
        &self,
        m: &Manifest,
        dir: &Path,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<PathBuf, UpdateError> {
        use tokio::io::AsyncWriteExt;
        std::fs::create_dir_all(dir)?;
        let name = format!("aionix-{}-setup-{}.exe", self.product.slug(), m.version);
        let final_path = dir.join(&name);
        // Already downloaded and intact (e.g. a previous run): reuse it.
        if final_path.exists() && sha256_file(&final_path)? == m.installer.sha256 {
            progress(m.installer.size, m.installer.size);
            return Ok(final_path);
        }
        let part = dir.join(format!("{name}.part"));
        let mut resp = self
            .http
            .get(&m.installer.url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        let total = m.installer.size;
        let mut file = tokio::fs::File::create(&part).await?;
        let mut hasher = Sha256::new();
        let mut done = 0u64;
        loop {
            let chunk = match resp.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    drop(file);
                    let _ = std::fs::remove_file(&part);
                    return Err(UpdateError::Network(e.to_string()));
                }
            };
            done += chunk.len() as u64;
            if done > total {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err(UpdateError::HashMismatch);
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            progress(done, total);
        }
        file.flush().await?;
        drop(file);
        if done != total || hex::encode(hasher.finalize()) != m.installer.sha256 {
            let _ = std::fs::remove_file(&part);
            return Err(UpdateError::HashMismatch);
        }
        let _ = std::fs::remove_file(&final_path);
        std::fs::rename(&part, &final_path)?;
        Ok(final_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionix_crypto::IdentityKey;

    fn manifest(product: Product, version: &str) -> Manifest {
        Manifest {
            product,
            version: version.parse().unwrap(),
            published_at: "2026-10-10T00:00:00Z".into(),
            notes: vec!["teste".into()],
            installer: Installer {
                url: "https://example.test/setup.exe".into(),
                size: 3,
                sha256: hex::encode(Sha256::digest(b"abc")),
            },
        }
    }

    #[test]
    fn version_parsing_and_ordering() {
        let v: Version = "1.2.10".parse().unwrap();
        assert_eq!(v, Version(1, 2, 10));
        assert_eq!("v0.2.0".parse::<Version>().unwrap(), Version(0, 2, 0));
        assert!("1.2".parse::<Version>().is_err());
        assert!("1.2.3.4".parse::<Version>().is_err());
        assert!(is_newer(Version(0, 2, 0), Version(0, 2, 1)));
        assert!(is_newer(Version(0, 9, 9), Version(0, 10, 0)));
        assert!(
            !is_newer(Version(0, 2, 1), Version(0, 2, 1)),
            "same version is not an update"
        );
        assert!(
            !is_newer(Version(0, 2, 1), Version(0, 2, 0)),
            "downgrade is refused"
        );
    }

    #[test]
    fn signed_manifest_verifies() {
        let key = IdentityKey::generate();
        let bytes = serde_json::to_vec(&manifest(Product::Host, "0.3.0")).unwrap();
        let sig = sign_manifest(&key, &bytes);
        let m = verify_manifest(&bytes, &sig, &[key.public()], Product::Host).unwrap();
        assert_eq!(m.version, Version(0, 3, 0));
    }

    #[test]
    fn any_trusted_key_is_accepted() {
        let old = IdentityKey::generate();
        let new = IdentityKey::generate();
        let bytes = serde_json::to_vec(&manifest(Product::Host, "0.3.0")).unwrap();
        let sig = sign_manifest(&new, &bytes);
        assert!(
            verify_manifest(&bytes, &sig, &[old.public(), new.public()], Product::Host).is_ok()
        );
    }

    #[test]
    fn tampered_manifest_is_rejected() {
        let key = IdentityKey::generate();
        let bytes = serde_json::to_vec(&manifest(Product::Host, "0.3.0")).unwrap();
        let sig = sign_manifest(&key, &bytes);
        let tampered = String::from_utf8(bytes).unwrap().replace(
            "https://example.test/setup.exe",
            "https://evil.test/setup.exe",
        );
        assert!(matches!(
            verify_manifest(tampered.as_bytes(), &sig, &[key.public()], Product::Host),
            Err(UpdateError::BadSignature)
        ));
    }

    #[test]
    fn wrong_key_is_rejected() {
        let signer = IdentityKey::generate();
        let trusted = IdentityKey::generate();
        let bytes = serde_json::to_vec(&manifest(Product::Host, "0.3.0")).unwrap();
        let sig = sign_manifest(&signer, &bytes);
        assert!(matches!(
            verify_manifest(&bytes, &sig, &[trusted.public()], Product::Host),
            Err(UpdateError::BadSignature)
        ));
        assert!(matches!(
            verify_manifest(&bytes, &sig, &[], Product::Host),
            Err(UpdateError::NoTrustedKeys)
        ));
    }

    #[test]
    fn signature_from_another_domain_is_rejected() {
        let key = IdentityKey::generate();
        let bytes = serde_json::to_vec(&manifest(Product::Host, "0.3.0")).unwrap();
        let sig = aionix_crypto::sign_domain(&key, SignatureDomain::DeviceLogin, &bytes);
        assert!(verify_manifest(&bytes, &sig, &[key.public()], Product::Host).is_err());
    }

    #[test]
    fn other_product_and_insecure_url_are_rejected() {
        let key = IdentityKey::generate();
        let bytes = serde_json::to_vec(&manifest(Product::Technician, "0.3.0")).unwrap();
        let sig = sign_manifest(&key, &bytes);
        assert!(verify_manifest(&bytes, &sig, &[key.public()], Product::Host).is_err());

        let mut m = manifest(Product::Host, "0.3.0");
        m.installer.url = "http://example.test/setup.exe".into();
        let bytes = serde_json::to_vec(&m).unwrap();
        let sig = sign_manifest(&key, &bytes);
        assert!(verify_manifest(&bytes, &sig, &[key.public()], Product::Host).is_err());
    }

    #[test]
    fn sha256_of_file() {
        let dir = std::env::temp_dir().join(format!("aionix-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.bin");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            hex::encode(Sha256::digest(b"abc"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
