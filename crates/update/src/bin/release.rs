//! Release tooling for the desktop apps (used by infra/release.ps1).
//!
//!   aionix-release keygen   --key <file>          new release signing key (never overwrites)
//!   aionix-release pubkey   --key <file>          prints the public key (branding/release-keys.txt)
//!   aionix-release manifest --key <file> --product host --version 0.2.0
//!                           --installer <setup.exe> --url <https url> [--note "..."]...
//!                           --out <dir>           writes latest.json + latest.json.sig
//!   aionix-release verify   --product host --manifest <latest.json> [--installer <file>]
//!                                                 checks against the compiled-in keys

use std::path::PathBuf;

use aionix_crypto::IdentityKey;
use aionix_update::{Installer, Manifest, Product, Version};
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Aionix Connect desktop release tooling")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Keygen {
        #[arg(long)]
        key: PathBuf,
    },
    Pubkey {
        #[arg(long)]
        key: PathBuf,
    },
    Manifest {
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        product: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        installer: PathBuf,
        #[arg(long)]
        url: String,
        #[arg(long = "note")]
        notes: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
    Verify {
        #[arg(long)]
        product: String,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        installer: Option<PathBuf>,
    },
}

fn load_key(path: &PathBuf) -> Result<IdentityKey> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("chave de release não encontrada: {}", path.display()))?;
    let bytes = B64
        .decode(text.trim())
        .context("chave de release corrompida")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("chave de release corrompida"))?;
    Ok(IdentityKey::from_secret_bytes(&arr))
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Keygen { key } => {
            if key.exists() {
                bail!(
                    "{} já existe; não sobrescrevo uma chave de release",
                    key.display()
                );
            }
            if let Some(dir) = key.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let k = IdentityKey::generate();
            std::fs::write(&key, B64.encode(&k.secret_bytes()[..]))?;
            println!("{}", k.public().to_base64());
        }
        Cmd::Pubkey { key } => println!("{}", load_key(&key)?.public().to_base64()),
        Cmd::Manifest {
            key,
            product,
            version,
            installer,
            url,
            notes,
            out,
        } => {
            let key = load_key(&key)?;
            let product: Product = product.parse()?;
            let version: Version = version.parse()?;
            if !url.starts_with("https://") {
                bail!("a URL do instalador precisa ser https");
            }
            let size = std::fs::metadata(&installer)
                .with_context(|| format!("instalador não encontrado: {}", installer.display()))?
                .len();
            let sha256 = aionix_update::sha256_file(&installer)?;
            let published_at = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)?;
            let m = Manifest {
                product,
                version,
                published_at,
                notes,
                installer: Installer { url, size, sha256 },
            };
            let bytes = serde_json::to_vec_pretty(&m)?;
            let sig = aionix_update::sign_manifest(&key, &bytes);
            // Self-check with the key that signed it before anything is published.
            aionix_update::verify_manifest(&bytes, &sig, &[key.public()], product)?;
            std::fs::create_dir_all(&out)?;
            std::fs::write(out.join("latest.json"), &bytes)?;
            std::fs::write(out.join("latest.json.sig"), &sig)?;
            println!("{} {} -> {}", product.slug(), version, out.display());
        }
        Cmd::Verify {
            product,
            manifest,
            installer,
        } => {
            let product: Product = product.parse()?;
            let bytes = std::fs::read(&manifest)?;
            let sig = std::fs::read_to_string(manifest.with_extension("json.sig"))?;
            let m = aionix_update::verify_manifest(
                &bytes,
                &sig,
                &aionix_update::trusted_keys(),
                product,
            )?;
            if let Some(file) = installer {
                let size = std::fs::metadata(&file)?.len();
                if size != m.installer.size
                    || aionix_update::sha256_file(&file)? != m.installer.sha256
                {
                    bail!("o instalador não confere com o manifesto assinado");
                }
            }
            println!(
                "OK {} {} ({} bytes)",
                product.slug(),
                m.version,
                m.installer.size
            );
        }
    }
    Ok(())
}
