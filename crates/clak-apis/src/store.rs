//! The APIs added by the user: `apis.json` in the hub folder, next to `connectors.json`,
//! and the secrets (tokens, keys, passwords, the whole OAuth access) in the secret box of
//! `clak-platform`: on Windows `apis.secrets`, encrypted with DPAPI; on the Mac the
//! Keychain, one item per API. As
//! with connectors, a secret leaves here only toward the API it belongs to.

use crate::model::ApiEntry;
use anyhow::Context as _;
use clak_platform::SecretBox;
use clak_wire::json;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// `apis.json` and the secrets, read and written one at a time.
pub struct ApiStore {
    home: PathBuf,
    secrets: Arc<dyn SecretBox>,
    gate: Mutex<()>,
}

impl ApiStore {
    pub fn new(home: impl Into<PathBuf>, secrets: Arc<dyn SecretBox>) -> ApiStore {
        ApiStore {
            home: home.into(),
            secrets,
            gate: Mutex::new(()),
        }
    }

    /// The hub's store, with the system box: `apis.secrets` (DPAPI) on Windows, the
    /// Keychain on the Mac.
    pub fn open(home: &Path) -> ApiStore {
        ApiStore::new(
            home,
            Arc::from(clak_platform::default_box(home, "apis.secrets")),
        )
    }

    fn list_path(&self) -> PathBuf {
        self.home.join("apis.json")
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        // A panic halfway through a write leaves nothing half-done: the file is replaced whole.
        self.gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn all(&self) -> Vec<ApiEntry> {
        let _gate = self.lock();
        self.load()
    }

    pub fn get(&self, id: &str) -> Option<ApiEntry> {
        let _gate = self.lock();
        self.load().into_iter().find(|a| a.id == id)
    }

    /// A new API, with its secret (none or empty: no secret).
    pub fn add(&self, entry: ApiEntry, secret: Option<&str>) -> anyhow::Result<ApiEntry> {
        let _gate = self.lock();
        let mut all = self.load();
        all.push(entry.clone());
        self.save(&all)?;
        self.write_secret(&entry.id, secret, &entry.name)?;
        Ok(entry)
    }

    pub fn update(&self, entry: &ApiEntry) -> anyhow::Result<()> {
        let _gate = self.lock();
        let all: Vec<ApiEntry> = self
            .load()
            .into_iter()
            .map(|a| if a.id == entry.id { entry.clone() } else { a })
            .collect();
        self.save(&all)
    }

    /// Removes the API and its secret.
    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        let _gate = self.lock();
        let all: Vec<ApiEntry> = self.load().into_iter().filter(|a| a.id != id).collect();
        self.save(&all)?;
        self.write_secret(id, None, "")
    }

    /// The secret in plain text, or nothing if it's missing or can't be decrypted.
    pub fn secret(&self, id: &str) -> Option<String> {
        self.secrets.secret(id)
    }

    /// Whether it exists, without reading it (the Keychain asks the user nothing).
    pub fn has_secret(&self, id: &str) -> bool {
        self.secrets.has_secret(id)
    }

    /// None or empty removes it.
    pub fn set_secret(&self, id: &str, secret: Option<&str>) -> anyhow::Result<()> {
        let _gate = self.lock();
        let name = self
            .load()
            .into_iter()
            .find(|a| a.id == id)
            .map_or_else(|| id.to_owned(), |a| a.name);
        self.write_secret(id, secret, &name)
    }

    fn write_secret(&self, id: &str, secret: Option<&str>, name: &str) -> anyhow::Result<()> {
        let secret = secret.filter(|s| !s.is_empty());
        self.secrets
            .set_secret(id, secret, &format!("Clak: {name} (API)"))
            .map_err(|e| anyhow::anyhow!(e.0))
            .context("the secret can't be saved")
    }

    fn load(&self) -> Vec<ApiEntry> {
        read(&self.list_path())
            .as_ref()
            .and_then(|file| file.get("apis"))
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(ApiEntry::from_json).collect())
            .unwrap_or_default()
    }

    fn save(&self, entries: &[ApiEntry]) -> anyhow::Result<()> {
        let mut file = Map::new();
        file.insert(
            "apis".into(),
            Value::Array(entries.iter().map(ApiEntry::to_json).collect()),
        );
        write(&self.list_path(), &Value::Object(file))
    }
}

/// A JSON file, or nothing if it's missing or unreadable.
fn read(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let bytes = bytes.strip_prefix("\u{FEFF}".as_bytes()).unwrap_or(&bytes);
    json::parse(bytes).ok()
}

/// Writes alongside, then replaces: there's never a half-written file. On macOS, user-only.
fn write(path: &Path, content: &Value) -> anyhow::Result<()> {
    let folder = path.parent().context("no folder")?;
    std::fs::create_dir_all(folder)
        .with_context(|| format!("Can't create {}.", folder.display()))?;
    let draft = path.with_extension("json.tmp");
    std::fs::write(&draft, json::encode_pretty(content))
        .with_context(|| format!("Can't write {}.", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&draft, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&draft, path).with_context(|| format!("Can't write {}.", path.display()))?;
    Ok(())
}
