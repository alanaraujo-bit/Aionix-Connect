//! Operating-system integration used by hosts (Agent, Quick Support) and the
//! Technician app. Windows is implemented; other platforms report `Unsupported`.

#[cfg(windows)]
pub mod windows;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("not supported on this platform")]
    Unsupported,
    #[error("access denied")]
    AccessDenied,
    #[error("{0}")]
    Os(String),
}

#[cfg(windows)]
impl From<::windows::core::Error> for PlatformError {
    fn from(e: ::windows::core::Error) -> Self {
        PlatformError::Os(format!("{} ({:#x})", e.message(), e.code().0))
    }
}

/// Basic facts about the machine, shown to technicians and stored in the inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemInfo {
    pub hostname: String,
    pub os_name: String,
    pub os_version: String,
    pub cpu: String,
    pub memory_total_bytes: u64,
    pub username: String,
}

/// Where a component keeps its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataScope {
    /// Per-user data (Technician app, Quick Support).
    User,
    /// Machine-wide data readable only by administrators/SYSTEM (Agent).
    Machine,
}

/// Returns (and creates) the data directory for `component` under the branded root,
/// e.g. `%LOCALAPPDATA%\Aionix Connect\technician`.
pub fn data_dir(scope: DataScope, component: &str) -> Result<PathBuf, PlatformError> {
    let base = match scope {
        DataScope::User => std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("XDG_DATA_HOME"))
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))),
        DataScope::Machine => std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from("/var/lib"))),
    }
    .ok_or_else(|| PlatformError::Os("no data directory".into()))?;
    let dir = base.join(&aionix_brand::brand().identifiers.data_dir_name).join(component);
    std::fs::create_dir_all(&dir).map_err(|e| PlatformError::Os(e.to_string()))?;
    Ok(dir)
}
