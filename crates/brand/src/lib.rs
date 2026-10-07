//! Product identity.
//!
//! Every user-visible product name, company name and OS-level identifier comes from
//! `branding/brand.json`, embedded at compile time. Code must never hard-code the brand;
//! use [`brand()`] instead so a rename only touches that JSON file.

use serde::Deserialize;
use std::sync::OnceLock;

const BRAND_JSON: &str = include_str!("../../../branding/brand.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Brand {
    pub product_name: String,
    pub short_name: String,
    pub company_name: String,
    pub apps: Apps,
    pub identifiers: Identifiers,
    pub links: Links,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Apps {
    pub technician: String,
    pub agent: String,
    pub quick_support: String,
    pub server: String,
    pub admin: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identifiers {
    pub bundle_prefix: String,
    pub windows_service_name: String,
    pub windows_service_display_name: String,
    pub windows_service_description: String,
    pub data_dir_name: String,
    pub cli_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Links {
    pub website: String,
    pub support: String,
    pub privacy: String,
}

/// Returns the compiled-in product identity.
pub fn brand() -> &'static Brand {
    static BRAND: OnceLock<Brand> = OnceLock::new();
    BRAND.get_or_init(|| {
        serde_json::from_str(BRAND_JSON).expect("branding/brand.json is validated by tests")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_json_is_valid_and_complete() {
        let b = brand();
        assert!(!b.product_name.is_empty());
        assert!(!b.identifiers.windows_service_name.contains(' '));
        assert!(b.apps.technician.starts_with(&b.product_name));
    }
}
