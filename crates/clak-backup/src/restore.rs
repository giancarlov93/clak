//! Restoring the items the user ticked, on the hub that is running: what is restored
//! starts at once (a server connects, an application is allowed), as if the user had added
//! it now. Nothing already here is removed or replaced, but a missing secret.
//!
//! The order matters: the applications first, because their key may have changed if the
//! user pointed one to where it is now. The commands the user approved come last, with the
//! programs restored: an application under its key here, a server or an API under its id
//! here (found by its address).

use crate::plan::{Check, Here, HereApi, HereConnector, Kind, Plan};
use clak_apis::{ApiDraft, ApiHub};
use clak_connectors::ConnectorHub;
use clak_hub::Broker;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Where the restore goes: the running hub, its servers and APIs, its folder.
pub struct Targets<'a> {
    pub home: &'a Path,
    pub broker: &'a Broker,
    pub connectors: &'a ConnectorHub,
    pub apis: Option<&'a ApiHub>,
}

impl Targets<'_> {
    /// What is here now, to check a backup against. `path` is the user's PATH (see
    /// `clak_connectors::launch::user_path`, slow on the Mac).
    pub fn here(&self, path: std::ffi::OsString) -> Here {
        let apps = self.broker.run(|c| c.permissions().all());
        Here {
            apps,
            connectors: self
                .connectors
                .entries()
                .into_iter()
                .map(|entry| HereConnector {
                    has_credential: self.connectors.has_credential(&entry.id),
                    env: if entry.is_stdio() {
                        self.connectors.env(&entry.id)
                    } else {
                        Default::default()
                    },
                    entry,
                })
                .collect(),
            apis: self
                .apis
                .map(|hub| {
                    hub.entries()
                        .into_iter()
                        .map(|entry| HereApi {
                            has_secret: hub.has_secret(&entry.id),
                            entry,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            path,
        }
    }
}

/// What happened to one item.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub ok: bool,
    pub message: String,
}

impl Outcome {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "kind": self.kind.as_str(), "name": self.name,
            "ok": self.ok, "message": self.message,
        })
    }
}

/// Restores the items of `plan` whose ids are in `chosen`, if they can be: for each one,
/// what happened.
pub async fn restore(plan: &Plan, chosen: &HashSet<String>, to: &Targets<'_>) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    let picked = |kind: Kind| {
        plan.items
            .iter()
            .filter(move |i| i.kind == kind && chosen.contains(&i.id))
            .filter(|i| i.check.restorable())
    };

    // The applications, and where each old key leads now.
    let mut keys: HashMap<String, String> = HashMap::new();
    for item in plan.items.iter().filter(|i| i.kind == Kind::App) {
        let old = &plan.contents.apps[item.index];
        if let (Check::Here, Some(here)) = (&item.check, &item.here) {
            keys.insert(old.key.clone(), here.clone());
        }
    }
    for item in picked(Kind::App) {
        let mut entry = plan.contents.apps[item.index].clone();
        let old = entry.key.clone();
        if let Some((key, origin)) = &item.moved {
            entry.key = key.clone();
            entry.exe = origin
                .get("exe")
                .and_then(Value::as_str)
                .unwrap_or(&entry.exe)
                .to_owned();
            entry.signer = origin
                .get("signer")
                .and_then(Value::as_str)
                .filter(|_| origin.get("signed").and_then(Value::as_str) == Some("valid"))
                .map(str::to_owned);
        }
        let key = entry.key.clone();
        let done = to.broker.run(|c| c.restore_app(entry));
        keys.insert(old, key);
        outcomes.push(outcome(
            item,
            Ok(if done {
                "Allowed again."
            } else {
                "Already here."
            }
            .to_owned()),
        ));
    }

    for item in picked(Kind::Connector) {
        let result = restore_connector(plan, item, to).await;
        outcomes.push(outcome(item, result));
    }

    for item in picked(Kind::Api) {
        let result = match to.apis {
            Some(hub) => restore_api(plan, item, hub).await,
            None => Err("The REST APIs aren't available on this hub.".into()),
        };
        outcomes.push(outcome(item, result));
    }

    // The approvals of the programs this restore added (not of those already here): only
    // for commands not decided here, and each for its own version (what changed since asks).
    let added = |item: &&crate::plan::Item| matches!(item.check, Check::Ready | Check::Warn(_));
    let mut restored: HashMap<String, String> = HashMap::new();
    for item in picked(Kind::App).filter(added) {
        let old = &plan.contents.apps[item.index].key;
        if let Some(key) = keys.get(old) {
            restored.insert(format!("app:{old}"), format!("app:{key}"));
        }
    }
    // A server or an API here by its address, the same name first; each one taken once,
    // so two at the same address (two accounts) don't both land on the first.
    let mut taken: HashSet<String> = HashSet::new();
    let mut pick = |candidates: Vec<(String, bool)>| -> Option<String> {
        let free = |c: &&(String, bool)| !taken.contains(&c.0);
        let id = candidates
            .iter()
            .filter(free)
            .find(|c| c.1)
            .or_else(|| candidates.iter().find(free))?
            .0
            .clone();
        taken.insert(id.clone());
        Some(id)
    };
    let connectors_here = to.connectors.entries();
    for item in picked(Kind::Connector).filter(added) {
        let old = &plan.contents.connectors[item.index].entry;
        let candidates = connectors_here
            .iter()
            .filter(|e| e.url == old.url && e.launch == old.launch)
            .map(|e| (e.id.clone(), e.name == old.name))
            .collect();
        if let Some(here) = pick(candidates) {
            restored.insert(format!("mcp:{}", old.id), format!("mcp:{here}"));
        }
    }
    let apis_here = to.apis.map(ApiHub::entries).unwrap_or_default();
    for item in picked(Kind::Api).filter(added) {
        let old = &plan.contents.apis[item.index].entry;
        let candidates = apis_here
            .iter()
            .filter(|e| e.base_url == old.base_url)
            .map(|e| (e.id.clone(), e.name == old.name))
            .collect();
        if let Some(here) = pick(candidates) {
            restored.insert(format!("api:{}", old.id), format!("api:{here}"));
        }
    }
    for program in &plan.contents.commands {
        if let Some(key) = restored.get(&program.key) {
            let mut program = program.clone();
            program.key = key.clone();
            to.broker.run(|c| {
                c.commands.restore(program);
                c.touch();
            });
        }
    }
    to.broker.run(|c| c.settle_calls(None));

    outcomes
}

async fn restore_connector(
    plan: &Plan,
    item: &crate::plan::Item,
    to: &Targets<'_>,
) -> Result<String, String> {
    let saved = &plan.contents.connectors[item.index];
    let e = &saved.entry;
    let hub = to.connectors;
    if item.check == Check::Secret {
        let id = item.here.as_deref().ok_or("It is no longer here.")?;
        let here = hub.get(id).ok_or("It is no longer here.")?;
        return match &here.launch {
            Some(launch) => {
                let mut env = hub.env(id);
                for (k, v) in &saved.env {
                    if env.get(k).is_none_or(String::is_empty) {
                        env.insert(k.clone(), v.clone());
                    }
                }
                hub.edit_stdio(id, &here.name, here.command.as_deref(), launch, Some(env))
                    .await?;
                Ok("Its variables restored.".into())
            }
            None => {
                match (&saved.oauth, &saved.token) {
                    (Some(grant), _) => hub.set_oauth(id, grant)?,
                    (None, Some(token)) => hub.set_token(id, Some(token))?,
                    (None, None) => return Ok("Already here.".into()),
                }
                Ok("Its token restored.".into())
            }
        };
    }
    let warned = matches!(item.check, Check::Warn(_));
    let entry = clak_connectors::ConnectorEntry {
        enabled: e.enabled && !warned,
        ..e.clone()
    };
    hub.restore(
        &entry,
        saved.token.as_deref(),
        saved.oauth.as_ref(),
        &saved.env,
    )?;
    Ok(if warned {
        "Restored turned off: change its command line, then turn it on.".into()
    } else if !e.enabled {
        "Restored, turned off as it was.".into()
    } else {
        "Restored.".into()
    })
}

async fn restore_api(
    plan: &Plan,
    item: &crate::plan::Item,
    hub: &ApiHub,
) -> Result<String, String> {
    let saved = &plan.contents.apis[item.index];
    if item.check == Check::Secret {
        let id = item.here.as_deref().ok_or("It is no longer here.")?;
        let here = hub.get(id).ok_or("It is no longer here.")?;
        hub.edit(id, draft(&here, saved.secret.clone())).await?;
        return Ok("Its secret restored.".into());
    }
    let added = hub.add(draft(&saved.entry, saved.secret.clone()))?;
    if !saved.entry.enabled {
        hub.set_enabled(&added.id, false).await?;
        return Ok("Restored, turned off as it was.".into());
    }
    Ok("Restored.".into())
}

fn draft(entry: &clak_apis::ApiEntry, secret: Option<String>) -> ApiDraft {
    ApiDraft {
        name: entry.name.clone(),
        base_url: entry.base_url.clone(),
        auth: entry.auth.clone(),
        secret,
        endpoints: entry.endpoints.clone(),
        command: entry.command.clone(),
        source: entry.source.clone(),
        grant: None,
    }
}

fn outcome(item: &crate::plan::Item, result: Result<String, String>) -> Outcome {
    let (ok, message) = match result {
        Ok(message) => (true, message),
        Err(message) => (false, message),
    };
    Outcome {
        id: item.id.clone(),
        kind: item.kind,
        name: item.name.clone(),
        ok,
        message,
    }
}
