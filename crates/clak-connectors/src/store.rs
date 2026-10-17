//! The connectors added by the user: `connectors.json` in the hub's folder, next to
//! `apps.json`, and the tokens apart, in the secret store ([`SecretBox`]): on Windows
//! `connectors.secrets`, encrypted with DPAPI; on the Mac the Keychain. For a connector with OAuth the store holds the whole
//! sign-in (access and refresh token, the registered client), under `<id>#oauth`. For a server that
//! is a program on this computer (stdio) the store holds its environment variables,
//! which are often keys, under `<id>#env`.
//!
//! A token never goes into the log, the panel's snapshot, the control channel's
//! messages or a command line: it leaves here only toward the server it belongs to.
//! Encryption protects it from someone reading the file as another user or from another
//! computer, not from a program already running as the user.

use crate::launch::Launch;
use crate::oauth::{OAuthGrant, strip_bom};
use clak_hub::cli_names;
use clak_platform::SecretBox;
use clak_wire::json as wire;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A connector as the user added it. The token isn't here. `auth` says how it presents
/// itself: with a pasted token (or none), or with an OAuth sign-in. `command` is the
/// command-line name chosen by the user; `None`, the one derived from the name.
/// `launch` is there for a server that is a program to start (stdio): then `url` is empty.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorEntry {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub since: f64,
    pub auth: String,
    pub command: Option<String>,
    pub launch: Option<Launch>,
}

impl ConnectorEntry {
    pub const TOKEN_AUTH: &'static str = "token";
    pub const OAUTH_AUTH: &'static str = "oauth";

    pub fn is_oauth(&self) -> bool {
        self.auth == Self::OAUTH_AUTH
    }

    pub fn is_stdio(&self) -> bool {
        self.launch.is_some()
    }

    /// Where it leads, for display: the address, or the program's command line.
    pub fn target(&self) -> String {
        match &self.launch {
            Some(launch) => launch.display(),
            None => self.url.clone(),
        }
    }
}

pub struct ConnectorStore {
    home: PathBuf,
    secrets: Arc<dyn SecretBox>,
    gate: Mutex<()>,
}

impl ConnectorStore {
    /// `secrets`: usually `clak_platform::default_box(home, "connectors.secrets")`.
    pub fn new(home: impl Into<PathBuf>, secrets: Arc<dyn SecretBox>) -> ConnectorStore {
        ConnectorStore {
            home: home.into(),
            secrets,
            gate: Mutex::new(()),
        }
    }

    fn list_path(&self) -> PathBuf {
        self.home.join("connectors.json")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn all(&self) -> Vec<ConnectorEntry> {
        let _gate = self.lock();
        self.load()
    }

    pub fn get(&self, id: &str) -> Option<ConnectorEntry> {
        let _gate = self.lock();
        self.load().into_iter().find(|c| c.id == id)
    }

    pub fn add(
        &self,
        name: &str,
        url: &str,
        token: Option<&str>,
        command: Option<&str>,
    ) -> anyhow::Result<ConnectorEntry> {
        let _gate = self.lock();
        let entry = new_entry(name, url, ConnectorEntry::TOKEN_AUTH, command, None);
        let mut all = self.load();
        all.push(entry.clone());
        self.save(&all)?;
        if let Some(token) = token.filter(|t| !t.is_empty()) {
            self.secrets
                .set_secret(&entry.id, Some(token), &token_label(name))?;
        }
        Ok(entry)
    }

    pub fn add_oauth(
        &self,
        name: &str,
        url: &str,
        grant: &OAuthGrant,
        command: Option<&str>,
    ) -> anyhow::Result<ConnectorEntry> {
        let _gate = self.lock();
        let entry = new_entry(name, url, ConnectorEntry::OAUTH_AUTH, command, None);
        let mut all = self.load();
        all.push(entry.clone());
        self.save(&all)?;
        self.secrets.set_secret(
            &oauth_key(&entry.id),
            Some(&wire::text(&grant.to_json())),
            &grant_label(name),
        )?;
        Ok(entry)
    }

    /// A server that is a program: the command line, and the environment variables (encrypted).
    pub fn add_stdio(
        &self,
        name: &str,
        launch: &Launch,
        env: &BTreeMap<String, String>,
        command: Option<&str>,
    ) -> anyhow::Result<ConnectorEntry> {
        let _gate = self.lock();
        let entry = new_entry(
            name,
            "",
            ConnectorEntry::TOKEN_AUTH,
            command,
            Some(launch.clone()),
        );
        let mut all = self.load();
        all.push(entry.clone());
        self.save(&all)?;
        self.write_env(&entry.id, name, env)?;
        Ok(entry)
    }

    /// The program's environment variables; empty if there are none or they can't be decrypted.
    pub fn env(&self, id: &str) -> BTreeMap<String, String> {
        let _gate = self.lock();
        self.secrets
            .secret(&env_key(id))
            .and_then(|json| serde_json::from_str::<Map<String, Value>>(&json).ok())
            .map(|map| {
                map.into_iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_owned())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The new environment variables, replacing the old ones (none: they're removed).
    pub fn set_env(&self, id: &str, env: &BTreeMap<String, String>) -> anyhow::Result<()> {
        let _gate = self.lock();
        let name = self.name_of(id);
        self.write_env(id, &name, env)
    }

    fn write_env(
        &self,
        id: &str,
        name: &str,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        let text = (!env.is_empty()).then(|| wire::text(&json!(env)));
        self.secrets
            .set_secret(&env_key(id), text.as_deref(), &env_label(name))?;
        Ok(())
    }

    pub fn update(&self, entry: &ConnectorEntry) -> anyhow::Result<()> {
        let _gate = self.lock();
        let all: Vec<ConnectorEntry> = self
            .load()
            .into_iter()
            .map(|c| if c.id == entry.id { entry.clone() } else { c })
            .collect();
        self.save(&all)
    }

    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        let _gate = self.lock();
        let all: Vec<ConnectorEntry> = self.load().into_iter().filter(|c| c.id != id).collect();
        self.save(&all)?;
        self.secrets.set_secret(id, None, "")?;
        self.secrets.set_secret(&oauth_key(id), None, "")?;
        self.secrets.set_secret(&env_key(id), None, "")?;
        Ok(())
    }

    /// The token in plain text, or `None` if there is none or it can't be decrypted (a file copied from another user).
    pub fn token(&self, id: &str) -> Option<String> {
        let _gate = self.lock();
        self.secrets.secret(id)
    }

    /// The OAuth sign-in, or `None` if there is none or it can't be decrypted.
    pub fn oauth(&self, id: &str) -> Option<OAuthGrant> {
        let _gate = self.lock();
        let json = self.secrets.secret(&oauth_key(id))?;
        OAuthGrant::from_json(&serde_json::from_str::<Value>(&json).ok()?)
    }

    /// A pasted token (or none): the connector switches to the token, and the OAuth sign-in is forgotten.
    pub fn set_token(&self, id: &str, token: Option<&str>) -> anyhow::Result<()> {
        let _gate = self.lock();
        let name = self.name_of(id);
        self.secrets
            .set_secret(id, token.filter(|t| !t.is_empty()), &token_label(&name))?;
        self.secrets.set_secret(&oauth_key(id), None, "")?;
        self.set_auth(id, ConnectorEntry::TOKEN_AUTH)
    }

    /// An OAuth sign-in, new or refreshed: the connector switches to OAuth, and the pasted token is forgotten.
    pub fn set_oauth(&self, id: &str, grant: &OAuthGrant) -> anyhow::Result<()> {
        let _gate = self.lock();
        let name = self.name_of(id);
        self.secrets.set_secret(
            &oauth_key(id),
            Some(&wire::text(&grant.to_json())),
            &grant_label(&name),
        )?;
        self.secrets.set_secret(id, None, "")?;
        self.set_auth(id, ConnectorEntry::OAUTH_AUTH)
    }

    fn name_of(&self, id: &str) -> String {
        self.load()
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.name)
            .unwrap_or_default()
    }

    fn set_auth(&self, id: &str, auth: &str) -> anyhow::Result<()> {
        let all = self.load();
        if all.iter().any(|c| c.id == id && c.auth != auth) {
            let changed: Vec<ConnectorEntry> = all
                .into_iter()
                .map(|mut c| {
                    if c.id == id {
                        c.auth = auth.to_owned();
                    }
                    c
                })
                .collect();
            self.save(&changed)?;
        }
        Ok(())
    }

    fn load(&self) -> Vec<ConnectorEntry> {
        let Some(Value::Array(items)) =
            read_json(&self.list_path()).and_then(|mut v| v.get_mut("connectors").map(Value::take))
        else {
            return Vec::new();
        };
        let mut list = Vec::new();
        for item in items.iter().filter_map(Value::as_object) {
            let (Some(id), Some(name)) = (wire::str(item.get("id")), wire::str(item.get("name")))
            else {
                continue;
            };
            // A program (stdio) has its command line; an HTTP server its address.
            let launch = if wire::str(item.get("transport")) == Some(STDIO) {
                let Some(program) = wire::str(item.get("program")).filter(|p| !p.is_empty()) else {
                    continue;
                };
                let args = match item.get("args") {
                    Some(Value::Array(args)) => args
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                    _ => Vec::new(),
                };
                Some(Launch {
                    program: program.to_owned(),
                    args,
                })
            } else {
                None
            };
            let url = wire::str(item.get("url")).unwrap_or("");
            if launch.is_none() && url.is_empty() {
                continue;
            }
            let command = wire::str(item.get("command"))
                .filter(|c| !c.is_empty() && cli_names::problem(Some(c)).is_none())
                .map(str::to_owned);
            list.push(ConnectorEntry {
                id: id.to_owned(),
                name: name.to_owned(),
                url: url.to_owned(),
                enabled: item.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                since: item.get("since").and_then(Value::as_f64).unwrap_or(0.0),
                auth: if wire::str(item.get("auth")) == Some(ConnectorEntry::OAUTH_AUTH) {
                    ConnectorEntry::OAUTH_AUTH.into()
                } else {
                    ConnectorEntry::TOKEN_AUTH.into()
                },
                command,
                launch,
            });
        }
        list
    }

    fn save(&self, entries: &[ConnectorEntry]) -> anyhow::Result<()> {
        let items: Vec<Value> = entries
            .iter()
            .map(|c| {
                let mut item = json!({
                    "id": c.id,
                    "name": c.name,
                    "url": c.url,
                    "enabled": c.enabled,
                    "since": wire::float(c.since),
                    "auth": c.auth,
                    "command": c.command,
                });
                if let Some(launch) = &c.launch {
                    item["transport"] = json!(STDIO);
                    item["program"] = json!(launch.program);
                    item["args"] = json!(launch.args);
                }
                item
            })
            .collect();
        write_json(&self.list_path(), &json!({ "connectors": items }))?;
        Ok(())
    }
}

/// The transport of a server that is a program, in `connectors.json`.
const STDIO: &str = "stdio";

fn new_entry(
    name: &str,
    url: &str,
    auth: &str,
    command: Option<&str>,
    launch: Option<Launch>,
) -> ConnectorEntry {
    ConnectorEntry {
        id: wire::new_id("cn"),
        name: name.to_owned(),
        url: url.to_owned(),
        enabled: true,
        since: wire::now(),
        auth: auth.into(),
        command: command.map(str::to_owned),
        launch,
    }
}

/// The entries as the user reads them in the Keychain (where the system shows a name).
fn token_label(name: &str) -> String {
    format!("Clak: {name}")
}

fn grant_label(name: &str) -> String {
    format!("Clak: {name} (sign-in)")
}

fn env_label(name: &str) -> String {
    format!("Clak: {name} (environment)")
}

fn oauth_key(id: &str) -> String {
    format!("{id}#oauth")
}

fn env_key(id: &str) -> String {
    format!("{id}#env")
}

/// A JSON file in the hub's folder, or `None` if it doesn't exist or can't be read. Also used
/// by the REST APIs (`apis.json`).
pub fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(strip_bom(&bytes)).ok()
}

/// Writes an indented JSON file, through a draft: a reader finds the old one or the
/// new one, never half.
pub fn write_json(path: &Path, content: &Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut draft = path.as_os_str().to_owned();
    draft.push(".tmp");
    let draft = PathBuf::from(draft);
    std::fs::write(&draft, wire::encode_pretty(content))?;
    std::fs::rename(&draft, path)
}
