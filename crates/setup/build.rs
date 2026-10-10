//! Embeds the product payload and the Windows resources of the setup.
//!
//! Release builds set (infra/release.ps1):
//!   AIONIX_SETUP_PAYLOAD  path of the already-built app executable
//! Without them (`cargo build --workspace`, clippy) the setup still compiles with an
//! empty payload and refuses to install.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=AIONIX_SETUP_PAYLOAD");
    let out = std::env::var("OUT_DIR").unwrap();
    let payload_out = Path::new(&out).join("payload.bin");
    match std::env::var("AIONIX_SETUP_PAYLOAD") {
        Ok(p) if !p.is_empty() => {
            println!("cargo:rerun-if-changed={p}");
            std::fs::copy(&p, &payload_out).unwrap_or_else(|e| panic!("payload {p}: {e}"));
        }
        _ => std::fs::write(&payload_out, b"").unwrap(),
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../branding");
    let brand: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("brand.json")).unwrap()).unwrap();
    let name = brand["productName"].as_str().unwrap();
    let company = brand["companyName"].as_str().unwrap();
    let app = name.to_owned();
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let icon = root.join("icons").join("setup.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let nums: Vec<u16> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let (a, b, c) = (nums[0], nums[1], nums[2]);
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\"\"");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "041604B0"
    BEGIN
      VALUE "CompanyName", "{company}"
      VALUE "FileDescription", "Instalador do {app}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "aionix-setup"
      VALUE "OriginalFilename", "aionix-setup.exe"
      VALUE "ProductName", "{app}"
      VALUE "ProductVersion", "{version}"
      VALUE "LegalCopyright", "© {company}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x416, 1200
  END
END
"#,
        icon = esc(&icon.display().to_string()),
        company = esc(company),
        app = esc(&app),
    );
    let rc_path = Path::new(&out).join("setup.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
