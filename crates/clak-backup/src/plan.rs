//! What a backup would restore on this computer, item by item, checked before anything is
//! touched: the backup may come from another computer, or another system.
//!
//! - An **allowed application** is restored only if the executable at its path is the same
//!   program (the same signer, or if unsigned the same content), as when it connects: a file
//!   must not be able to allow a program. If it isn't there, or something else is, the user
//!   may point to where it is now; its key is made again from that file.
//! - An **"Always allow" rule** only if its agent is the same program at the same path, its
//!   folder is here, and its application is allowed (here, or restored with it).
//! - An **MCP server that is a program** is restored turned off if its program or a path on
//!   its command line isn't on this computer, so that the user changes it first.
//! - A **preset** whose folder isn't here is restored to be changed.
//!
//! What is already here stays as it is: a server or an API already here, without the token
//! the backup has, only gets the token.

use crate::contents::Contents;
use crate::file::Header;
use clak_apis::ApiEntry;
use clak_connectors::ConnectorEntry;
use clak_hub::launch::Target;
use clak_hub::permissions::{AppPermissions, Entry};
use clak_hub::rules::{self, Rule};
use clak_hub::{AgentIdentity, Platform};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    Connector,
    Api,
    Preset,
    Rule,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::App => "app",
            Kind::Connector => "connector",
            Kind::Api => "api",
            Kind::Preset => "preset",
            Kind::Rule => "rule",
        }
    }
}

/// How an item stands on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Restored as it was.
    Ready,
    /// Already here, without the secret the backup has: only the secret is restored.
    Secret,
    /// Already here: nothing to do.
    Here,
    /// Restored, to be changed afterwards: why.
    Warn(String),
    /// Can't be restored on this computer: why.
    Blocked(String),
}

impl Check {
    /// Whether the user can tick it.
    pub fn restorable(&self) -> bool {
        matches!(self, Check::Ready | Check::Secret | Check::Warn(_))
    }

    fn as_str(&self) -> &'static str {
        match self {
            Check::Ready => "ready",
            Check::Secret => "secret",
            Check::Here => "here",
            Check::Warn(_) => "warn",
            Check::Blocked(_) => "blocked",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// `app-0`, `connector-2`: the kind and the place in the backup.
    pub id: String,
    pub kind: Kind,
    pub index: usize,
    pub name: String,
    /// Where it is: the path, the address, the folder.
    pub detail: String,
    pub check: Check,
    /// Ticked to start with: what can be restored as it is.
    pub chosen: bool,
    /// The item already here it completes (`Check::Secret`, or an application already
    /// allowed under its new key).
    pub here: Option<String>,
    /// An application pointed to where it is now: its new key, and its origin read from
    /// the file.
    pub moved: Option<(String, Value)>,
}

/// What is already on this computer, to compare with.
#[derive(Default)]
pub struct Here {
    pub apps: Vec<Entry>,
    pub rules: Vec<Rule>,
    pub connectors: Vec<HereConnector>,
    pub apis: Vec<HereApi>,
    pub presets: Vec<String>,
    /// The user's PATH, where a program named without a folder is looked for.
    pub path: OsString,
}

pub struct HereConnector {
    pub entry: ConnectorEntry,
    pub has_credential: bool,
    pub env: BTreeMap<String, String>,
}

pub struct HereApi {
    pub entry: ApiEntry,
    pub has_secret: bool,
}

/// A backup opened and checked: the content, with its secrets, stays in here.
pub struct Plan {
    pub header: Header,
    pub contents: Contents,
    pub items: Vec<Item>,
}

impl Plan {
    /// Each item of `contents`, checked against `here`. Slow (it reads executables): never
    /// under a lock, nor on an async thread.
    pub fn check(header: Header, contents: Contents, here: &Here, platform: &dyn Platform) -> Plan {
        let mut items = Vec::new();
        for (index, app) in contents.apps.iter().enumerate() {
            items.push(check_app(index, app, here, platform));
        }
        for (index, saved) in contents.connectors.iter().enumerate() {
            let e = &saved.entry;
            let mut item = item(Kind::Connector, index, &e.name, e.target());
            let same = here.connectors.iter().find(|h| match &e.launch {
                Some(launch) => h.entry.launch.as_ref() == Some(launch),
                None => !h.entry.is_stdio() && clak_connectors::same_url(&h.entry.url, &e.url),
            });
            item.check = match same {
                Some(h) => {
                    item.here = Some(h.entry.id.clone());
                    let missing = match &e.launch {
                        Some(_) => saved.env.iter().any(|(k, v)| {
                            !v.is_empty() && h.env.get(k).is_none_or(String::is_empty)
                        }),
                        None => {
                            !h.has_credential && (saved.token.is_some() || saved.oauth.is_some())
                        }
                    };
                    if missing { Check::Secret } else { Check::Here }
                }
                None => match &e.launch {
                    Some(launch) => {
                        match missing_paths(&launch.program, &launch.args, &here.path) {
                            Some(what) => Check::Warn(format!(
                                "{what} isn't on this computer: it is restored turned off, to change its command line."
                            )),
                            None => Check::Ready,
                        }
                    }
                    None => Check::Ready,
                },
            };
            items.push(item);
        }
        for (index, saved) in contents.apis.iter().enumerate() {
            let e = &saved.entry;
            let mut item = item(Kind::Api, index, &e.name, e.base_url.clone());
            let bare = |url: &str| url.trim().trim_end_matches('/').to_lowercase();
            item.check =
                match here.apis.iter().find(|h| {
                    h.entry.name == e.name && bare(&h.entry.base_url) == bare(&e.base_url)
                }) {
                    Some(h) => {
                        item.here = Some(h.entry.id.clone());
                        if !h.has_secret && saved.secret.is_some() {
                            Check::Secret
                        } else {
                            Check::Here
                        }
                    }
                    None => Check::Ready,
                };
            items.push(item);
        }
        for (index, preset) in contents.presets.iter().enumerate() {
            let text = |key: &str| preset.get(key).and_then(Value::as_str).unwrap_or("");
            let (name, folder) = (text("name"), text("folder"));
            let mut item = item(Kind::Preset, index, name, folder.to_owned());
            item.check = if here.presets.iter().any(|p| p == name) {
                Check::Here
            } else if !folder.is_empty() && !Path::new(folder).is_dir() {
                Check::Warn("Its folder isn't on this computer: change it in the preset.".into())
            } else {
                Check::Ready
            };
            items.push(item);
        }
        for (index, rule) in contents.rules.iter().enumerate() {
            items.push(check_rule(index, rule, here, platform));
        }
        for item in &mut items {
            item.chosen = matches!(item.check, Check::Ready | Check::Secret);
        }
        Plan {
            header,
            contents,
            items,
        }
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// The application `id` is now at `exe`: checked again from that file. Slow (it may read
    /// the executable). The item as it stands now.
    pub fn locate(
        &mut self,
        id: &str,
        exe: &str,
        here: &Here,
        platform: &dyn Platform,
    ) -> Result<&Item, String> {
        let at = self
            .items
            .iter()
            .position(|i| i.id == id && i.kind == Kind::App)
            .ok_or("That application isn't in this backup.")?;
        let entry = &self.contents.apps[self.items[at].index];
        if !Path::new(exe).is_file() {
            return Err("There is no program at that path.".into());
        }
        let origin = platform.origin_of(exe);
        let key = AppPermissions::key_for(platform, &origin, &entry.name)
            .ok_or("That file can't be read.")?;
        let item = &mut self.items[at];
        item.detail = exe.to_owned();
        if here.apps.iter().any(|a| a.key == key) {
            item.check = Check::Here;
            item.chosen = false;
            item.here = Some(key.clone());
        } else {
            item.check = Check::Ready;
            item.chosen = true;
            item.here = None;
        }
        item.moved = Some((key, origin));
        Ok(&self.items[at])
    }

    /// How an item stands, given the others: a rule whose application can't be restored
    /// can't be either.
    pub fn effective(&self, item: &Item) -> Check {
        if item.kind != Kind::Rule || !item.check.restorable() {
            return item.check.clone();
        }
        match self.rule_app(item) {
            Some(app) if !matches!(app.check, Check::Ready | Check::Here) => Check::Blocked(
                format!("Its application, “{}”, can't be restored.", app.name),
            ),
            _ => item.check.clone(),
        }
    }

    /// The application item a rule's program is, if it is one of the backup's.
    pub fn rule_app(&self, item: &Item) -> Option<&Item> {
        let rule = self.contents.rules.get(item.index)?;
        let Target::App(key) = &rule.program.target else {
            return None;
        };
        self.items.iter().find(|i| {
            i.kind == Kind::App
                && self
                    .contents
                    .apps
                    .get(i.index)
                    .is_some_and(|a| &a.key == key)
        })
    }

    /// For the panel: no secret, no key.
    pub fn view(&self) -> Value {
        json!({
            "header": self.header.to_json(),
            // Made on another system: its paths are of that one.
            "foreign": self.header.system != std::env::consts::OS,
            "counts": self.contents.counts(),
            "items": self.items.iter().map(|i| self.item_view(i)).collect::<Vec<_>>(),
        })
    }

    pub fn item_view(&self, item: &Item) -> Value {
        let check = self.effective(item);
        let message = match &check {
            Check::Warn(why) | Check::Blocked(why) => Some(why.clone()),
            Check::Secret => Some(
                match item.kind {
                    Kind::Connector if self.contents.connectors[item.index].entry.is_stdio() => {
                        "Already here, without the values of its variables: they are restored."
                    }
                    _ => "Already here, without its token: the token is restored.",
                }
                .to_owned(),
            ),
            Check::Here => Some("Already here.".to_owned()),
            Check::Ready => self.ready_note(item),
        };
        json!({
            "id": item.id,
            "kind": item.kind.as_str(),
            "name": item.name,
            "detail": item.detail,
            "state": check.as_str(),
            "message": message,
            "chosen": item.chosen && check.restorable(),
            "locate": item.kind == Kind::App && (matches!(check, Check::Blocked(_)) || item.moved.is_some()),
        })
    }

    /// What goes with an item that is restored as it is.
    fn ready_note(&self, item: &Item) -> Option<String> {
        match item.kind {
            Kind::Connector => {
                let saved = &self.contents.connectors[item.index];
                if saved.oauth.is_some() {
                    Some("With its sign-in.".into())
                } else if saved.token.is_some() {
                    Some("With its token.".into())
                } else if saved.env.values().any(|v| !v.is_empty()) {
                    Some("With its variables.".into())
                } else {
                    None
                }
            }
            Kind::Api => self.contents.apis[item.index]
                .secret
                .as_ref()
                .map(|_| "With its secret.".into()),
            Kind::App => item.moved.as_ref().map(|_| "Checked at this path.".into()),
            Kind::Rule => self
                .rule_app(item)
                .map(|app| format!("With “{}”.", app.name)),
            Kind::Preset => None,
        }
    }
}

fn item(kind: Kind, index: usize, name: &str, detail: String) -> Item {
    Item {
        id: format!("{}-{index}", kind.as_str()),
        kind,
        index,
        name: name.to_owned(),
        detail,
        check: Check::Ready,
        chosen: false,
        here: None,
        moved: None,
    }
}

fn check_app(index: usize, app: &Entry, here: &Here, platform: &dyn Platform) -> Item {
    let shown = app.label.clone().unwrap_or_else(|| app.name.clone());
    let mut item = item(Kind::App, index, &shown, app.exe.clone());
    item.check = if here.apps.iter().any(|a| a.key == app.key) {
        Check::Here
    } else if !Path::new(&app.exe).is_file() {
        Check::Blocked("It isn't at this path on this computer: point to where it is.".into())
    } else {
        let origin = platform.origin_of(&app.exe);
        match AppPermissions::key_for(platform, &origin, &app.name) {
            Some(key) if key == app.key => Check::Ready,
            Some(_) => Check::Blocked(
                match origin
                    .get("signer")
                    .and_then(Value::as_str)
                    .filter(|_| origin.get("signed").and_then(Value::as_str) == Some("valid"))
                {
                    Some(signer) => match app.signer.as_deref() {
                        Some(allowed) => format!(
                            "The program at this path is signed by {signer}, not {allowed}."
                        ),
                        None => format!(
                            "The program at this path is signed by {signer}; the one allowed wasn't signed."
                        ),
                    },
                    None if app.signer.is_some() => {
                        "The program at this path isn't signed as the one allowed.".into()
                    }
                    None => "The program at this path is another one, or another version.".into(),
                },
            ),
            None => Check::Blocked("The program at this path can't be read.".into()),
        }
    };
    item
}

fn check_rule(index: usize, rule: &Rule, here: &Here, platform: &dyn Platform) -> Item {
    let mut detail = rule.agent.clone();
    if let Some(folder) = &rule.folder {
        detail.push_str(&format!(" · in {folder}"));
    }
    let mut item = item(Kind::Rule, index, &rule.label, detail);
    let same = |r: &Rule| {
        r.agent_key == rule.agent_key && r.folder == rule.folder && r.program == rule.program
    };
    item.check = if here.rules.iter().any(same) {
        Check::Here
    } else if rule.exe.is_empty() || !Path::new(&rule.exe).is_file() {
        Check::Blocked(format!(
            "{} isn't at {} on this computer.",
            rule.agent, rule.exe
        ))
    } else if agent_key_at(platform, &rule.exe, &rule.agent).as_deref() != Some(&rule.agent_key) {
        Check::Blocked(format!(
            "{} at {} is another program, or another version.",
            rule.agent, rule.exe
        ))
    } else if let Some(folder) = rule.folder.as_ref().filter(|f| !Path::new(f).is_dir()) {
        Check::Blocked(format!("The folder {folder} isn't on this computer."))
    } else {
        // An application not allowed here must come with it: [`Plan::effective`] says
        // whether it can.
        Check::Ready
    };
    item
}

/// The key the agent at `exe` would have, as when it asks: its path, its name, and its
/// signer or content.
fn agent_key_at(platform: &dyn Platform, exe: &str, name: &str) -> Option<String> {
    let origin = platform.origin_of(exe);
    let agent = AgentIdentity {
        pid: 0,
        started: 0,
        started_at: None,
        exe: Some(exe.to_owned()),
        name: name.to_owned(),
        chain: Vec::new(),
        signed: origin
            .get("signed")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        signer: origin
            .get("signer")
            .and_then(Value::as_str)
            .map(str::to_owned),
    };
    rules::agent_key(platform, &agent)
}

/// What of a command line isn't on this computer: the program, or a path among its
/// arguments. `None` if everything is.
pub fn missing_paths(program: &str, args: &[String], path: &OsString) -> Option<String> {
    if clak_connectors::launch::resolve(program, path).is_none() {
        return Some(format!("“{program}”"));
    }
    args.iter()
        .find(|a| looks_like_a_path(a) && !Path::new(a.as_str()).exists())
        .map(|a| format!("“{a}”"))
}

/// An absolute path, of any system: `C:\…`, `\\server\…`, `/home/…` (with at least two
/// parts: `/c` is an option of `cmd`).
fn looks_like_a_path(arg: &str) -> bool {
    let b = arg.as_bytes();
    let drive =
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    let unc = arg.starts_with(r"\\");
    let unix = arg.starts_with('/') && arg[1..].contains('/');
    drive || unc || unix
}

#[cfg(test)]
mod tests {
    use super::looks_like_a_path;

    #[test]
    fn paths_of_any_system_but_not_options() {
        assert!(looks_like_a_path(r"C:\tools\server.js"));
        assert!(looks_like_a_path("D:/x"));
        assert!(looks_like_a_path(r"\\nas\share\x"));
        assert!(looks_like_a_path("/Users/me/server.js"));
        assert!(!looks_like_a_path("/c"));
        assert!(!looks_like_a_path("-y"));
        assert!(!looks_like_a_path(
            "@modelcontextprotocol/server-filesystem"
        ));
        assert!(!looks_like_a_path("https://example.com/a/b"));
        assert!(!looks_like_a_path("C:"));
    }
}
