use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

/// Human-facing nine digit device identifier, e.g. `482 193 771`.
///
/// It is a lookup handle only. It is **not** a credential: authorization is always
/// based on cryptographic device identity and control-plane issued tickets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FriendlyId(u32);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FriendlyIdError {
    #[error("friendly id must have exactly {} digits", FriendlyId::DIGITS)]
    InvalidLength,
    #[error("friendly id contains invalid characters")]
    InvalidCharacter,
    #[error("friendly id cannot start with zero")]
    LeadingZero,
}

impl FriendlyId {
    pub const DIGITS: usize = 9;
    pub const MIN: u32 = 100_000_000;
    pub const MAX: u32 = 999_999_999;

    pub fn new(value: u32) -> Result<Self, FriendlyIdError> {
        if value < Self::MIN {
            return Err(FriendlyIdError::LeadingZero);
        }
        if value > Self::MAX {
            return Err(FriendlyIdError::InvalidLength);
        }
        Ok(Self(value))
    }

    /// Generates a uniformly random id. Uniqueness is enforced by the control plane.
    pub fn random() -> Self {
        Self(rand::random_range(Self::MIN..=Self::MAX))
    }

    pub fn value(self) -> u32 {
        self.0
    }

    /// Compact digits without separators, used in URLs and storage.
    pub fn compact(self) -> String {
        self.0.to_string()
    }
}

impl fmt::Display for FriendlyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.to_string();
        write!(f, "{} {} {}", &s[0..3], &s[3..6], &s[6..9])
    }
}

impl FromStr for FriendlyId {
    type Err = FriendlyIdError;

    /// Accepts digits optionally separated by spaces, dashes or dots (`482-193-771`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut digits = String::with_capacity(Self::DIGITS);
        for c in s.trim().chars() {
            match c {
                '0'..='9' => digits.push(c),
                ' ' | '-' | '.' | '\u{a0}' => {}
                _ => return Err(FriendlyIdError::InvalidCharacter),
            }
        }
        if digits.len() != Self::DIGITS {
            return Err(FriendlyIdError::InvalidLength);
        }
        if digits.starts_with('0') {
            return Err(FriendlyIdError::LeadingZero);
        }
        let value: u32 = digits.parse().map_err(|_| FriendlyIdError::InvalidCharacter)?;
        Self::new(value)
    }
}

impl Serialize for FriendlyId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.compact())
    }
}

impl<'de> Deserialize<'de> for FriendlyId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_with_groups() {
        assert_eq!(FriendlyId::new(482_193_771).unwrap().to_string(), "482 193 771");
    }

    #[test]
    fn parses_human_variants() {
        for s in ["482193771", "482 193 771", " 482-193-771 ", "482.193.771"] {
            assert_eq!(s.parse::<FriendlyId>().unwrap().value(), 482_193_771);
        }
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!("48219377".parse::<FriendlyId>(), Err(FriendlyIdError::InvalidLength));
        assert_eq!("4821937710".parse::<FriendlyId>(), Err(FriendlyIdError::InvalidLength));
        assert_eq!("082193771".parse::<FriendlyId>(), Err(FriendlyIdError::LeadingZero));
        assert_eq!("48219377a".parse::<FriendlyId>(), Err(FriendlyIdError::InvalidCharacter));
    }

    #[test]
    fn random_is_in_range() {
        for _ in 0..1000 {
            let id = FriendlyId::random();
            assert_eq!(id.compact().len(), FriendlyId::DIGITS);
        }
    }

    #[test]
    fn serde_roundtrip_uses_compact_string() {
        let id = FriendlyId::new(123_456_789).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"123456789\"");
        assert_eq!(serde_json::from_str::<FriendlyId>(&json).unwrap(), id);
    }
}
