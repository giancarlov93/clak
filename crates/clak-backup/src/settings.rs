//! The automatic backup's settings, in `backup.json` in the hub's folder: whether it is on,
//! the folder, how many copies to keep, the lock (the public key, and the private one
//! locked with the password, never the password) and how the last one went.
//!
//! Off until the user turns it on. The file itself never goes into a backup.

use crate::seal::Lock;
use clak_wire::json as wire;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const FILE: &str = "backup.json";

/// How many backups are kept, unless the user says otherwise.
pub const KEEP: usize = 10;
pub const MAX_KEEP: usize = 100;

/// How the last attempt went.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Last {
    pub at: f64,
    /// The file written; none if it failed.
    pub file: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub folder: Option<PathBuf>,
    pub keep: usize,
    pub lock: Option<Lock>,
    /// The key of the fingerprints (hex), made with the lock: they mean nothing without it.
    pub mark: Option<String>,
    /// The fingerprint of what the last backup holds: the same, no new one.
    pub fingerprint: Option<String>,
    pub last: Option<Last>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            folder: None,
            keep: KEEP,
            lock: None,
            mark: None,
            fingerprint: None,
            last: None,
        }
    }
}

impl Settings {
    pub fn path(home: &Path) -> PathBuf {
        home.join(FILE)
    }

    /// What `backup.json` says; missing or unreadable, everything off.
    pub fn load(home: &Path) -> Settings {
        let Some(value) = std::fs::read(Settings::path(home))
            .ok()
            .and_then(|bytes| wire::parse(&bytes).ok())
        else {
            return Settings::default();
        };
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
        };
        let lock = value.get("lock").and_then(Lock::from_json);
        Settings {
            // Without a lock or a folder it can't run, whatever the file says.
            enabled: value
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            folder: text("folder").map(PathBuf::from),
            keep: value
                .get("keep")
                .and_then(Value::as_u64)
                .map_or(KEEP, |k| (k as usize).clamp(1, MAX_KEEP)),
            mark: text("mark").filter(|_| lock.is_some()),
            lock,
            fingerprint: text("fingerprint"),
            last: value.get("last").filter(|l| l.is_object()).map(|l| Last {
                at: l.get("at").and_then(Value::as_f64).unwrap_or(0.0),
                file: l.get("file").and_then(Value::as_str).map(str::to_owned),
                error: l.get("error").and_then(Value::as_str).map(str::to_owned),
            }),
        }
    }

    pub fn save(&self, home: &Path) -> std::io::Result<()> {
        let content = json!({
            "enabled": self.enabled,
            "folder": self.folder.as_ref().map(|f| f.to_string_lossy()),
            "keep": self.keep,
            "lock": self.lock.as_ref().map(Lock::to_json),
            "mark": self.mark,
            "fingerprint": self.fingerprint,
            "last": self.last.as_ref().map(|l| json!({
                "at": wire::float(l.at), "file": l.file, "error": l.error,
            })),
        });
        std::fs::create_dir_all(home)?;
        let path = Settings::path(home);
        let draft = path.with_extension("json.tmp");
        std::fs::write(&draft, wire::encode_pretty(&content))?;
        std::fs::rename(&draft, &path)
    }

    /// Whether a backup can be written: a password chosen and a folder.
    pub fn ready(&self) -> bool {
        self.lock.is_some() && self.mark.is_some() && self.folder.is_some()
    }

    /// A new password: a new lock and a new key for the fingerprints, so that the next
    /// backup is written even if nothing else changed. The backups already written keep
    /// the old password.
    pub fn set_lock(&mut self, lock: Lock) {
        self.lock = Some(lock);
        self.mark = Some(wire::random_hex(32));
        self.fingerprint = None;
    }

    /// The key of the fingerprints, as bytes.
    pub fn mark_key(&self) -> Option<Vec<u8>> {
        hex::decode(self.mark.as_deref()?).ok()
    }

    /// For the panel: everything but the lock, of which only whether there is one.
    pub fn view(&self) -> Value {
        json!({
            "enabled": self.enabled && self.ready(),
            "folder": self.folder.as_ref().map(|f| f.to_string_lossy()),
            "keep": self.keep,
            "password": self.lock.is_some(),
            "last": self.last.as_ref().map(|l| json!({
                "at": wire::float(l.at), "file": l.file, "error": l.error,
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::Cost;

    #[test]
    fn off_until_turned_on_and_back_as_written() {
        let home =
            std::env::temp_dir().join(format!("clak-backup-settings-{}", wire::random_hex(4)));
        let settings = Settings::load(&home);
        assert_eq!(settings, Settings::default());
        assert!(!settings.enabled && !settings.ready());
        assert_eq!(settings.view()["enabled"], false);

        let mut settings = Settings {
            enabled: true,
            folder: Some(home.join("copies")),
            keep: 3,
            ..Settings::default()
        };
        assert_eq!(settings.view()["enabled"], false, "no password yet");
        settings.set_lock(Lock::new("a long password", Cost::CHEAP).unwrap());
        settings.fingerprint = Some("abc".into());
        settings.last = Some(Last {
            at: 5.0,
            file: Some("x".into()),
            error: None,
        });
        settings.save(&home).unwrap();
        let back = Settings::load(&home);
        assert_eq!(back, settings);
        assert!(back.ready());
        assert_eq!(back.mark_key().unwrap().len(), 32);
        let view = back.view();
        assert_eq!(view["enabled"], true);
        assert_eq!(view["password"], true);
        assert!(view.get("lock").is_none() && view.get("mark").is_none());

        // A new password: the next backup is written in any case.
        let mut changed = back.clone();
        changed.set_lock(Lock::new("another password", Cost::CHEAP).unwrap());
        assert_eq!(changed.fingerprint, None);
        assert_ne!(changed.mark, back.mark);

        std::fs::write(
            Settings::path(&home),
            "{\"keep\": 100000, \"enabled\": true}",
        )
        .unwrap();
        let odd = Settings::load(&home);
        assert_eq!(odd.keep, MAX_KEEP);
        assert!(!odd.ready());
        std::fs::remove_dir_all(&home).ok();
    }
}
