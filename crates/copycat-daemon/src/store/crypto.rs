//! Payload encryption and the three key-storage modes (ADR-013).
//!
//! Clipboard history holds credentials, tokens, private messages, and customer
//! data. It is not a cache. The key lives in the OS keyring where there is one;
//! where there is not — servers, containers, minimal desktops, CI — the daemon
//! says so rather than either refusing to run or pretending.

use std::path::Path;

use anyhow::{Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::Rng;

const KEYRING_SERVICE: &str = "copycat";
const KEYRING_USER: &str = "payload-key";
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;

/// Where the master key came from. Reported verbatim by `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStorage {
    /// The OS keyring. The only mode considered fully protected.
    Keyring,
    /// A `0600` file in the data directory, because no keyring was reachable.
    KeyFile,
    /// No key at all: nothing is persisted this session.
    MemoryOnly,
}

impl KeyStorage {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyStorage::Keyring => "keyring",
            KeyStorage::KeyFile => "key-file (degraded)",
            KeyStorage::MemoryOnly => "memory-only (persistence disabled)",
        }
    }

    pub fn is_degraded(self) -> bool {
        self != KeyStorage::Keyring
    }
}

pub struct PayloadCipher {
    cipher: XChaCha20Poly1305,
    storage: KeyStorage,
}

impl std::fmt::Debug for PayloadCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never let the key reach a log through a derived Debug.
        f.debug_struct("PayloadCipher").field("storage", &self.storage).finish()
    }
}

impl PayloadCipher {
    /// Acquire the master key, in the order ADR-013 requires.
    ///
    /// Returns `None` when there is no keyring and no permitted fallback: the
    /// caller then runs without persistence, which is a documented mode rather
    /// than a failure.
    pub fn open(key_file: &Path, allow_key_file_fallback: bool) -> (Option<Self>, KeyStorage) {
        match keyring_key() {
            Ok(key) => {
                return (
                    Some(PayloadCipher {
                        cipher: XChaCha20Poly1305::new(&key.into()),
                        storage: KeyStorage::Keyring,
                    }),
                    KeyStorage::Keyring,
                );
            }
            Err(error) => {
                tracing::debug!(error = %error, "os keyring unavailable");
            }
        }

        if !allow_key_file_fallback {
            return (None, KeyStorage::MemoryOnly);
        }

        match file_key(key_file) {
            Ok(key) => (
                Some(PayloadCipher {
                    cipher: XChaCha20Poly1305::new(&key.into()),
                    storage: KeyStorage::KeyFile,
                }),
                KeyStorage::KeyFile,
            ),
            Err(error) => {
                tracing::warn!(error = %error, "key file unusable; running without persistence");
                (None, KeyStorage::MemoryOnly)
            }
        }
    }

    /// Build a cipher over a caller-supplied key. For tests.
    #[cfg(test)]
    pub fn from_key(key: [u8; KEY_BYTES]) -> Self {
        PayloadCipher {
            cipher: XChaCha20Poly1305::new(&key.into()),
            storage: KeyStorage::KeyFile,
        }
    }

    /// A fresh 192-bit nonce per payload (§9.2). Never derived, never reused.
    pub fn seal(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut nonce_bytes = [0u8; NONCE_BYTES];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| anyhow::anyhow!("payload encryption failed"))?;
        Ok((nonce_bytes.to_vec(), ciphertext))
    }

    pub fn unseal(&self, nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
        let nonce: [u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| anyhow::anyhow!("stored nonce is {} bytes, expected {NONCE_BYTES}", nonce.len()))?;
        self.cipher
            .decrypt(&XNonce::from(nonce), ciphertext)
            // The failure could be a wrong key or a tampered row; either way the
            // detail is not something to guess at in a message.
            .map_err(|_| anyhow::anyhow!("payload failed authentication"))
    }
}

fn random_key() -> [u8; KEY_BYTES] {
    let mut key = [0u8; KEY_BYTES];
    rand::rng().fill_bytes(&mut key);
    key
}

fn keyring_key() -> Result<[u8; KEY_BYTES]> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .context("opening the keyring entry")?;

    match entry.get_password() {
        Ok(encoded) => decode_key(&encoded).context("the stored key is malformed"),
        Err(keyring::Error::NoEntry) => {
            let key = random_key();
            entry.set_password(&encode_key(&key)).context("storing a new key")?;
            Ok(key)
        }
        Err(e) => Err(e).context("reading the stored key"),
    }
}

fn file_key(path: &Path) -> Result<[u8; KEY_BYTES]> {
    if let Some(parent) = path.parent() {
        crate::paths::create_private_dir(parent)?;
        crate::paths::verify_private_dir(parent)?;
    }

    match std::fs::read(path) {
        Ok(bytes) => {
            verify_key_file_mode(path)?;
            bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("{} is not a {KEY_BYTES}-byte key", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = random_key();
            write_private_file(path, &key)?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    // Created 0600 by `mode`, so there is no window in which the key exists
    // with looser permissions.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("creating {}", path.display()))
}

#[cfg(unix)]
fn verify_key_file_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        anyhow::bail!("{} is mode {mode:04o}; it must be 0600", path.display());
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_key_file_mode(_path: &Path) -> Result<()> {
    Ok(())
}

fn encode_key(key: &[u8; KEY_BYTES]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_key(encoded: &str) -> Result<[u8; KEY_BYTES]> {
    if encoded.len() != KEY_BYTES * 2 {
        anyhow::bail!("expected {} hex characters", KEY_BYTES * 2);
    }
    let mut key = [0u8; KEY_BYTES];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16)?;
    }
    Ok(key)
}

/// The `Key` import exists to pin the key length at compile time.
const _: () = assert!(KEY_BYTES == 32);
type _KeyCheck = Key;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_payloads_round_trip() {
        let cipher = PayloadCipher::from_key([7u8; 32]);
        let (nonce, ciphertext) = cipher.seal(b"postgres://user:pw@host").unwrap();
        assert_eq!(cipher.unseal(&nonce, &ciphertext).unwrap(), b"postgres://user:pw@host");
    }

    #[test]
    fn every_seal_uses_a_fresh_nonce() {
        let cipher = PayloadCipher::from_key([7u8; 32]);
        let (first, first_ct) = cipher.seal(b"same").unwrap();
        let (second, second_ct) = cipher.seal(b"same").unwrap();
        assert_ne!(first, second, "nonce reuse would leak equality of payloads");
        assert_ne!(first_ct, second_ct);
    }

    #[test]
    fn a_tampered_ciphertext_is_rejected() {
        let cipher = PayloadCipher::from_key([7u8; 32]);
        let (nonce, mut ciphertext) = cipher.seal(b"balance: 100").unwrap();
        ciphertext[0] ^= 0x01;
        assert!(cipher.unseal(&nonce, &ciphertext).is_err());
    }

    #[test]
    fn another_key_cannot_read_the_payload() {
        let (nonce, ciphertext) = PayloadCipher::from_key([1u8; 32]).seal(b"secret").unwrap();
        assert!(PayloadCipher::from_key([2u8; 32]).unseal(&nonce, &ciphertext).is_err());
    }

    #[test]
    fn debug_output_never_carries_the_key() {
        let rendered = format!("{:?}", PayloadCipher::from_key([9u8; 32]));
        assert!(!rendered.contains('9') || !rendered.contains("cipher"), "{rendered}");
        assert!(rendered.contains("storage"));
    }

    /// The daemon refuses a data directory other users can enter, and a temp
    /// directory inherits the umask, so tests have to set this up explicitly.
    #[cfg(unix)]
    fn private_tempdir() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn a_generated_key_file_is_created_unreadable_to_others() {
        use std::os::unix::fs::PermissionsExt;
        let dir = private_tempdir();
        let path = dir.path().join("payload.key");

        let key = file_key(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file must not be readable by anyone else");
        assert_eq!(file_key(&path).unwrap(), key, "the key is stable across opens");
    }

    #[cfg(unix)]
    #[test]
    fn a_loosely_permissioned_key_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = private_tempdir();
        let path = dir.path().join("payload.key");
        file_key(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(file_key(&path).is_err());
    }

    #[test]
    fn key_encoding_round_trips() {
        let key = random_key();
        assert_eq!(decode_key(&encode_key(&key)).unwrap(), key);
        assert!(decode_key("short").is_err());
    }
}
