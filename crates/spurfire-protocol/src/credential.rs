//! One-use client join credentials with secret-safe diagnostics.

use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize};
use zeroize::Zeroizing;

use crate::UnixMillis;

/// Placeholder returned instead of key material by a dry-run join.
pub const DRY_RUN_AUTH_KEY: &str = "DRY_RUN_NO_KEY";

pub(crate) struct DeserializedSecret(Option<Zeroizing<String>>);

impl DeserializedSecret {
    pub(crate) fn into_zeroizing(mut self) -> Zeroizing<String> {
        self.0.take().expect("deserialized secret is present")
    }
}

impl<'de> Deserialize<'de> for DeserializedSecret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SecretVisitor;

        impl de::Visitor<'_> for SecretVisitor {
            type Value = DeserializedSecret;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a secret string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(DeserializedSecret(Some(Zeroizing::new(value.to_owned()))))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(DeserializedSecret(Some(Zeroizing::new(value))))
            }
        }

        deserializer.deserialize_string(SecretVisitor)
    }
}

/// One-use, ephemeral, preauthorized credential delivered only by a join response.
///
/// This type intentionally does **not** implement [`Serialize`]. The protocol's
/// explicit `JoinLobbyResponse` serializer is the only built-in wire path that
/// reveals `auth_key`; normal snapshots and debug formatting cannot emit it.
#[derive(PartialEq, Eq)]
pub struct JoinCredential {
    /// Stable receipt ID used for idempotent replays.
    pub credential_id: String,
    auth_key: Zeroizing<String>,
    /// Tailnet the embedded client should join.
    pub tailnet: String,
    /// Lobby-confined ownership tags.
    pub tags: Vec<String>,
    /// Absolute credential expiry.
    pub expires_at: UnixMillis,
}

impl JoinCredential {
    /// Creates a credential. One-use semantics are invariant and cannot be disabled.
    #[must_use]
    pub fn new(
        credential_id: impl Into<String>,
        auth_key: Zeroizing<String>,
        tailnet: impl Into<String>,
        tags: Vec<String>,
        expires_at: UnixMillis,
    ) -> Self {
        Self {
            credential_id: credential_id.into(),
            auth_key,
            tailnet: tailnet.into(),
            tags,
            expires_at,
        }
    }

    /// Explicitly exposes key material to the client enrollment call.
    ///
    /// Callers must not log or persist the returned value.
    #[must_use]
    pub fn expose_auth_key(&self) -> &str {
        &self.auth_key
    }

    /// Join credentials are always exactly one-use.
    #[must_use]
    pub const fn is_one_use(&self) -> bool {
        true
    }

    pub(crate) fn as_wire(&self) -> JoinCredentialWire<'_> {
        JoinCredentialWire {
            credential_id: &self.credential_id,
            auth_key: &self.auth_key,
            tailnet: &self.tailnet,
            tags: &self.tags,
            expires_at: self.expires_at,
            one_use: true,
        }
    }
}

impl fmt::Debug for JoinCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JoinCredential")
            .field("credential_id", &self.credential_id)
            .field("auth_key", &Redacted)
            .field("tailnet", &self.tailnet)
            .field("tags", &self.tags)
            .field("expires_at", &self.expires_at)
            .field("one_use", &true)
            .finish()
    }
}

struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Serialize)]
pub(crate) struct JoinCredentialWire<'a> {
    credential_id: &'a str,
    auth_key: &'a str,
    tailnet: &'a str,
    tags: &'a [String],
    expires_at: UnixMillis,
    one_use: bool,
}

#[derive(Deserialize)]
struct OwnedJoinCredentialWire {
    credential_id: String,
    auth_key: DeserializedSecret,
    tailnet: String,
    tags: Vec<String>,
    expires_at: UnixMillis,
    one_use: bool,
}

impl<'de> Deserialize<'de> for JoinCredential {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = OwnedJoinCredentialWire::deserialize(deserializer)?;
        if !wire.one_use {
            return Err(de::Error::custom("join credential must be one-use"));
        }
        Ok(Self::new(
            wire.credential_id,
            wire.auth_key.into_zeroizing(),
            wire.tailnet,
            wire.tags,
            wire.expires_at,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_contains_auth_key() {
        let credential = JoinCredential::new(
            "credential-1",
            Zeroizing::new("synthetic-auth-key-super-secret-canary".into()),
            "example.ts.net",
            vec!["tag:spurfire-lobby-example".into()],
            UnixMillis::new(100),
        );
        let debug = format!("{credential:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("super-secret-canary"));
        assert!(!debug.contains(credential.expose_auth_key()));
        assert!(credential.is_one_use());
    }
}
