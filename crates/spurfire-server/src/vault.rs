//! Encrypted, versioned custody for dynamically-created child OAuth credentials.
//!
//! The vault key is read from a root/workload-mounted file, never an environment
//! variable. Durable JSON contains only exact identity metadata, nonce, and
//! authenticated ciphertext. Plaintext is zeroized after encryption/decryption.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
#[cfg(unix)]
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use spurfire_control::ChildOAuthCredentials;
use spurfire_protocol::{LobbyId, TailnetDnsName};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use zeroize::{Zeroize, Zeroizing};

use crate::crypto::{base64url_decode, base64url_encode};

const VAULT_SCHEMA: u32 = 1;
const NONCE_BYTES: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildVaultIdentity {
    pub lobby_id: LobbyId,
    pub network_generation: u64,
    pub provider_tailnet_id: String,
    pub tailnet_dns_name: TailnetDnsName,
}

impl ChildVaultIdentity {
    fn aad(&self, version: u64) -> Vec<u8> {
        format!(
            "spurfire-child-vault-v1\0{}\0{}\0{}\0{}\0{}",
            self.lobby_id,
            self.network_generation,
            self.provider_tailnet_id,
            self.tailnet_dns_name.as_str(),
            version,
        )
        .into_bytes()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct EncryptedRecord {
    identity: ChildVaultIdentity,
    version: u64,
    nonce: String,
    ciphertext: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ErasureReceipt {
    identity: ChildVaultIdentity,
    version: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct VaultImage {
    schema_version: u32,
    records: BTreeMap<LobbyId, EncryptedRecord>,
    /// Non-secret, exact-tuple proof that custody existed and was CAS-erased.
    #[serde(default)]
    erasures: BTreeMap<LobbyId, ErasureReceipt>,
}

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("child vault I/O failed")]
    Io,
    #[error("child vault image or key is invalid")]
    Invalid,
    #[error("child vault identity/version conflict")]
    Conflict,
    #[error("child vault record is unavailable")]
    Missing,
    #[error("child vault encryption failed")]
    Crypto,
}

#[derive(Clone)]
pub struct EncryptedChildVault {
    path: Arc<PathBuf>,
    key: Arc<Zeroizing<[u8; 32]>>,
    image: Arc<RwLock<VaultImage>>,
    mutation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Lifetime-held OS advisory lock fencing every writer of this vault image.
    _writer_lock: Arc<std::fs::File>,
}

impl std::fmt::Debug for EncryptedChildVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedChildVault")
            .field("path", &self.path)
            .field("key", &"<redacted>")
            .field("records", &"<encrypted>")
            .finish()
    }
}

impl EncryptedChildVault {
    #[cfg(not(unix))]
    pub async fn open(
        path: impl Into<PathBuf>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, VaultError> {
        let _ = (path.into(), key_path.as_ref());
        // Durable secret custody requires atomic replacement plus directory
        // durability. Unsupported platforms fail before provider activation.
        Err(VaultError::Invalid)
    }

    #[cfg(unix)]
    pub async fn open(
        path: impl Into<PathBuf>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, VaultError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|_| VaultError::Io)?;
        }
        let writer_lock = open_writer_lock(&path).map_err(|_| VaultError::Io)?;
        let key = read_key_file(key_path.as_ref()).await?;
        let image = match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let image: VaultImage =
                    serde_json::from_slice(&bytes).map_err(|_| VaultError::Invalid)?;
                if image.schema_version != VAULT_SCHEMA {
                    return Err(VaultError::Invalid);
                }
                image
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => VaultImage {
                schema_version: VAULT_SCHEMA,
                records: BTreeMap::new(),
                erasures: BTreeMap::new(),
            },
            Err(_) => return Err(VaultError::Io),
        };
        Ok(Self {
            path: Arc::new(path),
            key: Arc::new(Zeroizing::new(key)),
            image: Arc::new(RwLock::new(image)),
            mutation_lock: Arc::new(tokio::sync::Mutex::new(())),
            _writer_lock: Arc::new(writer_lock),
        })
    }

    pub fn contains_lobby(&self, lobby_id: LobbyId) -> bool {
        self.image
            .read()
            .is_ok_and(|image| image.records.contains_key(&lobby_id))
    }

    pub fn identities(&self) -> Result<Vec<ChildVaultIdentity>, VaultError> {
        let image = self.image.read().map_err(|_| VaultError::Io)?;
        Ok(image
            .records
            .values()
            .map(|record| record.identity.clone())
            .collect())
    }

    /// Checks the loaded, lifetime-lock-protected image for an exact erasure receipt.
    pub fn has_erasure_receipt(&self, identity: &ChildVaultIdentity) -> Result<bool, VaultError> {
        let image = self.image.read().map_err(|_| VaultError::Io)?;
        Ok(!image.records.contains_key(&identity.lobby_id)
            && image
                .erasures
                .get(&identity.lobby_id)
                .is_some_and(|receipt| &receipt.identity == identity && receipt.version > 0))
    }

    pub async fn put_if_absent(
        &self,
        identity: ChildVaultIdentity,
        credentials: ChildOAuthCredentials,
    ) -> Result<u64, VaultError> {
        let _mutation = self.mutation_lock.lock().await;
        let (mut client_id, mut client_secret) = credentials.into_secret_parts();
        let mut plaintext = encode_plaintext(&client_id, &client_secret)?;
        client_id.zeroize();
        client_secret.zeroize();
        let result = async {
            let mut next = self.image.read().map_err(|_| VaultError::Io)?.clone();
            if let Some(existing) = next.records.get(&identity.lobby_id) {
                return if existing.identity == identity {
                    Ok(existing.version)
                } else {
                    Err(VaultError::Conflict)
                };
            }
            if next.erasures.contains_key(&identity.lobby_id) {
                return Err(VaultError::Conflict);
            }
            let version = 1;
            let mut nonce_bytes = [0_u8; NONCE_BYTES];
            getrandom::getrandom(&mut nonce_bytes).map_err(|_| VaultError::Crypto)?;
            let cipher = ChaCha20Poly1305::new_from_slice(self.key.as_ref().as_slice())
                .map_err(|_| VaultError::Crypto)?;
            let ciphertext = cipher
                .encrypt(
                    Nonce::from_slice(&nonce_bytes),
                    Payload {
                        msg: &plaintext,
                        aad: &identity.aad(version),
                    },
                )
                .map_err(|_| VaultError::Crypto)?;
            next.records.insert(
                identity.lobby_id,
                EncryptedRecord {
                    identity,
                    version,
                    nonce: base64url_encode(&nonce_bytes),
                    ciphertext: base64url_encode(&ciphertext),
                },
            );
            self.persist(&next).await?;
            *self.image.write().map_err(|_| VaultError::Io)? = next;
            Ok(version)
        }
        .await;
        plaintext.zeroize();
        result
    }

    pub fn get_exact(
        &self,
        identity: &ChildVaultIdentity,
    ) -> Result<(ChildOAuthCredentials, u64), VaultError> {
        let image = self.image.read().map_err(|_| VaultError::Io)?;
        let record = image
            .records
            .get(&identity.lobby_id)
            .ok_or(VaultError::Missing)?;
        if &record.identity != identity {
            return Err(VaultError::Conflict);
        }
        let nonce = base64url_decode(&record.nonce)
            .filter(|value| value.len() == NONCE_BYTES)
            .ok_or(VaultError::Invalid)?;
        let ciphertext = base64url_decode(&record.ciphertext).ok_or(VaultError::Invalid)?;
        let cipher = ChaCha20Poly1305::new_from_slice(self.key.as_ref().as_slice())
            .map_err(|_| VaultError::Crypto)?;
        let mut plaintext = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &identity.aad(record.version),
                },
            )
            .map_err(|_| VaultError::Crypto)?;
        let decoded = decode_plaintext(&plaintext).map(|(id, secret)| {
            (
                ChildOAuthCredentials::new(id.as_str(), secret.as_str()),
                record.version,
            )
        });
        plaintext.zeroize();
        decoded
    }

    pub async fn delete_cas(
        &self,
        identity: &ChildVaultIdentity,
        version: u64,
    ) -> Result<(), VaultError> {
        let _mutation = self.mutation_lock.lock().await;
        let mut next = self.image.read().map_err(|_| VaultError::Io)?.clone();
        let record = next
            .records
            .get(&identity.lobby_id)
            .ok_or(VaultError::Missing)?;
        if &record.identity != identity || record.version != version {
            return Err(VaultError::Conflict);
        }
        next.records.remove(&identity.lobby_id);
        next.erasures.insert(
            identity.lobby_id,
            ErasureReceipt {
                identity: identity.clone(),
                version,
            },
        );
        self.persist(&next).await?;
        let readback = self.read_image_from_disk().await?;
        if readback.records.contains_key(&identity.lobby_id)
            || readback.erasures.get(&identity.lobby_id)
                != Some(&ErasureReceipt {
                    identity: identity.clone(),
                    version,
                })
        {
            return Err(VaultError::Conflict);
        }
        *self.image.write().map_err(|_| VaultError::Io)? = readback;
        Ok(())
    }

    /// Verifies a prior exact CAS erase after a crash between vault and lobby commits.
    pub async fn verify_erased(&self, identity: &ChildVaultIdentity) -> Result<u64, VaultError> {
        let _mutation = self.mutation_lock.lock().await;
        let readback = self.read_image_from_disk().await?;
        if readback.records.contains_key(&identity.lobby_id) {
            return Err(VaultError::Conflict);
        }
        let receipt = readback
            .erasures
            .get(&identity.lobby_id)
            .filter(|receipt| &receipt.identity == identity)
            .ok_or(VaultError::Missing)?;
        let version = receipt.version;
        *self.image.write().map_err(|_| VaultError::Io)? = readback;
        Ok(version)
    }

    async fn read_image_from_disk(&self) -> Result<VaultImage, VaultError> {
        let bytes = tokio::fs::read(self.path.as_ref())
            .await
            .map_err(|_| VaultError::Io)?;
        let image: VaultImage = serde_json::from_slice(&bytes).map_err(|_| VaultError::Invalid)?;
        if image.schema_version != VAULT_SCHEMA {
            return Err(VaultError::Invalid);
        }
        Ok(image)
    }

    async fn persist(&self, image: &VaultImage) -> Result<(), VaultError> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|_| VaultError::Io)?;
        }
        let bytes = serde_json::to_vec(image).map_err(|_| VaultError::Invalid)?;
        let temporary = self.path.with_extension("tmp");
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await.map_err(|_| VaultError::Io)?;
        file.write_all(&bytes).await.map_err(|_| VaultError::Io)?;
        file.sync_all().await.map_err(|_| VaultError::Io)?;
        drop(file);
        #[cfg(windows)]
        if tokio::fs::try_exists(self.path.as_ref())
            .await
            .map_err(|_| VaultError::Io)?
        {
            // Never delete the last durable custody image to emulate replace.
            // Windows activation remains fail-closed until atomic replacement
            // with directory durability is implemented.
            return Err(VaultError::Io);
        }
        tokio::fs::rename(&temporary, self.path.as_ref())
            .await
            .map_err(|_| VaultError::Io)?;
        #[cfg(unix)]
        {
            let parent = self
                .path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            tokio::fs::File::open(parent)
                .await
                .map_err(|_| VaultError::Io)?
                .sync_all()
                .await
                .map_err(|_| VaultError::Io)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_writer_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(lock_path)?;
    file.try_lock_exclusive()?;
    Ok(file)
}

#[cfg(unix)]
async fn read_key_file(path: &Path) -> Result<[u8; 32], VaultError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|_| VaultError::Io)?;
        let mode = metadata.permissions().mode();
        // Kubernetes fsGroup-mounted Secrets need group-read. World access and
        // group write/execute remain forbidden.
        if mode & 0o007 != 0 || mode & 0o030 != 0 {
            return Err(VaultError::Invalid);
        }
    }
    let mut bytes = tokio::fs::read(path).await.map_err(|_| VaultError::Io)?;
    let result = if bytes.len() == 32 {
        let mut key = [0_u8; 32];
        key.copy_from_slice(&bytes);
        Ok(key)
    } else {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| VaultError::Invalid)?
            .trim();
        if text.len() != 64 {
            Err(VaultError::Invalid)
        } else {
            let mut key = [0_u8; 32];
            for (index, chunk) in text.as_bytes().chunks_exact(2).enumerate() {
                let value = std::str::from_utf8(chunk)
                    .ok()
                    .and_then(|v| u8::from_str_radix(v, 16).ok())
                    .ok_or(VaultError::Invalid)?;
                key[index] = value;
            }
            Ok(key)
        }
    };
    bytes.zeroize();
    result
}

fn encode_plaintext(
    client_id: &str,
    client_secret: &str,
) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    let id_len = u32::try_from(client_id.len()).map_err(|_| VaultError::Invalid)?;
    let secret_len = u32::try_from(client_secret.len()).map_err(|_| VaultError::Invalid)?;
    let mut output = Zeroizing::new(Vec::with_capacity(
        8 + client_id.len() + client_secret.len(),
    ));
    output.extend_from_slice(&id_len.to_be_bytes());
    output.extend_from_slice(client_id.as_bytes());
    output.extend_from_slice(&secret_len.to_be_bytes());
    output.extend_from_slice(client_secret.as_bytes());
    Ok(output)
}

fn decode_plaintext(bytes: &[u8]) -> Result<(Zeroizing<String>, Zeroizing<String>), VaultError> {
    if bytes.len() < 8 {
        return Err(VaultError::Invalid);
    }
    let id_len =
        u32::from_be_bytes(bytes[0..4].try_into().map_err(|_| VaultError::Invalid)?) as usize;
    let secret_offset = 4_usize.checked_add(id_len).ok_or(VaultError::Invalid)?;
    let secret_len_end = secret_offset.checked_add(4).ok_or(VaultError::Invalid)?;
    if secret_len_end > bytes.len() {
        return Err(VaultError::Invalid);
    }
    let secret_len = u32::from_be_bytes(
        bytes[secret_offset..secret_len_end]
            .try_into()
            .map_err(|_| VaultError::Invalid)?,
    ) as usize;
    let end = secret_len_end
        .checked_add(secret_len)
        .ok_or(VaultError::Invalid)?;
    if end != bytes.len() {
        return Err(VaultError::Invalid);
    }
    let id =
        String::from_utf8(bytes[4..secret_offset].to_vec()).map_err(|_| VaultError::Invalid)?;
    let secret =
        String::from_utf8(bytes[secret_len_end..end].to_vec()).map_err(|_| VaultError::Invalid)?;
    Ok((Zeroizing::new(id), Zeroizing::new(secret)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn vault_persists_only_authenticated_ciphertext_and_cas_deletes() {
        let root = std::env::temp_dir().join(format!("spurfire-vault-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let key_path = root.join("key");
        tokio::fs::write(&key_path, [7_u8; 32]).await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
        let path = root.join("vault.json");
        let vault = EncryptedChildVault::open(&path, &key_path).await.unwrap();
        let identity = ChildVaultIdentity {
            lobby_id: LobbyId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
            network_generation: 3,
            provider_tailnet_id: "TtVaultCNTRL".into(),
            tailnet_dns_name: TailnetDnsName::parse("vault-test.ts.net").unwrap(),
        };
        let version = vault
            .put_if_absent(
                identity.clone(),
                ChildOAuthCredentials::new("child-id-canary", "child-secret-canary"),
            )
            .await
            .unwrap();
        let disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(!disk.contains("child-id-canary"));
        assert!(!disk.contains("child-secret-canary"));
        assert!(matches!(
            EncryptedChildVault::open(&path, &key_path).await,
            Err(VaultError::Io)
        ));
        drop(vault);
        let reopened = EncryptedChildVault::open(&path, &key_path).await.unwrap();
        let (credentials, loaded_version) = reopened.get_exact(&identity).unwrap();
        assert_eq!(loaded_version, version);
        assert!(!format!("{credentials:?}").contains("canary"));
        reopened.delete_cas(&identity, version).await.unwrap();
        assert!(matches!(
            reopened.get_exact(&identity),
            Err(VaultError::Missing)
        ));
        assert_eq!(reopened.verify_erased(&identity).await.unwrap(), version);
        drop(reopened);
        let recovered = EncryptedChildVault::open(&path, &key_path).await.unwrap();
        assert_eq!(recovered.verify_erased(&identity).await.unwrap(), version);

        let unknown = ChildVaultIdentity {
            lobby_id: LobbyId::parse("00000000-0000-4000-8000-000000000002").unwrap(),
            ..identity
        };
        assert!(matches!(
            recovered.verify_erased(&unknown).await,
            Err(VaultError::Missing)
        ));
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn unsupported_platform_refuses_to_open_vault() {
        assert!(matches!(
            EncryptedChildVault::open("unused-vault", "unused-key").await,
            Err(VaultError::Invalid)
        ));
    }
}
