//! Wire protocol versioning.

use std::{fmt, str::FromStr};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Current Spurfire wire version.
pub const WIRE_VERSION: WireVersion = WireVersion::new(1, 1);

/// Alias that reads naturally at call sites comparing against the local build.
pub const CURRENT_WIRE_VERSION: WireVersion = WIRE_VERSION;

/// A `MAJOR.MINOR` protocol version.
///
/// Major versions must match. Minor versions may differ because additive fields
/// are ignored by serde unless a DTO explicitly opts into stricter validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireVersion {
    major: u16,
    minor: u16,
}

impl WireVersion {
    /// Builds a wire version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// The compatibility-breaking component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// The additive compatibility component.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Classifies compatibility with `other`.
    #[must_use]
    pub const fn compatibility_with(self, other: Self) -> WireCompatibility {
        if self.major == other.major {
            WireCompatibility::Compatible
        } else {
            WireCompatibility::MajorMismatch
        }
    }

    /// Returns whether the two versions may communicate.
    #[must_use]
    pub const fn is_compatible_with(self, other: Self) -> bool {
        matches!(
            self.compatibility_with(other),
            WireCompatibility::Compatible
        )
    }

    /// Returns an error for a major-version mismatch.
    pub const fn check_compatible_with(self, other: Self) -> Result<(), WireVersionMismatch> {
        if self.is_compatible_with(other) {
            Ok(())
        } else {
            Err(WireVersionMismatch {
                local: self,
                remote: other,
            })
        }
    }
}

/// Checks two protocol versions using Spurfire's major-compatible policy.
#[must_use]
pub const fn is_wire_compatible(local: WireVersion, remote: WireVersion) -> bool {
    local.is_compatible_with(remote)
}

/// Result of comparing two wire versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireCompatibility {
    /// Major versions match; minor differences are additive and accepted.
    Compatible,
    /// Major versions differ and communication must be rejected.
    MajorMismatch,
}

/// A major wire-version mismatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("wire version incompatible: local {local}, remote {remote}")]
pub struct WireVersionMismatch {
    /// Version supported by the validating process.
    pub local: WireVersion,
    /// Version supplied by the remote peer.
    pub remote: WireVersion,
}

/// Error parsing a canonical `MAJOR.MINOR` wire version.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WireVersionParseError {
    /// The string was not exactly two decimal components separated by one dot.
    #[error("wire version must use MAJOR.MINOR form")]
    InvalidFormat,
    /// A component was empty, non-decimal, non-canonical, or larger than `u16`.
    #[error("wire version contains an invalid {component} component")]
    InvalidComponent {
        /// Component name (`major` or `minor`).
        component: &'static str,
    },
}

impl fmt::Display for WireVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

impl FromStr for WireVersion {
    type Err = WireVersionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (major, minor) = value
            .split_once('.')
            .ok_or(WireVersionParseError::InvalidFormat)?;
        if minor.contains('.') {
            return Err(WireVersionParseError::InvalidFormat);
        }

        fn parse_component(
            value: &str,
            component: &'static str,
        ) -> Result<u16, WireVersionParseError> {
            if value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
                || (value.len() > 1 && value.starts_with('0'))
            {
                return Err(WireVersionParseError::InvalidComponent { component });
            }
            value
                .parse()
                .map_err(|_| WireVersionParseError::InvalidComponent { component })
        }

        Ok(Self::new(
            parse_component(major, "major")?,
            parse_component(minor, "minor")?,
        ))
    }
}

impl Serialize for WireVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WireVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VersionVisitor;

        impl de::Visitor<'_> for VersionVisitor {
            type Value = WireVersion;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a canonical MAJOR.MINOR wire version")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(VersionVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn major_must_match_but_minor_may_differ() {
        assert!(WIRE_VERSION.is_compatible_with(WireVersion::new(1, 99)));
        assert!(!WIRE_VERSION.is_compatible_with(WireVersion::new(2, 0)));
    }

    #[test]
    fn wire_form_is_canonical_string() {
        let version = WireVersion::new(12, 34);
        assert_eq!(serde_json::to_string(&version).unwrap(), r#""12.34""#);
        assert_eq!(
            serde_json::from_str::<WireVersion>(r#""12.34""#).unwrap(),
            version
        );
        assert!("01.0".parse::<WireVersion>().is_err());
        assert!("1.0.0".parse::<WireVersion>().is_err());
    }
}
