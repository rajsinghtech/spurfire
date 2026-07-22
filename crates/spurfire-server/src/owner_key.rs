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
    let key = ed25519_dalek::VerifyingKey::from_bytes(&OWNER_PUBLIC_KEY)?;
    if OWNER_PUBLIC_KEY == [0; 32] || key.is_weak() {
        return Err(ed25519_dalek::SignatureError::new());
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_owner_key_is_disabled_or_strong() {
        if OWNER_PUBLIC_KEY == [0; 32] {
            assert!(verifying_key().is_err());
        } else {
            assert!(!verifying_key().unwrap().is_weak());
        }
    }
}
