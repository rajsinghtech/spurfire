//! Public owner identity compiled into the protected launcher.
//!
//! Updating this file requires reviewing the public output of
//! `spurfire-owner-key init`; the private seed is never represented here.

/// Stable owner key identifier included in signed receipts.
pub const OWNER_KEY_ID: &str = "raj-protected-alpha-owner-v1";
/// Ed25519 verifying key emitted by the macOS Keychain owner workflow. The
/// corresponding seed remains non-exportable from the Spurfire workflow and is
/// never represented in this repository, an image, or the cluster.
pub const OWNER_PUBLIC_KEY: [u8; 32] = [
    0x84, 0xe6, 0x1f, 0xaa, 0xf2, 0x9a, 0xf6, 0xbc, 0xea, 0x9c, 0x00, 0x8a, 0x25, 0xee, 0x22, 0x5c,
    0xe6, 0x60, 0xc1, 0x1f, 0xfc, 0xb9, 0x2e, 0x56, 0x6e, 0xa0, 0x13, 0x47, 0x78, 0x00, 0x3c, 0xcf,
];

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
