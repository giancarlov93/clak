//! Where connector tokens and API secrets live.
//!
//! The two OSes keep them differently. On Windows the box only encrypts (DPAPI,
//! user scope), and the encrypted blobs live in a JSON file next to the
//! configuration: `connectors.secrets`, `apis.secrets`, each one
//! `{ "<id>": "<base64 of the blob>" }`. On the Mac there is no file: each secret is a
//! generic password in the Keychain, looked up by *name* (the service
//! `dev.clak.connector` and the id as the account).
//!
//! The common contract is the Mac's, by name: [`SecretBox`] reads, checks and writes
//! a secret given its id. On Windows it's implemented by [`FileBox`], which combines
//! the secrets file and a blob cipher ([`BlobCipher`]), that is [`Dpapi`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// An error to show the user as is: the Keychain saying no, a file that can't be
/// written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretError(pub String);

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretError {}

/// Where secrets live, by name. An id is the connector's (`cn_…`), its OAuth
/// sign-in's (`cn_…#oauth`) or the API's (`api_…`).
pub trait SecretBox: Send + Sync {
    /// The plaintext secret, or None if it's missing or can't be decrypted (a file copied from another user).
    fn secret(&self, id: &str) -> Option<String>;
    /// Whether it's there. For the Keychain without reading it: no need to ask the user anything.
    fn has_secret(&self, id: &str) -> bool;
    /// None removes it. `label` is what the user reads next to the item, where the
    /// OS shows one (Keychain Access); ignored elsewhere.
    fn set_secret(&self, id: &str, secret: Option<&str>, label: &str) -> Result<(), SecretError>;
}

/// Encrypts and decrypts blobs: the OS-dependent part of the box, when the secrets
/// live in a file.
pub trait BlobCipher: Send + Sync {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, SecretError>;
    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError>;
}

/// The secrets in a JSON file, `{ "<id>": "<base64 of the encrypted blob>" }`: written
/// indented, in UTF-8 without BOM, through a `.tmp`.
///
/// A missing or unreadable file is an empty box. The lock covers accesses from the
/// same process; two hubs on the same file can't happen (the lockfile prevents it).
pub struct FileBox<C: BlobCipher> {
    path: PathBuf,
    cipher: C,
    gate: Mutex<()>,
}

impl<C: BlobCipher> FileBox<C> {
    pub fn new(path: impl Into<PathBuf>, cipher: C) -> Self {
        Self {
            path: path.into(),
            cipher,
            gate: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> serde_json::Map<String, serde_json::Value> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(strip_bom(&bytes)).ok())
            .and_then(|value| match value {
                serde_json::Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn write(&self, all: &serde_json::Map<String, serde_json::Value>) -> Result<(), SecretError> {
        let failed =
            |e: std::io::Error| SecretError(format!("Couldn't write {}: {e}", self.path.display()));
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        let text = serde_json::to_string_pretty(all).map_err(|e| SecretError(e.to_string()))?;
        let mut draft = self.path.clone().into_os_string();
        draft.push(".tmp");
        std::fs::write(&draft, text).map_err(failed)?;
        std::fs::rename(&draft, &self.path).map_err(failed)
    }
}

/// A leading BOM is tolerated: some editors add one.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}

impl<C: BlobCipher> SecretBox for FileBox<C> {
    fn secret(&self, id: &str) -> Option<String> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let encoded = self.load().get(id)?.as_str()?.to_string();
        let sealed = BASE64.decode(encoded.trim()).ok()?;
        String::from_utf8(self.cipher.unprotect(&sealed).ok()?).ok()
    }

    /// It's there if it decrypts.
    fn has_secret(&self, id: &str) -> bool {
        self.secret(id).is_some()
    }

    fn set_secret(&self, id: &str, secret: Option<&str>, _label: &str) -> Result<(), SecretError> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        match secret {
            None => {
                all.remove(id);
            }
            Some(secret) => {
                let sealed = self.cipher.protect(secret.as_bytes())?;
                all.insert(
                    id.to_string(),
                    serde_json::Value::String(BASE64.encode(sealed)),
                );
            }
        }
        self.write(&all)
    }
}

/// Test secrets: in memory, never in the Keychain or the user's files.
#[derive(Default)]
pub struct MemoryBox {
    secrets: Mutex<HashMap<String, String>>,
}

impl MemoryBox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything inside, to search it for a secret that mustn't be there.
    pub fn all(&self) -> HashMap<String, String> {
        self.secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl SecretBox for MemoryBox {
    fn secret(&self, id: &str) -> Option<String> {
        self.secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    fn has_secret(&self, id: &str) -> bool {
        self.secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    fn set_secret(&self, id: &str, secret: Option<&str>, _label: &str) -> Result<(), SecretError> {
        let mut all = self.secrets.lock().unwrap_or_else(|e| e.into_inner());
        match secret {
            Some(secret) => all.insert(id.to_string(), secret.to_string()),
            None => all.remove(id),
        };
        Ok(())
    }
}

/// Where an OS without a vault we know (Linux, for now) keeps secrets: nowhere.
/// It reads empty, and writing is an error.
pub struct NoBox;

impl SecretBox for NoBox {
    fn secret(&self, _id: &str) -> Option<String> {
        None
    }

    fn has_secret(&self, _id: &str) -> bool {
        false
    }

    fn set_secret(&self, _id: &str, secret: Option<&str>, _label: &str) -> Result<(), SecretError> {
        match secret {
            None => Ok(()),
            Some(_) => Err(SecretError(
                "This system has no secret store: tokens can't be saved".into(),
            )),
        }
    }
}

// --- Windows: DPAPI ----------------------------------------------------------------

/// DPAPI with user scope (`CryptProtectData`) and Clak's own entropy.
///
/// Protects against someone reading the file as another user or from another computer,
/// not against a program already running as the user.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Dpapi;

/// The Windows box: a file of secrets encrypted with DPAPI.
#[cfg(windows)]
pub type DpapiBox = FileBox<Dpapi>;

#[cfg(windows)]
impl DpapiBox {
    /// The secrets in the file `path` (for example `~/.clak/connectors.secrets`).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        FileBox::new(path, Dpapi)
    }
}

#[cfg(windows)]
impl BlobCipher for Dpapi {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, SecretError> {
        crate::win::dpapi(plain, true)
    }

    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
        crate::win::dpapi(sealed, false)
    }
}

// --- Mac: Keychain -----------------------------------------------------------------

/// The user's login Keychain, one generic password per secret: service
/// [`KeychainBox::SERVICE`], account the id (`cn_…`, `cn_…#oauth`, `api_…`).
///
/// Protects against someone copying the files in the hub's folder, and against another
/// app asking for it: the OS asks the user whether to let it read. Not against a
/// program the user says yes to. With the ad hoc signature, every build is a different
/// app to the Keychain: after an update the OS may ask again whether to let it read
/// the tokens ("Always Allow").
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Default)]
pub struct KeychainBox;

#[cfg(target_os = "macos")]
impl KeychainBox {
    pub const SERVICE: &'static str = "dev.clak.connector";
}

#[cfg(target_os = "macos")]
impl SecretBox for KeychainBox {
    fn secret(&self, id: &str) -> Option<String> {
        crate::mac::keychain_read(KeychainBox::SERVICE, id)
    }

    fn has_secret(&self, id: &str) -> bool {
        crate::mac::keychain_has(KeychainBox::SERVICE, id)
    }

    fn set_secret(&self, id: &str, secret: Option<&str>, label: &str) -> Result<(), SecretError> {
        crate::mac::keychain_write(KeychainBox::SERVICE, id, secret, label)
    }
}

/// The OS box for a store's secrets: on Windows the file `file` in the hub's folder
/// (`connectors.secrets` for connectors, `apis.secrets` for APIs), encrypted with
/// DPAPI; on the Mac the Keychain, which is a single one for all (the ids don't
/// overlap) and doesn't use the file name.
pub fn default_box(home: &Path, file: &str) -> Box<dyn SecretBox> {
    #[cfg(windows)]
    {
        Box::new(DpapiBox::open(home.join(file)))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (home, file);
        Box::new(KeychainBox)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (home, file);
        Box::new(NoBox)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake cipher, to test the file without the OS: it reverses the bytes.
    struct Reverse;

    impl BlobCipher for Reverse {
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, SecretError> {
            Ok(plain.iter().rev().copied().collect())
        }
        fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
            if sealed.first() == Some(&b'!') {
                return Err(SecretError("broken".into()));
            }
            Ok(sealed.iter().rev().copied().collect())
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clak-platform-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn il_file_dei_segreti_tiene_i_blob_in_base64() {
        let dir = temp("filebox");
        let path = dir.join("connectors.secrets");
        let secrets = FileBox::new(&path, Reverse);
        assert_eq!(secrets.secret("cn_1"), None);
        secrets
            .set_secret("cn_1", Some("abc"), "Clak: uno")
            .unwrap();
        secrets
            .set_secret("cn_1#oauth", Some("{\"a\":1}"), "")
            .unwrap();
        assert_eq!(secrets.secret("cn_1").as_deref(), Some("abc"));
        assert!(secrets.has_secret("cn_1#oauth"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "{\n  \"cn_1\": \"Y2Jh\",\n  \"cn_1#oauth\": \"fTE6ImEiew==\"\n}"
        );
        assert!(!dir.join("connectors.secrets.tmp").exists());

        secrets.set_secret("cn_1", None, "").unwrap();
        assert_eq!(secrets.secret("cn_1"), None);
        assert!(!secrets.has_secret("cn_1"));

        // A blob that can't be decrypted (copied from another user) is a missing secret.
        std::fs::write(
            &path,
            "\u{FEFF}{\"cn_2\": \"IXg=\", \"cn_3\": 5, \"cn_4\": \"%%%\"}",
        )
        .unwrap();
        assert_eq!(secrets.secret("cn_2"), None);
        assert_eq!(secrets.secret("cn_3"), None);
        assert_eq!(secrets.secret("cn_4"), None);
        // A corrupt file is an empty box, and the next write rebuilds it.
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(secrets.secret("cn_2"), None);
        secrets.set_secret("api_1", Some("k"), "").unwrap();
        assert_eq!(secrets.secret("api_1").as_deref(), Some("k"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn la_scatola_in_memoria() {
        let secrets = MemoryBox::new();
        secrets.set_secret("cn_1", Some("x"), "").unwrap();
        assert_eq!(secrets.secret("cn_1").as_deref(), Some("x"));
        assert!(secrets.has_secret("cn_1"));
        assert_eq!(secrets.all().len(), 1);
        secrets.set_secret("cn_1", None, "").unwrap();
        assert!(secrets.all().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_cifra_e_decifra_per_l_utente() {
        let sealed = Dpapi.protect(b"segreto \xC3\xA8").unwrap();
        assert_ne!(sealed, b"segreto \xC3\xA8");
        assert_eq!(Dpapi.unprotect(&sealed).unwrap(), b"segreto \xC3\xA8");
        assert!(Dpapi.unprotect(b"not a blob").is_err());

        let dir = temp("dpapi");
        let secrets = default_box(&dir, "apis.secrets");
        secrets.set_secret("api_1", Some("k123"), "").unwrap();
        assert_eq!(secrets.secret("api_1").as_deref(), Some("k123"));
        assert!(
            std::fs::read_to_string(dir.join("apis.secrets"))
                .unwrap()
                .contains("\"api_1\": \"AQAAANCMnd8BFdERjHoAwE/Cl+s")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
