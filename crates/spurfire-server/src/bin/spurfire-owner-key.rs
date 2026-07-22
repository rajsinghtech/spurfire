//! Offline macOS Keychain owner-key workflow. No command accepts secret input.

#[cfg(target_os = "macos")]
mod macos {
    use clap::{Parser, Subcommand};
    use ed25519_dalek::{Signer, SigningKey};
    use security_framework::{os::macos::keychain::SecKeychain, passwords::get_generic_password};
    use serde::Serialize;
    use spurfire_server::{owner_key::OWNER_KEY_ID, ProtectedAlphaClaims, ProtectedAlphaReceipt};
    use std::io::{Read, Write};
    use zeroize::{Zeroize, Zeroizing};

    const SERVICE: &str = "dev.spurfire.protected-alpha.owner-key";
    const MAX_CLAIMS: u64 = 64 * 1024;

    #[derive(Parser)]
    #[command(
        name = "spurfire-owner-key",
        about = "Offline protected Alpha receipt owner"
    )]
    struct Args {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Subcommand)]
    enum Command {
        /// Generate directly in memory, save seed to Keychain, emit public key only.
        Init,
        /// Retrieve the seed only to derive and emit the public key.
        Public,
        /// Read canonical claims from stdin and emit a signed receipt to stdout.
        Sign,
    }

    #[derive(Serialize)]
    struct PublicIdentity<'a> {
        owner_key_id: &'a str,
        ed25519_public_key_hex: String,
    }

    pub fn main() {
        if let Err(message) = run() {
            eprintln!("{message}");
            std::process::exit(78);
        }
    }

    fn run() -> Result<(), &'static str> {
        match Args::parse().command {
            Command::Init => {
                if get_generic_password(SERVICE, OWNER_KEY_ID).is_ok() {
                    return Err("KEYCHAIN_BLOCKED: owner key already exists");
                }
                let mut seed = Zeroizing::new([0_u8; 32]);
                getrandom::getrandom(seed.as_mut())
                    .map_err(|_| "KEYCHAIN_BLOCKED: secure randomness unavailable")?;
                // Security.framework receives bytes directly in-process. There is
                // no `security -w`, argv, stdout, or plaintext temporary file.
                // Add-only storage prevents a read error or concurrent initializer
                // from rotating an existing owner identity.
                SecKeychain::default()
                    .and_then(|keychain| {
                        keychain.add_generic_password(SERVICE, OWNER_KEY_ID, seed.as_slice())
                    })
                    .map_err(|_| "KEYCHAIN_BLOCKED: Security.framework write failed")?;
                emit_public(&SigningKey::from_bytes(&seed))
            }
            Command::Public => with_key(emit_public),
            Command::Sign => with_key(sign_stdin),
        }
    }

    fn with_key(
        action: impl FnOnce(&SigningKey) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let mut seed = Zeroizing::new(
            get_generic_password(SERVICE, OWNER_KEY_ID)
                .map_err(|_| "KEYCHAIN_BLOCKED: Security.framework read failed")?,
        );
        let bytes: [u8; 32] = seed
            .as_slice()
            .try_into()
            .map_err(|_| "KEYCHAIN_BLOCKED: Keychain owner seed is invalid")?;
        let key = SigningKey::from_bytes(&bytes);
        let result = action(&key);
        seed.zeroize();
        result
    }

    fn emit_public(key: &SigningKey) -> Result<(), &'static str> {
        let identity = PublicIdentity {
            owner_key_id: OWNER_KEY_ID,
            ed25519_public_key_hex: hex::encode(key.verifying_key().to_bytes()),
        };
        serde_json::to_writer(std::io::stdout().lock(), &identity)
            .map_err(|_| "KEYCHAIN_BLOCKED: public output failed")?;
        std::io::stdout()
            .lock()
            .write_all(b"\n")
            .map_err(|_| "KEYCHAIN_BLOCKED: public output failed")
    }

    fn sign_stdin(key: &SigningKey) -> Result<(), &'static str> {
        let mut bytes = Zeroizing::new(Vec::new());
        std::io::stdin()
            .lock()
            .take(MAX_CLAIMS)
            .read_to_end(&mut bytes)
            .map_err(|_| "KEYCHAIN_BLOCKED: claims input failed")?;
        let claims: ProtectedAlphaClaims =
            serde_json::from_slice(&bytes).map_err(|_| "KEYCHAIN_BLOCKED: claims are malformed")?;
        bytes.zeroize();
        if claims.owner_key_id != OWNER_KEY_ID {
            return Err("KEYCHAIN_BLOCKED: owner key ID mismatch");
        }
        let canonical = Zeroizing::new(
            serde_json::to_vec(&claims)
                .map_err(|_| "KEYCHAIN_BLOCKED: claims canonicalization failed")?,
        );
        let receipt = ProtectedAlphaReceipt {
            claims,
            signature: key.sign(&canonical).to_bytes().to_vec(),
        };
        // Only the signed receipt is emitted. The seed and canonical scratch
        // remain in memory and are zeroized on return.
        serde_json::to_writer(std::io::stdout().lock(), &receipt)
            .map_err(|_| "KEYCHAIN_BLOCKED: signed receipt output failed")?;
        std::io::stdout()
            .lock()
            .write_all(b"\n")
            .map_err(|_| "KEYCHAIN_BLOCKED: signed receipt output failed")
    }
}

#[cfg(target_os = "macos")]
fn main() {
    macos::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("KEYCHAIN_BLOCKED: macOS Security.framework is required");
    std::process::exit(78);
}
