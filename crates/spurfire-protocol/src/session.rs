//! Canonical, domain-separated session identity primitives.

use std::{fmt, net::IpAddr};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{LobbyId, PlayerId, WireVersion};

const ROSTER_DOMAIN: &[u8] = b"SPURFIRE-ROSTER\0v1\0";
const ENVELOPE_DOMAIN: &[u8] = b"SPURFIRE-ENV\0v1\0";
const KEYREG_DOMAIN: &[u8] = b"SPURFIRE-KEYREG\0v1\0";
const MANIFEST_DOMAIN: &[u8] = b"SPURFIRE-MANIFEST\0v1\0";

/// A per-session Ed25519 public key. Debug output is deliberately redacted.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionPublicKey([u8; 32]);

impl SessionPublicKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Strictly verifies a signature over one already domain-separated digest.
    pub fn verify_digest(
        self,
        digest: &[u8; 32],
        signature: SessionSignature,
    ) -> Result<(), SessionIdentityError> {
        let key = VerifyingKey::from_bytes(&self.0)
            .map_err(|_| SessionIdentityError::InvalidPublicKey)?;
        key.verify_strict(digest, &Signature::from_bytes(signature.as_bytes()))
            .map_err(|_| SessionIdentityError::BadSignature)
    }
}

impl fmt::Debug for SessionPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionPublicKey(<redacted>)")
    }
}

/// An Ed25519 signature. Debug output is deliberately redacted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionSignature([u8; 64]);

impl SessionSignature {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl fmt::Debug for SessionSignature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionSignature(<redacted>)")
    }
}

macro_rules! b64_serde {
    ($type:ty, $length:expr) => {
        impl Serialize for $type {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&URL_SAFE_NO_PAD.encode(self.as_bytes()))
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let encoded = String::deserialize(deserializer)?;
                let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(de::Error::custom)?;
                let bytes: [u8; $length] = decoded
                    .try_into()
                    .map_err(|_| de::Error::custom("invalid session identity byte length"))?;
                Ok(Self::from_bytes(bytes))
            }
        }
    };
}

b64_serde!(SessionPublicKey, 32);
b64_serde!(SessionSignature, 64);

/// Canonical lowercase SHA-256 roster hash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RosterHash([u8; 32]);

impl RosterHash {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for RosterHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RosterHash")
            .field(&self.to_string())
            .finish()
    }
}

impl fmt::Display for RosterHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for RosterHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RosterHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_hex_32(&value).map(Self).map_err(de::Error::custom)
    }
}

/// A normalized WireGuard node-key claim. It is channel metadata, never a signer.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeKey([u8; 32]);

impl NodeKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn parse(value: &str) -> Result<Self, SessionIdentityError> {
        let hex = value
            .strip_prefix("nodekey:")
            .ok_or(SessionIdentityError::InvalidNodeKey)?;
        parse_hex_32(hex)
            .map(Self)
            .map_err(|_| SessionIdentityError::InvalidNodeKey)
    }
}

impl fmt::Debug for NodeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NodeKey(<redacted>)")
    }
}

impl fmt::Display for NodeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("nodekey:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for NodeKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for NodeKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// The signed identity portion carried by a wire 1.2 gameplay envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBinding {
    pub network_generation: u64,
    pub session_generation: u64,
    pub roster_hash: RosterHash,
    pub signature: SessionSignature,
}

/// One exact endpoint and application identity in a signed roster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterManifestEntry {
    pub player_id: PlayerId,
    pub session_public_key: SessionPublicKey,
    pub tailnet_address: IpAddr,
    pub application_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_key: Option<NodeKey>,
}

/// The exact server-projected session roster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterManifest {
    pub lobby_id: LobbyId,
    pub network_generation: u64,
    pub session_generation: u64,
    pub roster_revision: u64,
    pub entries: Vec<RosterManifestEntry>,
}

impl RosterManifest {
    /// Validates sorting-independent uniqueness and endpoint shape.
    pub fn validate(&self) -> Result<(), SessionIdentityError> {
        if self.entries.is_empty() {
            return Err(SessionIdentityError::EmptyRoster);
        }
        let mut players = std::collections::BTreeSet::new();
        let mut ips = std::collections::BTreeSet::new();
        let mut nodes = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if entry.application_port == 0 {
                return Err(SessionIdentityError::InvalidEndpoint);
            }
            if !players.insert(entry.player_id)
                || !ips.insert(entry.tailnet_address)
                || entry.node_key.is_some_and(|node| !nodes.insert(node))
            {
                return Err(SessionIdentityError::DuplicateRosterIdentity);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn hash(&self) -> RosterHash {
        roster_hash(self)
    }
}

/// Canonical roster bytes. Rows are sorted by player ID.
pub fn canonical_roster_bytes(manifest: &RosterManifest) -> Vec<u8> {
    let mut rows: Vec<_> = manifest.entries.iter().collect();
    rows.sort_by_key(|entry| entry.player_id);
    let mut bytes = Vec::with_capacity(ROSTER_DOMAIN.len() + rows.len() * 100);
    bytes.extend_from_slice(ROSTER_DOMAIN);
    bytes.extend_from_slice(&manifest.network_generation.to_be_bytes());
    bytes.extend_from_slice(&manifest.session_generation.to_be_bytes());
    bytes.extend_from_slice(&manifest.roster_revision.to_be_bytes());
    bytes.extend_from_slice(&u32::try_from(rows.len()).unwrap_or(u32::MAX).to_be_bytes());
    for entry in rows {
        bytes.extend_from_slice(entry.player_id.as_bytes());
        bytes.extend_from_slice(entry.session_public_key.as_bytes());
        match entry.tailnet_address {
            IpAddr::V4(ip) => bytes.extend_from_slice(&ip.octets()),
            IpAddr::V6(ip) => bytes.extend_from_slice(&ip.octets()),
        }
        bytes.extend_from_slice(&entry.application_port.to_be_bytes());
        match &entry.node_key {
            Some(key) => bytes.extend_from_slice(key.as_bytes()),
            None => bytes.extend_from_slice(&[0; 32]),
        }
    }
    bytes
}

#[must_use]
pub fn roster_hash(manifest: &RosterManifest) -> RosterHash {
    RosterHash(sha256(&canonical_roster_bytes(manifest)))
}

/// Canonical digest signed by a gameplay sender. Payload bytes must use the
/// fixed variant layout owned by the peer wire crate, never JSON.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn canonical_envelope_digest(
    wire_version: WireVersion,
    lobby_id: LobbyId,
    network_generation: u64,
    session_generation: u64,
    roster_hash: RosterHash,
    sender: PlayerId,
    authority_epoch: u64,
    sequence: u64,
    simulation_tick: u64,
    canonical_payload: &[u8],
) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(160 + canonical_payload.len());
    bytes.extend_from_slice(ENVELOPE_DOMAIN);
    bytes.extend_from_slice(&wire_version.major().to_be_bytes());
    bytes.extend_from_slice(&wire_version.minor().to_be_bytes());
    bytes.extend_from_slice(lobby_id.as_bytes());
    bytes.extend_from_slice(&network_generation.to_be_bytes());
    bytes.extend_from_slice(&session_generation.to_be_bytes());
    bytes.extend_from_slice(roster_hash.as_bytes());
    bytes.extend_from_slice(sender.as_bytes());
    bytes.extend_from_slice(&authority_epoch.to_be_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(&simulation_tick.to_be_bytes());
    bytes.extend_from_slice(
        &u32::try_from(canonical_payload.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    bytes.extend_from_slice(canonical_payload);
    sha256(&bytes)
}

/// Self-challenge digest proving possession during capability-bound registration.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn canonical_keyreg_digest(
    lobby_id: LobbyId,
    player_id: PlayerId,
    network_generation: u64,
    roster_revision: u64,
    tailnet_address: IpAddr,
    application_port: u16,
    session_public_key: SessionPublicKey,
) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(128);
    bytes.extend_from_slice(KEYREG_DOMAIN);
    bytes.extend_from_slice(lobby_id.as_bytes());
    bytes.extend_from_slice(player_id.as_bytes());
    bytes.extend_from_slice(&network_generation.to_be_bytes());
    bytes.extend_from_slice(&roster_revision.to_be_bytes());
    match tailnet_address {
        IpAddr::V4(ip) => bytes.extend_from_slice(&ip.octets()),
        IpAddr::V6(ip) => bytes.extend_from_slice(&ip.octets()),
    }
    bytes.extend_from_slice(&application_port.to_be_bytes());
    bytes.extend_from_slice(session_public_key.as_bytes());
    sha256(&bytes)
}

/// Digest signed by the server's memory-only lobby manifest key.
#[must_use]
pub fn canonical_manifest_digest(
    manifest_public_key: SessionPublicKey,
    manifest: &RosterManifest,
) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(64 + manifest.entries.len() * 100);
    bytes.extend_from_slice(MANIFEST_DOMAIN);
    bytes.extend_from_slice(manifest_public_key.as_bytes());
    bytes.extend_from_slice(&canonical_roster_bytes(manifest));
    sha256(&bytes)
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SessionIdentityError {
    #[error("invalid Ed25519 public key")]
    InvalidPublicKey,
    #[error("signature verification failed")]
    BadSignature,
    #[error("node key must use nodekey:<64 lowercase hex> form")]
    InvalidNodeKey,
    #[error("roster must not be empty")]
    EmptyRoster,
    #[error("roster contains duplicate player, IP, or node identity")]
    DuplicateRosterIdentity,
    #[error("roster contains an invalid endpoint")]
    InvalidEndpoint,
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn parse_hex_32(value: &str) -> Result<[u8; 32], &'static str> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("expected 64 lowercase hexadecimal characters");
    }
    let mut out = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let nibble = |byte: u8| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => unreachable!(),
        };
        out[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn lobby() -> LobbyId {
        LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap()
    }
    fn player(last: u8) -> PlayerId {
        PlayerId::parse(&format!("00000000-0000-4000-8000-{last:012x}")).unwrap()
    }

    #[test]
    fn canonical_roster_is_sorted_and_manifest_signature_is_strict() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let public = SessionPublicKey::from_bytes(signing.verifying_key().to_bytes());
        let manifest = RosterManifest {
            lobby_id: lobby(),
            network_generation: 3,
            session_generation: 4,
            roster_revision: 5,
            entries: vec![
                RosterManifestEntry {
                    player_id: player(2),
                    session_public_key: SessionPublicKey::from_bytes([2; 32]),
                    tailnet_address: "100.64.0.2".parse().unwrap(),
                    application_port: 7777,
                    node_key: None,
                },
                RosterManifestEntry {
                    player_id: player(1),
                    session_public_key: SessionPublicKey::from_bytes([1; 32]),
                    tailnet_address: "100.64.0.1".parse().unwrap(),
                    application_port: 7777,
                    node_key: Some(NodeKey::from_bytes([9; 32])),
                },
            ],
        };
        manifest.validate().unwrap();
        let digest = canonical_manifest_digest(public, &manifest);
        let signature = SessionSignature::from_bytes(signing.sign(&digest).to_bytes());
        public.verify_digest(&digest, signature).unwrap();
        let mut reordered = manifest.clone();
        reordered.entries.reverse();
        assert_eq!(
            canonical_roster_bytes(&manifest),
            canonical_roster_bytes(&reordered)
        );
        assert_eq!(manifest.hash(), reordered.hash());

        let mut tampered = manifest;
        tampered.session_generation += 1;
        assert_eq!(
            public.verify_digest(&canonical_manifest_digest(public, &tampered), signature),
            Err(SessionIdentityError::BadSignature)
        );
    }

    #[test]
    fn identity_wire_encodings_are_bounded_and_debug_redacted() {
        let key = SessionPublicKey::from_bytes([0xab; 32]);
        let signature = SessionSignature::from_bytes([0xcd; 64]);
        assert_eq!(
            serde_json::from_str::<SessionPublicKey>(&serde_json::to_string(&key).unwrap())
                .unwrap(),
            key
        );
        assert_eq!(
            serde_json::from_str::<SessionSignature>(&serde_json::to_string(&signature).unwrap())
                .unwrap(),
            signature
        );
        assert!(!format!("{key:?}{signature:?}").contains("abab"));
        assert!(NodeKey::parse(&format!("nodekey:{}", "01".repeat(32))).is_ok());
        assert!(NodeKey::parse(&format!("nodekey:{}", "AA".repeat(32))).is_err());
    }
}
