//! The owner's choices for "Este computador", kept next to the identity (ADR-0015).
//!
//! Every choice that lets someone connect without the owner clicking "Permitir" lives
//! here, and only the owner changes it, in this computer's own window. Missing or
//! unreadable settings are the safe defaults: nothing is allowed.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

const FILE: &str = "host-settings.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSettings {
    /// The owner allows the organization's technicians to connect with the device
    /// password, without clicking "Permitir". Off by default.
    #[serde(default)]
    pub unattended_access: bool,
}

pub fn load(dir: &Path) -> HostSettings {
    fs::read(dir.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes through a temporary file, so an interrupted save never leaves half a file.
pub fn save(dir: &Path, settings: &HostSettings) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(settings)?)?;
    fs::rename(&tmp, dir.join(FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aionix-host-settings-{name}"));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn missing_settings_allow_nothing() {
        let dir = scratch("missing");
        assert_eq!(load(&dir), HostSettings::default());
        assert!(!load(&dir).unattended_access);
    }

    #[test]
    fn corrupt_settings_allow_nothing() {
        let dir = scratch("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE), b"{ not json").unwrap();
        assert_eq!(load(&dir), HostSettings::default());
    }

    #[test]
    fn saved_settings_round_trip() {
        let dir = scratch("roundtrip");
        let on = HostSettings {
            unattended_access: true,
        };
        save(&dir, &on).unwrap();
        assert_eq!(load(&dir), on);
        save(&dir, &HostSettings::default()).unwrap();
        assert!(!load(&dir).unattended_access);
        let _ = fs::remove_dir_all(&dir);
    }
}
