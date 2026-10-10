//! Windows resources for the desktop executables: icon (Explorer, taskbar, shortcuts)
//! and VERSIONINFO (Properties → Details, Apps & features) from the workspace version.
//! Also declares the release-time environment the apps compile in
//! (`AIONIX_DEFAULT_SERVER`, `AIONIX_UPDATE_BASE`).

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=AIONIX_DEFAULT_SERVER");
    println!("cargo:rerun-if-env-changed=AIONIX_UPDATE_BASE");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../branding");
    let brand: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("brand.json")).unwrap()).unwrap();
    let product = brand["productName"].as_str().unwrap().to_owned();
    let company = brand["companyName"].as_str().unwrap().to_owned();
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    for (bin, icon, description) in [
        ("aionix-connect", "setup.ico", product.clone()),
        (
            "aionix-launcher",
            "setup.ico",
            format!("{product} (desenvolvimento)"),
        ),
    ] {
        let icon_path = root.join("icons").join(icon);
        println!("cargo:rerun-if-changed={}", icon_path.display());
        let rc = rc_file(&icon_path, &version, &company, &product, &description, bin);
        let rc_path = Path::new(&out).join(format!("{bin}.rc"));
        std::fs::write(&rc_path, rc).unwrap();
        embed_resource::compile_for(&rc_path, [bin], embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
    println!("cargo:rerun-if-changed=../../branding/brand.json");
}

/// VERSIONINFO + icon; strings in pt-BR (0x0416, Unicode).
pub fn rc_file(
    icon: &Path,
    version: &str,
    company: &str,
    product: &str,
    description: &str,
    bin: &str,
) -> String {
    let nums: Vec<u16> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let (a, b, c) = (
        nums.first().copied().unwrap_or(0),
        nums.get(1).copied().unwrap_or(0),
        nums.get(2).copied().unwrap_or(0),
    );
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\"\"");
    format!(
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
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{bin}"
      VALUE "OriginalFilename", "{bin}.exe"
      VALUE "ProductName", "{product}"
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
        product = esc(product),
        description = esc(description),
    )
}
