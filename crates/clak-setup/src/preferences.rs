//! The app's preferences, in a JSON file in the hub's folder (next to the shim chosen by
//! hand): `win.json` on Windows, `mac.json` on the Mac. They only concern how the app
//! presents itself and when it starts: none touches permissions, which are granted only in
//! the consent windows.

use crate::Result;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The preferences file, in the hub's folder.
pub const FILE_NAME: &str = if cfg!(windows) {
    "win.json"
} else {
    "mac.json"
};

/// The preferences file of this hub folder.
pub fn file(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

#[derive(Debug)]
pub struct Preferences {
    home: PathBuf,
    lock: Mutex<()>,
}

impl Preferences {
    pub fn new(home: impl Into<PathBuf>) -> Preferences {
        Preferences {
            home: home.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> PathBuf {
        file(&self.home)
    }

    /// A notice from the tray when a program connects, or an agent starts or stops using it.
    pub fn notifications(&self) -> bool {
        self.read("notifications").unwrap_or(true)
    }

    pub fn set_notifications(&self, value: bool) -> Result<()> {
        self.write("notifications", Some(value))
    }

    /// A sound when a question appears. The window appears anyway.
    pub fn consent_sound(&self) -> bool {
        self.read("consentSound").unwrap_or(true)
    }

    pub fn set_consent_sound(&self, value: bool) -> Result<()> {
        self.write("consentSound", Some(value))
    }

    /// The connection clack is opt-in, independently of consent sounds and notifications.
    pub fn connection_sound(&self) -> bool {
        self.read("connectionSound").unwrap_or(false)
    }

    pub fn set_connection_sound(&self, value: bool) -> Result<()> {
        self.write("connectionSound", Some(value))
    }

    /// Start with the system as the user chose it from the panel; `None` if they never
    /// touched it. The installer respects it: it turns it on only if nobody said no.
    pub fn autostart(&self) -> Option<bool> {
        self.read("autostart")
    }

    pub fn set_autostart(&self, value: Option<bool>) -> Result<()> {
        self.write("autostart", value)
    }

    fn read(&self, key: &str) -> Option<bool> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        load(&self.path()).get(key).and_then(Value::as_bool)
    }

    fn write(&self, key: &str, value: Option<bool>) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = load(&self.path());
        match value {
            Some(b) => {
                settings.insert(key.to_owned(), Value::Bool(b));
            }
            None => {
                settings.remove(key);
            }
        }
        save(&self.home, &settings)
    }
}

/// The file's content; empty if missing or unreadable.
pub(crate) fn load(path: &Path) -> Map<String, Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

pub(crate) fn save(home: &Path, settings: &Map<String, Value>) -> Result<()> {
    std::fs::create_dir_all(home)?;
    let text = serde_json::to_string_pretty(settings).unwrap_or_else(|_| "{}".into());
    std::fs::write(file(home), text)?;
    Ok(())
}
