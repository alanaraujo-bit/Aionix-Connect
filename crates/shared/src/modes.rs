use serde::{Deserialize, Serialize};

/// What kind of software a registered device identity belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Permanently installed agent (Windows service + session host).
    Agent,
    /// Portable, temporary support executable.
    QuickSupport,
    /// A technician workstation running the Technician app.
    Technician,
}

impl DeviceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::QuickSupport => "quick_support",
            Self::Technician => "technician",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "quick_support" => Some(Self::QuickSupport),
            "technician" => Some(Self::Technician),
            _ => None,
        }
    }

    /// Whether this kind of device can host (be controlled in) a remote session.
    pub fn can_host(self) -> bool {
        matches!(self, Self::Agent | Self::QuickSupport)
    }
}

/// How the host authorizes an incoming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    /// The local user must explicitly approve (or the technician must know the
    /// temporary password shown on screen).
    Assisted,
    /// No local confirmation. Only valid when unattended access was deliberately
    /// enabled on the device *and* the technician holds an organization grant.
    Unattended,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assisted => "assisted",
            Self::Unattended => "unattended",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "assisted" => Some(Self::Assisted),
            "unattended" => Some(Self::Unattended),
            _ => None,
        }
    }
}

/// Network path currently carrying a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Direct path inside the same local network.
    Lan,
    /// Direct path across the internet (hole punched).
    Direct,
    /// Traffic forwarded (still end-to-end encrypted) by a relay server.
    Relay,
    /// Not yet determined.
    Unknown,
}

/// User-selectable image quality policy. `Automatic` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityMode {
    #[default]
    Automatic,
    PreferSpeed,
    Balanced,
    PreferQuality,
    Original,
}
