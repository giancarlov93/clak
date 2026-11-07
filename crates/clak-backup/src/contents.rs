//! What a backup holds: the MCP servers with their tokens, sign-ins and environment
//! variables, the REST APIs with their secrets, the presets, the allowed applications and
//! the "Always allow" rules. Not the usage counts, the prices, the icons nor the app's
//! preferences: they are made again, or belong to this computer.
//!
//! It is read from the hub's folder and from the secret store, the same ones the hub reads
//! (on the Mac the Keychain), and exists in plain text only in memory: sealed in the file,
//! and while restoring in the app, never on a page.

use clak_apis::{ApiEntry, ApiStore};
use clak_connectors::launch::Launch;
use clak_connectors::{ConnectorEntry, ConnectorStore, OAuthGrant};
use clak_hub::permissions::{AppPermissions, Entry};
use clak_hub::rules::{Rule, Rules};
use clak_wire::json as wire;
use hmac::{Hmac, Mac};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::path::Path;

/// A server as it was, with what it presented itself with.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedConnector {
    pub entry: ConnectorEntry,
    pub token: Option<String>,
    pub oauth: Option<OAuthGrant>,
    pub env: BTreeMap<String, String>,
}

impl SavedConnector {
    /// Whether it carries anything to present itself with.
    pub fn has_credential(&self) -> bool {
        self.token.is_some() || self.oauth.is_some() || self.env.values().any(|v| !v.is_empty())
    }
}

/// An API as it was, with its secret (a token, a key, or the whole OAuth access).
#[derive(Debug, Clone, PartialEq)]
pub struct SavedApi {
    pub entry: ApiEntry,
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contents {
    pub connectors: Vec<SavedConnector>,
    pub apis: Vec<SavedApi>,
    pub presets: Vec<Map<String, Value>>,
    pub apps: Vec<Entry>,
    pub rules: Vec<Rule>,
}

impl Contents {
    /// Everything, read now from `home` and the two stores.
    pub fn collect(home: &Path, connectors: &ConnectorStore, apis: &ApiStore) -> Contents {
        Contents {
            connectors: connectors
                .all()
                .into_iter()
                .map(|entry| SavedConnector {
                    token: connectors.token(&entry.id),
                    oauth: connectors.oauth(&entry.id),
                    env: if entry.is_stdio() {
                        connectors.env(&entry.id)
                    } else {
                        BTreeMap::new()
                    },
                    entry,
                })
                .collect(),
            apis: apis
                .all()
                .into_iter()
                .map(|entry| SavedApi {
                    secret: apis.secret(&entry.id).filter(|s| !s.is_empty()),
                    entry,
                })
                .collect(),
            presets: clak_setup::presets::all(home),
            apps: AppPermissions::load(home.join("apps.json")).all(),
            rules: Rules::load(home.join("rules.json")).all().to_vec(),
        }
    }

    /// How many of each, for the panel.
    pub fn counts(&self) -> Value {
        json!({
            "connectors": self.connectors.len(),
            "apis": self.apis.len(),
            "presets": self.presets.len(),
            "apps": self.apps.len(),
            "rules": self.rules.len(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.connectors.is_empty()
            && self.apis.is_empty()
            && self.presets.is_empty()
            && self.apps.is_empty()
            && self.rules.is_empty()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "connectors": self.connectors.iter().map(connector_json).collect::<Vec<_>>(),
            "apis": self.apis.iter().map(|a| {
                let mut item = a.entry.to_json();
                item["secret"] = json!(a.secret);
                item
            }).collect::<Vec<_>>(),
            "presets": self.presets,
            "apps": self.apps.iter().map(app_json).collect::<Vec<_>>(),
            "rules": self.rules.iter().map(Rule::to_json).collect::<Vec<_>>(),
        })
    }

    /// What can be read of a backup's content: an item that can't be read is left out.
    pub fn from_json(value: &Value) -> Contents {
        let list = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        Contents {
            connectors: list("connectors")
                .iter()
                .filter_map(connector_from)
                .collect(),
            apis: list("apis")
                .iter()
                .filter_map(|item| {
                    Some(SavedApi {
                        entry: ApiEntry::from_json(item)?,
                        secret: text(item.get("secret")),
                    })
                })
                .collect(),
            presets: list("presets")
                .into_iter()
                .filter_map(|p| match p {
                    Value::Object(map) => Some(map),
                    _ => None,
                })
                .collect(),
            apps: list("apps").iter().filter_map(app_from).collect(),
            rules: list("rules").iter().filter_map(Rule::from_json).collect(),
        }
    }

    /// A mark of what there is, to know whether anything changed since the last backup,
    /// keyed with `key` so that it says nothing of the secrets. What changes by itself
    /// does not count: an OAuth access token refreshed every hour, its expiry.
    pub fn fingerprint(&self, key: &[u8]) -> String {
        let mut value = self.to_json();
        for connector in value["connectors"].as_array_mut().into_iter().flatten() {
            if let Some(oauth) = connector.get_mut("oauth").and_then(Value::as_object_mut) {
                oauth.remove("access_token");
                oauth.remove("expires_at");
            }
        }
        for api in value["apis"].as_array_mut().into_iter().flatten() {
            if let Some(grant) = api
                .get("secret")
                .and_then(Value::as_str)
                .and_then(|s| wire::parse_str(s).ok())
                .filter(|g| g.get("refresh_token").is_some())
            {
                let mut grant = grant;
                if let Some(o) = grant.as_object_mut() {
                    o.remove("access_token");
                    o.remove("expires_at");
                }
                api["secret"] = json!(wire::text(&grant));
            }
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
        mac.update(&wire::encode_pretty(&value));
        hex::encode(mac.finalize().into_bytes())
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A connector in the same shape as `connectors.json`, with its secrets beside it.
fn connector_json(saved: &SavedConnector) -> Value {
    let e = &saved.entry;
    let mut item = json!({
        "id": e.id,
        "name": e.name,
        "url": e.url,
        "enabled": e.enabled,
        "since": wire::float(e.since),
        "auth": e.auth,
        "command": e.command,
        "token": saved.token,
        "oauth": saved.oauth.as_ref().map(OAuthGrant::to_json),
        "env": saved.env,
    });
    if let Some(launch) = &e.launch {
        item["transport"] = json!("stdio");
        item["program"] = json!(launch.program);
        item["args"] = json!(launch.args);
    }
    item
}

fn connector_from(item: &Value) -> Option<SavedConnector> {
    let launch = if item.get("transport").and_then(Value::as_str) == Some("stdio") {
        Some(Launch {
            program: text(item.get("program"))?,
            args: item
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        })
    } else {
        None
    };
    let url = item
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if launch.is_none() && url.is_empty() {
        return None;
    }
    let oauth = item.get("oauth").and_then(OAuthGrant::from_json);
    Some(SavedConnector {
        entry: ConnectorEntry {
            id: text(item.get("id"))?,
            name: text(item.get("name"))?,
            url,
            enabled: item.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            since: item.get("since").and_then(Value::as_f64).unwrap_or(0.0),
            auth: if oauth.is_some() {
                ConnectorEntry::OAUTH_AUTH.into()
            } else {
                ConnectorEntry::TOKEN_AUTH.into()
            },
            command: text(item.get("command")),
            launch,
        },
        token: text(item.get("token")),
        oauth,
        env: item
            .get("env")
            .and_then(Value::as_object)
            .map(|env| {
                env.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// An allowed application in the same shape as `apps.json`.
fn app_json(e: &Entry) -> Value {
    json!({
        "key": e.key, "exe": e.exe, "name": e.name, "signer": e.signer,
        "since": wire::float(e.since), "label": e.label, "command": e.command,
    })
}

fn app_from(item: &Value) -> Option<Entry> {
    Some(Entry {
        key: text(item.get("key"))?,
        exe: text(item.get("exe"))?,
        name: item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        signer: text(item.get("signer")),
        since: item.get("since").and_then(Value::as_f64).unwrap_or(0.0),
        label: text(item.get("label")),
        command: text(item.get("command")),
    })
}
