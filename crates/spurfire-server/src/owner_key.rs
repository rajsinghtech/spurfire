//! Public owner identity compiled into the protected launcher.
//!
//! Updating this file requires reviewing the public output of
//! `spurfire-owner-key init`; the private seed is never represented here.

/// Stable owner key identifier included in signed receipts.
pub const OWNER_KEY_ID: &str = "raj-protected-alpha-owner-v1";
/// Ed25519 verifying key. This intentionally non-secret bootstrap placeholder
/// makes protected activation fail closed until Raj installs and compiles the
/// public key emitted by the Keychain workflow.
pub const OWNER_PUBLIC_KEY: [u8; 32] = [0; 32];

pub fn verifying_key() -> Result<ed25519_dalek::VerifyingKey, ed25519_dalek::SignatureError> {
    ed25519_dalek::VerifyingKey::from_bytes(&OWNER_PUBLIC_KEY)
}
