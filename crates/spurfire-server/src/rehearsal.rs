//! Fail-closed qualification for the non-hosted local rehearsal binary.
//!
//! The ordinary server never constructs this authority. A receipt is delivered
//! over a protected local descriptor after a fresh boot challenge is generated;
//! plaintext receipt bytes are verified and immediately zeroized.

use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spurfire_protocol::{LobbyId, ProvisioningMode, UnixMillis, MAX_PLAYERS};
use thiserror::Error;
use zeroize::Zeroize;

/// Receipt audience. Changing semantics requires a new audience/version.
pub const LOCAL_REHEARSAL_AUDIENCE: &str = "spurfire-local-rehearsal/v1";
/// The reviewed source revision authorized for this one rehearsal implementation.
pub const REVIEWED_SOURCE_SHA: &str = "e89237ef04cdeb46eb9381d9766bbefeb3b458ee";
/// Maximum receipt validity.
pub const MAX_RECEIPT_LIFETIME: Duration = Duration::from_secs(5 * 60);
/// Only this restrictive policy profile may be rehearsed.
pub const REHEARSAL_POLICY_PROFILE: &str = "spurfire-rider-isolation/v1";
/// Audience for the separately deployed, one-lobby hosted Alpha execution plane.
pub const PROTECTED_ALPHA_AUDIENCE: &str = "spurfire-protected-alpha/v1";
/// The only purpose accepted by the protected Alpha verifier.
pub const PROTECTED_ALPHA_PURPOSE: &str = "bounded_hosted_alpha";

/// Canonical signed claims. Struct field order is part of the v1 canonical form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRehearsalClaims {
    pub audience: String,
    pub receipt_id: String,
    pub source_sha: String,
    pub executable_sha256: String,
    pub provenance_sha256: String,
    pub boot_challenge_sha256: String,
    pub owner_key_id: String,
    pub issued_at: UnixMillis,
    pub expires_at: UnixMillis,
    pub listener: String,
    pub expected_peer_uid: u32,
    pub lobby_id: LobbyId,
    pub network_generation: u64,
    pub provisioning_mode: ProvisioningMode,
    pub policy_profile: String,
    pub participant_cap: u8,
    pub absolute_deadline: UnixMillis,
    pub hosted: bool,
    pub purpose: String,
}

/// Signed receipt envelope. Signature bytes are encoded as a JSON byte array so
/// no bearer-friendly string form is created by this crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRehearsalReceipt {
    pub claims: LocalRehearsalClaims,
    pub signature: Vec<u8>,
}

/// Process/listener measurements supplied by the dedicated rehearsal binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RehearsalVerificationContext {
    pub now: UnixMillis,
    pub executable_sha256: [u8; 32],
    pub provenance_sha256: [u8; 32],
    pub boot_challenge_sha256: [u8; 32],
    pub listener: SocketAddr,
    pub peer_uid: u32,
}

/// Hash-only authority retained after receipt verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRehearsalQualification {
    pub(crate) receipt_verifier: [u8; 32],
    pub(crate) receipt_id_digest: [u8; 32],
    pub(crate) lobby_id: LobbyId,
    pub(crate) network_generation: u64,
    pub(crate) expires_at: UnixMillis,
    pub(crate) absolute_deadline: UnixMillis,
    pub(crate) participant_cap: u8,
    pub(crate) policy_profile_digest: [u8; 32],
}

impl LocalRehearsalQualification {
    #[must_use]
    pub const fn lobby_id(&self) -> LobbyId {
        self.lobby_id
    }

    #[must_use]
    pub const fn expires_at(&self) -> UnixMillis {
        self.expires_at
    }

    #[must_use]
    pub const fn absolute_deadline(&self) -> UnixMillis {
        self.absolute_deadline
    }
}

/// Receipt verification failures deliberately contain no receipt values.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum RehearsalReceiptError {
    #[error("local rehearsal receipt is malformed")]
    Malformed,
    #[error("local rehearsal receipt signature is invalid")]
    InvalidSignature,
    #[error("local rehearsal receipt signer is not trusted")]
    UnknownSigner,
    #[error("local rehearsal receipt binding is invalid")]
    InvalidBinding,
    #[error("local rehearsal receipt is outside its validity window")]
    InvalidLifetime,
    #[error("local rehearsal listener is not private loopback")]
    InvalidListener,
}

/// Canonical hosted-Alpha claims. Field order is the v1 signed canonical form.
/// The receipt contains authority but never provider or vault credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedAlphaClaims {
    pub audience: String,
    pub receipt_id: String,
    pub source_sha: String,
    pub worker_sha256: String,
    pub broker_sha256: String,
    pub provenance_sha256: String,
    pub artifact_set_sha256: String,
    pub policy_profile_sha256: String,
    pub public_origin: String,
    pub internal_listener: String,
    pub lobby_id: LobbyId,
    pub network_generation: u64,
    pub store_instance_id_sha256: String,
    pub canonical_state_path_sha256: String,
    pub supervisor_run_id: String,
    pub initial_epoch: u64,
    pub participant_cap: u8,
    pub issued_at: UnixMillis,
    pub expires_at: UnixMillis,
    pub final_io_deadline: UnixMillis,
    pub absolute_deadline: UnixMillis,
    pub provisioning_mode: ProvisioningMode,
    pub hosted: bool,
    pub purpose: String,
    pub owner_key_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedAlphaReceipt {
    pub claims: ProtectedAlphaClaims,
    pub signature: Vec<u8>,
}

/// Measurements supplied by the protected launcher, including the challenge
/// obtained from the already-open durable store before the receipt is issued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedAlphaVerificationContext {
    pub now: UnixMillis,
    pub source_sha: String,
    pub worker_sha256: [u8; 32],
    pub broker_sha256: [u8; 32],
    pub provenance_sha256: [u8; 32],
    pub artifact_set_sha256: [u8; 32],
    pub policy_profile_sha256: [u8; 32],
    pub public_origin: String,
    pub internal_listener: String,
    pub store_instance_id_sha256: [u8; 32],
    pub canonical_state_path_sha256: [u8; 32],
}

/// Hash-only, exact-lobby authority. Its fields are private so it can only be
/// constructed by signature verification in this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedAlphaQualification {
    pub(crate) receipt_verifier: [u8; 32],
    pub(crate) receipt_id_digest: [u8; 32],
    pub(crate) lobby_id: LobbyId,
    pub(crate) network_generation: u64,
    pub(crate) store_instance_id_sha256: [u8; 32],
    pub(crate) canonical_state_path_sha256: [u8; 32],
    pub(crate) supervisor_run_id_digest: [u8; 32],
    pub(crate) initial_epoch: u64,
    pub(crate) expires_at: UnixMillis,
    pub(crate) final_io_deadline: UnixMillis,
    pub(crate) absolute_deadline: UnixMillis,
    pub(crate) participant_cap: u8,
    pub(crate) worker_sha256: [u8; 32],
    pub(crate) broker_sha256: [u8; 32],
    pub(crate) provenance_sha256: [u8; 32],
    pub(crate) artifact_set_sha256: [u8; 32],
    pub(crate) policy_profile_digest: [u8; 32],
    pub(crate) public_origin_digest: [u8; 32],
    pub(crate) internal_listener_digest: [u8; 32],
}

impl ProtectedAlphaQualification {
    #[must_use]
    pub const fn lobby_id(&self) -> LobbyId {
        self.lobby_id
    }
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.network_generation
    }
    #[must_use]
    pub const fn final_io_deadline(&self) -> UnixMillis {
        self.final_io_deadline
    }
    #[must_use]
    pub const fn absolute_deadline(&self) -> UnixMillis {
        self.absolute_deadline
    }
}

/// Verify, reduce, and zeroize a hosted Alpha receipt. A valid signature is not
/// enough: every deployment, artifact, store, origin and deadline binding must
/// exactly match launcher measurements.
pub fn verify_protected_alpha_receipt(
    receipt_bytes: &mut [u8],
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    context: &ProtectedAlphaVerificationContext,
) -> Result<ProtectedAlphaQualification, RehearsalReceiptError> {
    let result = verify_protected_alpha_inner(receipt_bytes, trusted_keys, context);
    receipt_bytes.zeroize();
    result
}

fn verify_protected_alpha_inner(
    receipt_bytes: &[u8],
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    context: &ProtectedAlphaVerificationContext,
) -> Result<ProtectedAlphaQualification, RehearsalReceiptError> {
    let receipt: ProtectedAlphaReceipt =
        serde_json::from_slice(receipt_bytes).map_err(|_| RehearsalReceiptError::Malformed)?;
    let claims_bytes =
        serde_json::to_vec(&receipt.claims).map_err(|_| RehearsalReceiptError::Malformed)?;
    let key = trusted_keys
        .get(&receipt.claims.owner_key_id)
        .ok_or(RehearsalReceiptError::UnknownSigner)?;
    let signature = Signature::from_slice(&receipt.signature)
        .map_err(|_| RehearsalReceiptError::InvalidSignature)?;
    key.verify(&claims_bytes, &signature)
        .map_err(|_| RehearsalReceiptError::InvalidSignature)?;
    let claims = &receipt.claims;
    let lifetime = claims
        .expires_at
        .checked_duration_since(claims.issued_at)
        .ok_or(RehearsalReceiptError::InvalidLifetime)?;
    if context.now < claims.issued_at
        || context.now >= claims.expires_at
        || lifetime == 0
        || lifetime > MAX_RECEIPT_LIFETIME.as_millis() as u64
        || claims.final_io_deadline <= context.now
        || claims.final_io_deadline > claims.absolute_deadline
        || claims.absolute_deadline > claims.expires_at
    {
        return Err(RehearsalReceiptError::InvalidLifetime);
    }
    let private_listener = claims.internal_listener.starts_with('/')
        || claims
            .internal_listener
            .parse::<SocketAddr>()
            .is_ok_and(|value| value.ip().is_loopback() && !value.ip().is_unspecified());
    if !private_listener
        || !claims.public_origin.starts_with("https://")
        || claims.public_origin.ends_with('/')
    {
        return Err(RehearsalReceiptError::InvalidListener);
    }
    if claims.audience != PROTECTED_ALPHA_AUDIENCE
        || claims.purpose != PROTECTED_ALPHA_PURPOSE
        || !claims.hosted
        || claims.provisioning_mode != ProvisioningMode::TailnetPerLobby
        || claims.network_generation == 0
        || claims.initial_epoch == 0
        || claims.participant_cap == 0
        || claims.participant_cap > MAX_PLAYERS
        || claims.receipt_id.len() < 32
        || claims.supervisor_run_id.len() < 16
        || claims.source_sha != context.source_sha
        || claims.worker_sha256 != hex(&context.worker_sha256)
        || claims.broker_sha256 != hex(&context.broker_sha256)
        || claims.provenance_sha256 != hex(&context.provenance_sha256)
        || claims.artifact_set_sha256 != hex(&context.artifact_set_sha256)
        || claims.policy_profile_sha256 != hex(&context.policy_profile_sha256)
        || claims.public_origin != context.public_origin
        || claims.internal_listener != context.internal_listener
        || claims.store_instance_id_sha256 != hex(&context.store_instance_id_sha256)
        || claims.canonical_state_path_sha256 != hex(&context.canonical_state_path_sha256)
    {
        return Err(RehearsalReceiptError::InvalidBinding);
    }
    Ok(ProtectedAlphaQualification {
        receipt_verifier: domain_hash(b"spurfire-protected-alpha-receipt-v1\0", &claims_bytes),
        receipt_id_digest: domain_hash(
            b"spurfire-protected-alpha-id-v1\0",
            claims.receipt_id.as_bytes(),
        ),
        lobby_id: claims.lobby_id,
        network_generation: claims.network_generation,
        store_instance_id_sha256: context.store_instance_id_sha256,
        canonical_state_path_sha256: context.canonical_state_path_sha256,
        supervisor_run_id_digest: domain_hash(
            b"spurfire-protected-alpha-run-v1\0",
            claims.supervisor_run_id.as_bytes(),
        ),
        initial_epoch: claims.initial_epoch,
        expires_at: claims.expires_at,
        final_io_deadline: claims.final_io_deadline,
        absolute_deadline: claims.absolute_deadline,
        participant_cap: claims.participant_cap,
        worker_sha256: context.worker_sha256,
        broker_sha256: context.broker_sha256,
        provenance_sha256: context.provenance_sha256,
        artifact_set_sha256: context.artifact_set_sha256,
        policy_profile_digest: context.policy_profile_sha256,
        public_origin_digest: domain_hash(
            b"spurfire-protected-alpha-origin-v1\0",
            claims.public_origin.as_bytes(),
        ),
        internal_listener_digest: domain_hash(
            b"spurfire-protected-alpha-listener-v1\0",
            claims.internal_listener.as_bytes(),
        ),
    })
}

/// Verify and reduce a signed receipt to non-secret, hash-only authority.
pub fn verify_local_rehearsal_receipt(
    receipt_bytes: &mut [u8],
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    context: &RehearsalVerificationContext,
) -> Result<LocalRehearsalQualification, RehearsalReceiptError> {
    let result = verify_inner(receipt_bytes, trusted_keys, context);
    receipt_bytes.zeroize();
    result
}

fn verify_inner(
    receipt_bytes: &[u8],
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    context: &RehearsalVerificationContext,
) -> Result<LocalRehearsalQualification, RehearsalReceiptError> {
    let receipt: LocalRehearsalReceipt =
        serde_json::from_slice(receipt_bytes).map_err(|_| RehearsalReceiptError::Malformed)?;
    let claims_bytes =
        serde_json::to_vec(&receipt.claims).map_err(|_| RehearsalReceiptError::Malformed)?;
    let key = trusted_keys
        .get(&receipt.claims.owner_key_id)
        .ok_or(RehearsalReceiptError::UnknownSigner)?;
    let signature = Signature::from_slice(&receipt.signature)
        .map_err(|_| RehearsalReceiptError::InvalidSignature)?;
    key.verify(&claims_bytes, &signature)
        .map_err(|_| RehearsalReceiptError::InvalidSignature)?;

    let claims = &receipt.claims;
    let lifetime = claims
        .expires_at
        .checked_duration_since(claims.issued_at)
        .ok_or(RehearsalReceiptError::InvalidLifetime)?;
    if context.now < claims.issued_at
        || context.now >= claims.expires_at
        || lifetime == 0
        || lifetime > MAX_RECEIPT_LIFETIME.as_millis() as u64
        || claims.absolute_deadline <= context.now
        || claims.absolute_deadline > claims.expires_at.saturating_add(30_000)
    {
        return Err(RehearsalReceiptError::InvalidLifetime);
    }
    if !context.listener.ip().is_loopback()
        || context.listener.ip().is_unspecified()
        || claims.listener != context.listener.to_string()
        || claims.expected_peer_uid != context.peer_uid
    {
        return Err(RehearsalReceiptError::InvalidListener);
    }
    if claims.audience != LOCAL_REHEARSAL_AUDIENCE
        || claims.source_sha != REVIEWED_SOURCE_SHA
        || claims.executable_sha256 != hex(&context.executable_sha256)
        || claims.provenance_sha256 != hex(&context.provenance_sha256)
        || claims.boot_challenge_sha256 != hex(&context.boot_challenge_sha256)
        || claims.network_generation != 1
        || claims.provisioning_mode != ProvisioningMode::TailnetPerLobby
        || claims.policy_profile != REHEARSAL_POLICY_PROFILE
        || claims.participant_cap == 0
        || claims.participant_cap > MAX_PLAYERS
        || claims.hosted
        || claims.purpose != "local_rehearsal"
        || claims.receipt_id.len() < 32
    {
        return Err(RehearsalReceiptError::InvalidBinding);
    }

    let receipt_verifier = domain_hash(b"spurfire-local-rehearsal-receipt-v1\0", &claims_bytes);
    Ok(LocalRehearsalQualification {
        receipt_verifier,
        receipt_id_digest: domain_hash(
            b"spurfire-local-rehearsal-id-v1\0",
            claims.receipt_id.as_bytes(),
        ),
        lobby_id: claims.lobby_id,
        network_generation: claims.network_generation,
        expires_at: claims.expires_at,
        absolute_deadline: claims.absolute_deadline,
        participant_cap: claims.participant_cap,
        policy_profile_digest: domain_hash(
            b"spurfire-local-rehearsal-policy-v1\0",
            claims.policy_profile.as_bytes(),
        ),
    })
}

fn domain_hash(domain: &[u8], value: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(value);
    digest.finalize().into()
}

fn hex(bytes: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(64);
    for byte in bytes {
        value.push(char::from(DIGITS[usize::from(byte >> 4)]));
        value.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use spurfire_protocol::LobbyId;

    fn fixture(
        now: u64,
    ) -> (
        LocalRehearsalReceipt,
        BTreeMap<String, VerifyingKey>,
        RehearsalVerificationContext,
    ) {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let context = RehearsalVerificationContext {
            now: UnixMillis::new(now),
            executable_sha256: [1; 32],
            provenance_sha256: [2; 32],
            boot_challenge_sha256: [3; 32],
            listener: "127.0.0.1:7777".parse().unwrap(),
            peer_uid: 501,
        };
        let claims = LocalRehearsalClaims {
            audience: LOCAL_REHEARSAL_AUDIENCE.into(),
            receipt_id: "0123456789abcdef0123456789abcdef".into(),
            source_sha: REVIEWED_SOURCE_SHA.into(),
            executable_sha256: hex(&context.executable_sha256),
            provenance_sha256: hex(&context.provenance_sha256),
            boot_challenge_sha256: hex(&context.boot_challenge_sha256),
            owner_key_id: "owner-1".into(),
            issued_at: UnixMillis::new(now - 1),
            expires_at: UnixMillis::new(now + 60_000),
            listener: context.listener.to_string(),
            expected_peer_uid: context.peer_uid,
            lobby_id: LobbyId::parse("00000000-0000-4000-8000-000000000019").unwrap(),
            network_generation: 1,
            provisioning_mode: ProvisioningMode::TailnetPerLobby,
            policy_profile: REHEARSAL_POLICY_PROFILE.into(),
            participant_cap: 2,
            absolute_deadline: UnixMillis::new(now + 65_000),
            hosted: false,
            purpose: "local_rehearsal".into(),
        };
        let signature = signing
            .sign(&serde_json::to_vec(&claims).unwrap())
            .to_bytes()
            .to_vec();
        let receipt = LocalRehearsalReceipt { claims, signature };
        let keys = BTreeMap::from([("owner-1".into(), signing.verifying_key())]);
        (receipt, keys, context)
    }

    #[test]
    fn verifies_and_zeroizes_receipt() {
        let (receipt, keys, context) = fixture(1_000_000);
        let mut bytes = serde_json::to_vec(&receipt).unwrap();
        let qualification = verify_local_rehearsal_receipt(&mut bytes, &keys, &context).unwrap();
        assert_eq!(qualification.network_generation, 1);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }

    fn alpha_fixture(
        now: u64,
    ) -> (
        ProtectedAlphaReceipt,
        BTreeMap<String, VerifyingKey>,
        ProtectedAlphaVerificationContext,
    ) {
        let signing = SigningKey::from_bytes(&[9; 32]);
        let context = ProtectedAlphaVerificationContext {
            now: UnixMillis::new(now),
            source_sha: "4feada6bbb0cf60d171f7cf96412bfab8b634970".into(),
            worker_sha256: [1; 32],
            broker_sha256: [2; 32],
            provenance_sha256: [3; 32],
            artifact_set_sha256: [4; 32],
            policy_profile_sha256: [5; 32],
            public_origin: "https://alpha.spurfire.invalid".into(),
            internal_listener: "/run/spurfire/alpha.sock".into(),
            store_instance_id_sha256: [6; 32],
            canonical_state_path_sha256: [7; 32],
        };
        let claims = ProtectedAlphaClaims {
            audience: PROTECTED_ALPHA_AUDIENCE.into(),
            receipt_id: "fedcba9876543210fedcba9876543210".into(),
            source_sha: context.source_sha.clone(),
            worker_sha256: hex(&context.worker_sha256),
            broker_sha256: hex(&context.broker_sha256),
            provenance_sha256: hex(&context.provenance_sha256),
            artifact_set_sha256: hex(&context.artifact_set_sha256),
            policy_profile_sha256: hex(&context.policy_profile_sha256),
            public_origin: context.public_origin.clone(),
            internal_listener: context.internal_listener.clone(),
            lobby_id: LobbyId::parse("00000000-0000-4000-8000-0000000000aa").unwrap(),
            network_generation: 7,
            store_instance_id_sha256: hex(&context.store_instance_id_sha256),
            canonical_state_path_sha256: hex(&context.canonical_state_path_sha256),
            supervisor_run_id: "run-fedcba9876543210".into(),
            initial_epoch: 1,
            participant_cap: 2,
            issued_at: UnixMillis::new(now - 1),
            expires_at: UnixMillis::new(now + 60_000),
            final_io_deadline: UnixMillis::new(now + 30_000),
            absolute_deadline: UnixMillis::new(now + 50_000),
            provisioning_mode: ProvisioningMode::TailnetPerLobby,
            hosted: true,
            purpose: PROTECTED_ALPHA_PURPOSE.into(),
            owner_key_id: "alpha-owner".into(),
        };
        let signature = signing
            .sign(&serde_json::to_vec(&claims).unwrap())
            .to_bytes()
            .to_vec();
        (
            ProtectedAlphaReceipt { claims, signature },
            BTreeMap::from([("alpha-owner".into(), signing.verifying_key())]),
            context,
        )
    }

    #[test]
    fn protected_alpha_binds_every_execution_boundary_and_zeroizes() {
        let (receipt, keys, context) = alpha_fixture(2_000_000);
        let mut bytes = serde_json::to_vec(&receipt).unwrap();
        let qualification = verify_protected_alpha_receipt(&mut bytes, &keys, &context).unwrap();
        assert_eq!(qualification.generation(), 7);
        assert!(bytes.iter().all(|byte| *byte == 0));
        for field in 0..8 {
            let mut changed = context.clone();
            match field {
                0 => changed.source_sha = "0".repeat(40),
                1 => changed.worker_sha256[0] ^= 1,
                2 => changed.broker_sha256[0] ^= 1,
                3 => changed.artifact_set_sha256[0] ^= 1,
                4 => changed.public_origin.push_str(".other"),
                5 => changed.internal_listener.push_str(".other"),
                6 => changed.store_instance_id_sha256[0] ^= 1,
                _ => changed.canonical_state_path_sha256[0] ^= 1,
            }
            let mut bytes = serde_json::to_vec(&receipt).unwrap();
            assert!(verify_protected_alpha_receipt(&mut bytes, &keys, &changed).is_err());
            assert!(bytes.iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn rejects_expired_wrong_sha_wrong_mode_and_replay_binding() {
        let (receipt, keys, context) = fixture(1_000_000);
        for mutate in 0..5 {
            let mut candidate = receipt.clone();
            match mutate {
                0 => candidate.claims.expires_at = UnixMillis::new(999_999),
                1 => candidate.claims.source_sha = "0".repeat(40),
                2 => candidate.claims.provisioning_mode = ProvisioningMode::SharedTailnet,
                3 => candidate.claims.absolute_deadline = context.now,
                _ => candidate.claims.boot_challenge_sha256 = "4".repeat(64),
            }
            // Re-sign to isolate claim validation from signature validation.
            let signing = SigningKey::from_bytes(&[7; 32]);
            candidate.signature = signing
                .sign(&serde_json::to_vec(&candidate.claims).unwrap())
                .to_bytes()
                .to_vec();
            let mut bytes = serde_json::to_vec(&candidate).unwrap();
            assert!(verify_local_rehearsal_receipt(&mut bytes, &keys, &context).is_err());
            assert!(bytes.iter().all(|byte| *byte == 0));
        }
    }
}
