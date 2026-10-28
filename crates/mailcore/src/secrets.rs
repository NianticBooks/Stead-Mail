//! Secret storage. Primary backend is the OS keyring (Secret Service on Linux, Keychain on
//! macOS). Fallback is a passphrase-encrypted file (ChaCha20-Poly1305, Argon2id KDF) used when
//! no keyring daemon is reachable or when `MAILCORE_SECRETS=file` is set.

use std::path::{Path, PathBuf};

use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const SERVICE: &str = "mailcore";

pub trait SecretStore: Send + Sync {
    fn backend_name(&self) -> &'static str;
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

// ---------------------------------------------------------------- keyring backend

pub struct KeyringStore;

impl KeyringStore {
    /// Probe the keyring by writing and deleting a canary entry.
    pub fn probe() -> Result<KeyringStore> {
        let store = KeyringStore;
        let canary = format!("probe/{}", uuid::Uuid::new_v4());
        store.set(&canary, "ok")?;
        let back = store.get(&canary)?;
        store.delete(&canary)?;
        if back.as_deref() == Some("ok") {
            Ok(store)
        } else {
            Err(Error::Secrets("keyring probe readback mismatch".into()))
        }
    }

    fn entry(key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, key).map_err(|e| Error::Secrets(e.to_string()))
    }
}

impl SecretStore for KeyringStore {
    fn backend_name(&self) -> &'static str {
        "os-keyring"
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        match Self::entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Secrets(e.to_string())),
        }
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        Self::entry(key)?.set_password(value).map_err(|e| Error::Secrets(e.to_string()))
    }
    fn delete(&self, key: &str) -> Result<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Secrets(e.to_string())),
        }
    }
}

// ---------------------------------------------------------------- encrypted file backend

#[derive(Serialize, Deserialize, Default)]
struct FileContents {
    /// Argon2id salt, 16 bytes, hex.
    salt: String,
    /// ChaCha20-Poly1305 nonce, 12 bytes, hex.
    nonce: String,
    /// Ciphertext of a JSON `BTreeMap<String, String>`, hex.
    ciphertext: String,
}

pub struct EncryptedFileStore {
    path: PathBuf,
    key: [u8; 32],
    salt: [u8; 16],
}

impl EncryptedFileStore {
    /// Open or create the file. The passphrase is taken from `MAILCORE_PASSPHRASE` or prompted.
    pub fn open(path: &Path) -> Result<EncryptedFileStore> {
        let passphrase = match std::env::var("MAILCORE_PASSPHRASE") {
            Ok(p) if !p.is_empty() => p,
            _ => rpassword::prompt_password("mailcore secrets passphrase: ")
                .map_err(|e| Error::Secrets(format!("cannot read passphrase: {e}")))?,
        };
        Self::open_with_passphrase(path, &passphrase)
    }

    pub fn open_with_passphrase(path: &Path, passphrase: &str) -> Result<EncryptedFileStore> {
        let salt: [u8; 16] = if path.exists() {
            let f: FileContents = serde_json::from_slice(&std::fs::read(path)?)
                .map_err(|e| Error::Secrets(format!("corrupt secrets file: {e}")))?;
            hex_to_array(&f.salt)?
        } else {
            let mut s = [0u8; 16];
            getrandom::fill(&mut s).map_err(|e| Error::Secrets(e.to_string()))?;
            s
        };
        let mut key = [0u8; 32];
        Argon2::default()
            .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
            .map_err(|e| Error::Secrets(format!("kdf: {e}")))?;
        let store = EncryptedFileStore { path: path.to_path_buf(), key, salt };
        // Validate the passphrase early by decrypting (or creating) the file.
        let map = store.read_map()?;
        if !path.exists() {
            store.write_map(&map)?;
        }
        Ok(store)
    }

    fn read_map(&self) -> Result<std::collections::BTreeMap<String, String>> {
        if !self.path.exists() {
            return Ok(Default::default());
        }
        let f: FileContents = serde_json::from_slice(&std::fs::read(&self.path)?)
            .map_err(|e| Error::Secrets(format!("corrupt secrets file: {e}")))?;
        let nonce: [u8; 12] = hex_to_array(&f.nonce)?;
        let ct = hex_decode(&f.ciphertext)?;
        let cipher = ChaCha20Poly1305::new(&Key::from(self.key));
        let pt = cipher
            .decrypt(&Nonce::from(nonce), ct.as_ref())
            .map_err(|_| Error::Secrets("wrong passphrase or corrupt secrets file".into()))?;
        serde_json::from_slice(&pt).map_err(|e| Error::Secrets(format!("corrupt secrets payload: {e}")))
    }

    fn write_map(&self, map: &std::collections::BTreeMap<String, String>) -> Result<()> {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| Error::Secrets(e.to_string()))?;
        let cipher = ChaCha20Poly1305::new(&Key::from(self.key));
        let pt = serde_json::to_vec(map).map_err(|e| Error::Secrets(e.to_string()))?;
        let ct = cipher
            .encrypt(&Nonce::from(nonce), pt.as_ref())
            .map_err(|_| Error::Secrets("encrypt failed".into()))?;
        let f = FileContents { salt: hex_encode(&self.salt), nonce: hex_encode(&nonce), ciphertext: hex_encode(&ct) };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&f).map_err(|e| Error::Secrets(e.to_string()))?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl SecretStore for EncryptedFileStore {
    fn backend_name(&self) -> &'static str {
        "encrypted-file"
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.read_map()?.get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        let mut m = self.read_map()?;
        m.insert(key.to_string(), value.to_string());
        self.write_map(&m)
    }
    fn delete(&self, key: &str) -> Result<()> {
        let mut m = self.read_map()?;
        m.remove(key);
        self.write_map(&m)
    }
}

// ---------------------------------------------------------------- selection

/// Pick a backend: `MAILCORE_SECRETS=file|keyring` forces one; otherwise try the keyring and
/// fall back to the encrypted file in `data_dir`.
pub fn open_default(data_dir: &Path) -> Result<Box<dyn SecretStore>> {
    let file_path = data_dir.join("secrets.enc.json");
    match std::env::var("MAILCORE_SECRETS").as_deref() {
        Ok("file") => return Ok(Box::new(EncryptedFileStore::open(&file_path)?)),
        Ok("keyring") => return Ok(Box::new(KeyringStore::probe()?)),
        _ => {}
    }
    match KeyringStore::probe() {
        Ok(k) => Ok(Box::new(k)),
        Err(e) => {
            tracing::warn!("keyring unavailable ({e}); falling back to encrypted file {}", file_path.display());
            Ok(Box::new(EncryptedFileStore::open(&file_path)?))
        }
    }
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        return Err(Error::Secrets("bad hex".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| Error::Secrets("bad hex".into())))
        .collect()
}
fn hex_to_array<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v = hex_decode(s)?;
    v.try_into().map_err(|_| Error::Secrets("bad field length".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_file_roundtrip_and_wrong_passphrase() {
        let dir = std::env::temp_dir().join(format!("mailcore-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("secrets.enc.json");
        let s = EncryptedFileStore::open_with_passphrase(&path, "correct horse").unwrap();
        s.set("account/1/password", "app-specific-pw").unwrap();
        assert_eq!(s.get("account/1/password").unwrap().as_deref(), Some("app-specific-pw"));
        assert_eq!(s.get("missing").unwrap(), None);

        // Reopen with the right passphrase: data persists.
        let s2 = EncryptedFileStore::open_with_passphrase(&path, "correct horse").unwrap();
        assert_eq!(s2.get("account/1/password").unwrap().as_deref(), Some("app-specific-pw"));
        s2.delete("account/1/password").unwrap();
        assert_eq!(s2.get("account/1/password").unwrap(), None);

        // Wrong passphrase must fail closed.
        assert!(EncryptedFileStore::open_with_passphrase(&path, "wrong").is_err());

        // Plaintext never touches disk.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("app-specific-pw"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
