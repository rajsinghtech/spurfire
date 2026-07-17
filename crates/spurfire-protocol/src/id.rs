//! Validated lobby and player identifiers.

use std::{fmt, str::FromStr};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Why a lobby or player identifier was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum IdValidationError {
    /// A canonical UUID has exactly 36 ASCII characters.
    #[error("identifier must be a hyphenated UUID")]
    InvalidLength,
    /// Hyphens were not at UUID positions 8, 13, 18, and 23.
    #[error("identifier must use canonical UUID hyphen placement")]
    InvalidHyphens,
    /// A non-hexadecimal character occurred.
    #[error("identifier contains a non-hexadecimal character")]
    InvalidHex,
    /// Spurfire protocol identifiers are UUID version 4.
    #[error("identifier must be a UUIDv4")]
    NotVersion4,
    /// The RFC 4122 variant bits were not `10`.
    #[error("identifier must use the RFC 4122 UUID variant")]
    InvalidVariant,
}

fn decode_uuid_v4(value: &str) -> Result<[u8; 16], IdValidationError> {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return Err(IdValidationError::InvalidLength);
    }
    for index in [8, 13, 18, 23] {
        if bytes[index] != b'-' {
            return Err(IdValidationError::InvalidHyphens);
        }
    }

    fn nibble(byte: u8) -> Result<u8, IdValidationError> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(IdValidationError::InvalidHex),
        }
    }

    let mut decoded = [0_u8; 16];
    let mut source = 0;
    let mut target = 0;
    while source < bytes.len() {
        if matches!(source, 8 | 13 | 18 | 23) {
            source += 1;
            continue;
        }
        let high = nibble(bytes[source])?;
        let low = nibble(bytes[source + 1])?;
        decoded[target] = (high << 4) | low;
        source += 2;
        target += 1;
    }

    if decoded[6] >> 4 != 4 {
        return Err(IdValidationError::NotVersion4);
    }
    if decoded[8] >> 6 != 2 {
        return Err(IdValidationError::InvalidVariant);
    }
    Ok(decoded)
}

fn format_uuid(bytes: &[u8; 16], formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            formatter.write_str("-")?;
        }
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}

macro_rules! protocol_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Parses and validates a UUIDv4 identifier.
            pub fn parse(value: &str) -> Result<Self, IdValidationError> {
                decode_uuid_v4(value).map(Self)
            }

            /// Creates an identifier from RFC 4122 UUID bytes.
            pub fn from_bytes(bytes: [u8; 16]) -> Result<Self, IdValidationError> {
                if bytes[6] >> 4 != 4 {
                    return Err(IdValidationError::NotVersion4);
                }
                if bytes[8] >> 6 != 2 {
                    return Err(IdValidationError::InvalidVariant);
                }
                Ok(Self(bytes))
            }

            /// Returns the UUID bytes used for deterministic ordering.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// Consumes the identifier and returns its UUID bytes.
            #[must_use]
            pub const fn into_bytes(self) -> [u8; 16] {
                self.0
            }

            /// Returns the canonical lowercase, hyphenated representation.
            #[must_use]
            pub fn to_canonical_string(self) -> String {
                self.to_string()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                format_uuid(&self.0, formatter)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.to_string())
                    .finish()
            }
        }

        impl FromStr for $name {
            type Err = IdValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdValidationError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(&value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct IdVisitor;

                impl de::Visitor<'_> for IdVisitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str("a canonical UUIDv4 string")
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        $name::parse(value).map_err(E::custom)
                    }
                }

                deserializer.deserialize_str(IdVisitor)
            }
        }
    };
}

protocol_id!(
    LobbyId,
    "A strongly typed, canonical UUIDv4 identifying a lobby."
);
protocol_id!(
    PlayerId,
    "A strongly typed, canonical UUIDv4 identifying a player."
);

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "123e4567-e89b-42d3-a456-426614174000";

    #[test]
    fn ids_validate_version_variant_and_shape() {
        assert_eq!(PlayerId::parse(VALID).unwrap().to_string(), VALID);
        assert_eq!(
            PlayerId::parse("123e4567-e89b-12d3-a456-426614174000"),
            Err(IdValidationError::NotVersion4)
        );
        assert_eq!(
            PlayerId::parse("123e4567-e89b-42d3-7456-426614174000"),
            Err(IdValidationError::InvalidVariant)
        );
        assert!(PlayerId::parse("not-a-uuid").is_err());
        assert!(PlayerId::parse("123e4567e89b42d3a456426614174000").is_err());
    }

    #[test]
    fn representation_is_canonical_and_typed() {
        let id = LobbyId::parse("123E4567-E89B-42D3-A456-426614174000").unwrap();
        assert_eq!(id.to_string(), VALID);
        assert_eq!(serde_json::to_string(&id).unwrap(), format!(r#""{VALID}""#));
        assert_eq!(
            serde_json::from_str::<LobbyId>(&format!(r#""{VALID}""#)).unwrap(),
            id
        );
    }
}
