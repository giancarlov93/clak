//! The programs the user has allowed to connect, remembered across launches in
//! `apps.json`.
//!
//! What is remembered is the executable, not what the program says about itself: the same
//! path *and* the same signer, or — if it is unsigned — the same content. An updated
//! unsigned executable is a different executable, and is asked about again. Along with the
//! executable the declared name is remembered: a browser or a webview hosts many different
//! programs, and saying yes to one must not mean saying yes to all of them.
//!
//! A process that cannot be identified is never remembered.
//!
//! The user can give an application a name of their own and a command of their own for the
//! agents (`clak <command>`), and point it to where it has moved: from the app, never from
//! the control channel.

use crate::consent::Platform;
use clak_wire::json::{self};
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    pub exe: String,
    /// The name the program declares: part of the key.
    pub name: String,
    pub signer: Option<String>,
    pub since: f64,
    /// The name the user gave it, shown instead of the declared one.
    pub label: Option<String>,
    /// The command the user chose for it, instead of the one derived from its name.
    pub command: Option<String>,
}

fn exe_of(origin: &Value) -> String {
    origin
        .get("exe")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Who signed it, only if the signature is valid.
fn signer_of(origin: &Value) -> Option<String> {
    if origin.get("signed").and_then(Value::as_str) != Some("valid") {
        return None;
    }
    origin
        .get("signer")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A text field, `None` if missing or empty.
fn text(item: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    item.get(key)
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

pub struct AppPermissions {
    path: PathBuf,
    entries: IndexMap<String, Entry>,
}

impl AppPermissions {
    pub fn load(path: PathBuf) -> AppPermissions {
        let mut entries = IndexMap::new();
        // No file, or a damaged one: start over and ask again.
        if let Some(list) = std::fs::read(&path)
            .ok()
            .and_then(|b| json::parse(&b).ok())
            .and_then(|v| v.get("apps").and_then(Value::as_array).cloned())
        {
            for item in list.iter().filter_map(Value::as_object) {
                let Some(key) = item
                    .get("key")
                    .and_then(Value::as_str)
                    .filter(|k| !k.is_empty())
                else {
                    continue;
                };
                entries.insert(
                    key.to_owned(),
                    Entry {
                        key: key.to_owned(),
                        exe: item
                            .get("exe")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        signer: item
                            .get("signer")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        since: item.get("since").and_then(Value::as_f64).unwrap_or(0.0),
                        label: text(item, "label"),
                        command: text(item, "command"),
                    },
                );
            }
        }
        AppPermissions { path, entries }
    }

    /// In the order they were allowed.
    pub fn all(&self) -> Vec<Entry> {
        let mut list: Vec<Entry> = self.entries.values().cloned().collect();
        list.sort_by(|a, b| a.since.total_cmp(&b.since));
        list
    }

    /// The key a program is remembered by, or nothing if it cannot be remembered.
    /// Slow for an unsigned executable (its content is read): never under the lock.
    pub fn key_for(platform: &dyn Platform, origin: &Value, name: &str) -> Option<String> {
        let exe = origin
            .get("exe")
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty())?;
        let signer = origin
            .get("signer")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let identity = match (origin.get("signed").and_then(Value::as_str), signer) {
            (Some("valid"), Some(signer)) => format!("firma:{signer}"),
            _ => format!("sha256:{}", platform.executable_hash(exe)?),
        };
        Some(format!("{}|{name}|{identity}", exe.to_lowercase()))
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }

    pub fn allows(&self, key: Option<&str>) -> bool {
        key.is_some_and(|k| self.entries.contains_key(k))
    }

    pub fn allow(&mut self, key: &str, origin: &Value, name: &str) {
        self.entries.insert(
            key.to_owned(),
            Entry {
                key: key.to_owned(),
                exe: exe_of(origin),
                name: name.to_owned(),
                signer: signer_of(origin),
                since: json::now(),
                label: None,
                command: None,
            },
        );
        self.save();
    }

    /// The name and the command the user chose; `None` goes back to the program's own.
    pub fn edit(&mut self, key: &str, label: Option<String>, command: Option<String>) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        entry.label = label;
        entry.command = command;
        self.save();
        true
    }

    /// The application has moved: remembered by its new key, with the executable and signer
    /// of `origin`, keeping when it was allowed and what the user chose for it.
    pub fn moved(&mut self, key: &str, to: &str, origin: &Value) -> bool {
        let Some(mut entry) = self.entries.shift_remove(key) else {
            return false;
        };
        entry.key = to.to_owned();
        entry.exe = exe_of(origin);
        entry.signer = signer_of(origin);
        self.entries.insert(to.to_owned(), entry);
        self.save();
        true
    }

    pub fn forget(&mut self, key: &str) -> bool {
        if self.entries.shift_remove(key).is_none() {
            return false;
        }
        self.save();
        true
    }

    fn save(&self) {
        let content = json!({
            "apps": self.entries.values().map(|e| json!({
                "key": e.key, "exe": e.exe, "name": e.name, "signer": e.signer, "since": json::float(e.since),
                "label": e.label, "command": e.command,
            })).collect::<Vec<_>>(),
        });
        // Not written: the permission holds while the hub stays open, then it will be asked again.
        let draft = self.path.with_extension("json.tmp");
        if std::fs::write(&draft, json::encode_pretty(&content)).is_ok() {
            let _ = std::fs::rename(&draft, &self.path);
        }
    }
}
