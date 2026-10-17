//! REST APIs: listing them, adding them (from an OpenAPI or by hand), changing them,
//! changing their endpoints, removing them.

use crate::args::Args;
use crate::{
    Answer, HubSettings, Stop, clean, command, find, json_text, row_of, rows, say, secret, some,
    str_of, unencrypted,
};
use clak_apis::{ApiAuth, ApiDraft, ApiEndpoint, ApiEntry, ApiHub, ApiImport, AuthKind};
use clak_connectors::same_origin;
use clak_hub::{ConsentLine, ConsentTool, SettingsChange, cli_names};
use clak_wire::json::truthy;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use tokio_util::sync::CancellationToken;

/// The access fields: if the agent passes one, the access is rebuilt.
const AUTH_FIELDS: [&str; 6] = [
    "auth",
    "auth_name",
    "authorization_url",
    "token_url",
    "scope",
    "client_id",
];

impl HubSettings {
    pub(crate) async fn add_api(
        &self,
        call_id: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        let source = args.text("openapi")?.map(|s| s.trim().to_owned());
        let mut import: Option<ApiImport> = None;
        let endpoints: Vec<ApiEndpoint>;
        if let Some(source) = source.as_deref().filter(|s| !s.is_empty()) {
            if args.has("endpoints") {
                return Err(say(
                    "Either --openapi or --endpoints: endpoints can be added afterwards with set-endpoint.",
                ));
            }
            let read = tokio::select! {
                read = self.apis.import(source) => read,
                _ = cancel.cancelled() => return Err(Stop::Canceled),
            };
            let read = read.map_err(|e| say(e.0))?;
            let mut all = read.endpoints.clone();
            if let Some(only) = args.list("only") {
                let wanted: HashSet<String> = only
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|n| n.trim().to_owned())
                    .filter(|n| !n.is_empty())
                    .collect();
                // The first missing one, in the order the agent wrote them.
                let unknown = only
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .find(|n| !n.is_empty() && all.iter().all(|e| e.name != *n));
                if let Some(unknown) = unknown {
                    let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
                    return Err(say(format!(
                        "The document has no endpoint “{unknown}”. Its endpoints: {}.",
                        some(&names)
                    )));
                }
                all.retain(|e| wanted.contains(&e.name));
            }
            endpoints = all;
            import = Some(read);
        } else {
            if args.has("only") {
                return Err(say("--only goes with --openapi."));
            }
            endpoints = match args.list("endpoints") {
                Some(list) => read_endpoints(&list)?,
                None => {
                    return Err(say(
                        "Pass --openapi with the address of an OpenAPI document, or --endpoints with the endpoints (see -h).",
                    ));
                }
            };
        }

        let Some(name) = args
            .text("name")?
            .or_else(|| import.as_ref().map(|i| i.name.clone()))
        else {
            return Err(say("--name is required."));
        };
        let Some(base_url) = args
            .text("base_url")?
            .map(|u| u.trim().to_owned())
            .or_else(|| import.as_ref().and_then(|i| i.base_url.clone()))
        else {
            return Err(say(if import.is_none() {
                "--base-url is required."
            } else {
                "The document doesn't say the service's address: pass --base-url."
            }));
        };
        let basis = import.as_ref().map(|i| i.auth.clone()).unwrap_or_default();
        let auth = auth_from(args, &basis)?;
        let secret = secret_for(&auth, secret(args.text("secret")?))?;
        let draft = ApiDraft {
            name,
            base_url,
            auth: auth.clone(),
            secret: secret.clone(),
            endpoints: endpoints.clone(),
            command: command(args.text("command")?),
            source: import.as_ref().and(source.clone()),
            grant: None,
        };
        if let Some(problem) = ApiHub::problem(&draft) {
            return Err(say(problem));
        }

        let mut lines = vec![
            ConsentLine::new("Name", draft.name.as_str()),
            ConsentLine::new("Base URL", draft.base_url.trim().trim_end_matches('/')),
            ConsentLine::new(
                "Command",
                format!(
                    "clak {}",
                    draft
                        .command
                        .clone()
                        .unwrap_or_else(|| cli_names::from(&draft.name))
                ),
            ),
            ConsentLine::new("Access", access(&auth, secret.is_some(), false)),
        ];
        if let Some(import) = &import {
            lines.push(ConsentLine::new(
                "OpenAPI",
                format!("{} ({})", source.as_deref().unwrap_or(""), import.version),
            ));
        }
        lines.push(ConsentLine::new("Endpoints", endpoints.len().to_string()));
        let change = SettingsChange {
            title: format!("Add the REST API “{}”", clean(&draft.name)),
            action: "add this REST API".into(),
            lines,
            tools: endpoints.iter().map(|e| line(e, None)).collect(),
            warning: unencrypted(&draft.base_url, secret.is_some()),
        };
        self.confirm(call_id, change, cancel).await?;

        let entry = self.apis.add(draft).map_err(Stop::Failed)?;
        let mut row = api_row(&self.api_row_of(&entry.id)?);
        if let Some(import) = &import
            && !import.skipped.is_empty()
        {
            row["skipped"] = json!(import.skipped);
        }
        Ok(json_text(&next(row, &entry)))
    }

    pub(crate) async fn edit_api(
        &self,
        call_id: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        let old = self.find_api(&args.required("api")?)?;
        let name = args.text("name")?.unwrap_or_else(|| old.name.clone());
        let base_url = args
            .text("base_url")?
            .map(|u| u.trim().trim_end_matches('/').to_owned())
            .unwrap_or_else(|| old.base_url.clone());
        let command = if args.has("command") {
            command(args.text("command")?)
        } else {
            old.command.clone()
        };
        let auth = if AUTH_FIELDS.iter().any(|f| args.has(f)) {
            auth_from(args, &old.auth)?
        } else {
            old.auth.clone()
        };
        let given = secret_for(&auth, secret(args.text("secret")?))?;
        let drop = args.bool("remove_secret")? == Some(true);
        if given.is_some() && drop {
            return Err(say("--secret and --remove-secret together: choose one."));
        }
        let endpoints = match args.list("endpoints") {
            Some(list) => read_endpoints(&list)?,
            None => old.endpoints.clone(),
        };
        let enabled = args.bool("enabled")?;

        // A saved secret doesn't follow an agent: not to another host, and not into a changed
        // OAuth (a new token address would receive the client and its secret on the next
        // sign-in).
        let saved = self.apis.has_secret(&old.id);
        let moved = !same_origin(&old.base_url, &base_url);
        let oauth_changed = (old.auth.is_oauth() || auth.is_oauth()) && auth != old.auth;
        let mut secret = given.clone().or_else(|| drop.then(String::new));
        let left = secret.is_none() && saved && (moved || oauth_changed);
        if left {
            secret = Some(String::new());
        }

        let draft = ApiDraft {
            name: name.clone(),
            base_url: base_url.clone(),
            auth: auth.clone(),
            secret,
            endpoints: endpoints.clone(),
            command: command.clone(),
            source: old.source.clone(),
            grant: None,
        };
        if let Some(problem) = ApiHub::problem(&draft) {
            return Err(say(problem));
        }

        let cli = self.api_cli(&old);
        let mut lines = vec![ConsentLine::new(
            "API",
            format!("{} (clak {cli})", old.name),
        )];
        if name != old.name {
            lines.push(ConsentLine::new("Name", format!("{} → {name}", old.name)));
        }
        if base_url != old.base_url {
            lines.push(ConsentLine::new(
                "Base URL",
                format!("{} → {base_url}", old.base_url),
            ));
        }
        if command != old.command {
            lines.push(ConsentLine::new(
                "Command",
                format!(
                    "clak {cli} → clak {}",
                    command.clone().unwrap_or_else(|| cli_names::from(&name))
                ),
            ));
        }
        if auth != old.auth {
            lines.push(ConsentLine::new(
                "Access",
                format!("{} → {}", old.auth.label(), auth.label()),
            ));
        }
        if given.is_some() {
            lines.push(ConsentLine::new(
                "Secret",
                if saved {
                    "replaced by one given by the agent"
                } else {
                    "given by the agent"
                },
            ));
        } else if drop && saved {
            lines.push(ConsentLine::new("Secret", "the saved one is removed"));
        } else if left {
            lines.push(ConsentLine::new(
                "Secret",
                if auth.is_oauth() {
                    "the saved sign-in is dropped: you sign in again from the panel"
                } else {
                    "the saved one stays behind: it isn't sent to the new host"
                },
            ));
        }
        let diff = diff(&old.endpoints, &endpoints);
        if !diff.is_empty() {
            lines.push(ConsentLine::new(
                "Endpoints",
                format!("{} → {}", old.endpoints.len(), endpoints.len()),
            ));
        }
        let edits = lines.len() > 1;
        if let Some(on) = enabled
            && on != old.enabled
        {
            lines.push(ConsentLine::new(
                "State",
                if on { "turned on" } else { "turned off" },
            ));
        }
        if lines.len() == 1 {
            return Err(say("Nothing to change: pass what should change (see -h)."));
        }

        let change = SettingsChange {
            title: format!("Change the REST API “{}”", clean(&old.name)),
            action: "make this change".into(),
            lines,
            tools: diff,
            warning: unencrypted(&base_url, given.is_some() || (saved && !drop && !left)),
        };
        self.confirm(call_id, change, cancel).await?;

        self.unchanged(&old)?;
        if enabled == Some(false) {
            self.apis
                .set_enabled(&old.id, false)
                .await
                .map_err(Stop::Failed)?;
        }
        if edits {
            self.apis.edit(&old.id, draft).await.map_err(Stop::Failed)?;
        }
        if enabled == Some(true) {
            self.apis
                .set_enabled(&old.id, true)
                .await
                .map_err(Stop::Failed)?;
        }
        let row = api_row(&self.api_row_of(&old.id)?);
        Ok(json_text(&next(
            row,
            &self.apis.get(&old.id).unwrap_or(old),
        )))
    }

    pub(crate) async fn set_endpoint(
        &self,
        call_id: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        let old = self.find_api(&args.required("api")?)?;
        let Some(given) = args.node("endpoint") else {
            return Err(say("--endpoint is required."));
        };
        let list = match given {
            Value::Array(many) => many.clone(),
            one => vec![one.clone()],
        };
        let mut read = read_endpoints(&list)?;
        if read.len() != 1 {
            return Err(say(
                "--endpoint is one endpoint: for more, call it again or use edit-api --endpoints.",
            ));
        }
        let endpoint = read.remove(0);
        let mut endpoints = old.endpoints.clone();
        match endpoints.iter_mut().find(|e| e.name == endpoint.name) {
            Some(same) => *same = endpoint,
            None => endpoints.push(endpoint),
        }
        self.change_endpoints(call_id, old, endpoints, cancel).await
    }

    pub(crate) async fn remove_endpoint(
        &self,
        call_id: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        let old = self.find_api(&args.required("api")?)?;
        let name = args.required("name")?;
        if old.endpoints.iter().all(|e| e.name != name) {
            let names: Vec<&str> = old.endpoints.iter().map(|e| e.name.as_str()).collect();
            return Err(say(format!(
                "“{}” has no endpoint “{name}”. Its endpoints: {}.",
                old.name,
                some(&names)
            )));
        }
        if old.endpoints.len() == 1 {
            return Err(say(
                "It's the only endpoint: an API needs at least one (remove-api removes the API).",
            ));
        }
        let endpoints = old
            .endpoints
            .iter()
            .filter(|e| e.name != name)
            .cloned()
            .collect();
        self.change_endpoints(call_id, old, endpoints, cancel).await
    }

    /// An API's new endpoints, and nothing else: permissions stay, a new or changed command
    /// is asked again.
    async fn change_endpoints(
        &self,
        call_id: &str,
        old: ApiEntry,
        endpoints: Vec<ApiEndpoint>,
        cancel: &CancellationToken,
    ) -> Answer {
        let draft = ApiDraft {
            name: old.name.clone(),
            base_url: old.base_url.clone(),
            auth: old.auth.clone(),
            secret: None,
            endpoints: endpoints.clone(),
            command: old.command.clone(),
            source: old.source.clone(),
            grant: None,
        };
        if let Some(problem) = ApiHub::problem(&draft) {
            return Err(say(problem));
        }
        let diff = diff(&old.endpoints, &endpoints);
        if diff.is_empty() {
            return Err(say("The endpoint is already exactly like that."));
        }
        let change = SettingsChange {
            title: format!("Change the endpoints of “{}”", clean(&old.name)),
            action: "make this change".into(),
            lines: vec![
                ConsentLine::new("API", format!("{} (clak {})", old.name, self.api_cli(&old))),
                ConsentLine::new(
                    "Endpoints",
                    format!("{} → {}", old.endpoints.len(), endpoints.len()),
                ),
            ],
            tools: diff,
            warning: None,
        };
        self.confirm(call_id, change, cancel).await?;
        self.unchanged(&old)?;
        self.apis.edit(&old.id, draft).await.map_err(Stop::Failed)?;
        let row = api_row(&self.api_row_of(&old.id)?);
        Ok(json_text(&next(
            row,
            &self.apis.get(&old.id).unwrap_or(old),
        )))
    }

    pub(crate) async fn remove_api(
        &self,
        call_id: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        let old = self.find_api(&args.required("api")?)?;
        let mut lines = vec![
            ConsentLine::new("API", format!("{} (clak {})", old.name, self.api_cli(&old))),
            ConsentLine::new("Base URL", old.base_url.as_str()),
            ConsentLine::new("Endpoints", old.endpoints.len().to_string()),
        ];
        if self.apis.has_secret(&old.id) {
            lines.push(ConsentLine::new(
                "Secret",
                if old.auth.is_oauth() {
                    "its sign-in is deleted"
                } else {
                    "the saved one is deleted"
                },
            ));
        }
        let change = SettingsChange {
            title: format!("Remove the REST API “{}”", clean(&old.name)),
            action: "remove this REST API".into(),
            lines,
            tools: vec![],
            warning: None,
        };
        self.confirm(call_id, change, cancel).await?;
        self.apis.remove(&old.id).await.map_err(Stop::Failed)?;
        Ok(json_text(&json!({ "removed": old.name })))
    }

    /// Applies only to what the user saw: if it changed in the meantime, read it again.
    fn unchanged(&self, seen: &ApiEntry) -> Result<(), Stop> {
        if self.apis.get(&seen.id).as_ref() != Some(seen) {
            return Err(say(format!(
                "“{}” changed while the user was deciding: read it again (show-api) and retry.",
                seen.name
            )));
        }
        Ok(())
    }

    pub(crate) fn find_api(&self, key: &str) -> Result<ApiEntry, Stop> {
        let rows = rows(&self.apis.view());
        find(
            &self.apis.entries(),
            key,
            |e| e.id.as_str(),
            |e| cli_of(&rows, e),
            |e| e.name.as_str(),
            "REST API",
            "apis",
        )
    }

    fn api_cli(&self, entry: &ApiEntry) -> String {
        cli_of(&rows(&self.apis.view()), entry)
    }

    fn api_row_of(&self, id: &str) -> Result<Map<String, Value>, Stop> {
        row_of(&rows(&self.apis.view()), id)
            .cloned()
            .ok_or_else(|| say("The API was removed in the meantime."))
    }

    /// A whole API, in the same JSON that edit-api and set-endpoint accept. Secrets aren't in it.
    pub(crate) fn show_api(&self, entry: &ApiEntry) -> Result<Value, Stop> {
        let mut row = api_row(&self.api_row_of(&entry.id)?);
        row["auth"] = compact(entry.auth.to_json());
        row["endpoints"] = Value::Array(
            entry
                .endpoints
                .iter()
                .map(|e| {
                    let mut json = compact(e.to_json());
                    if let Some(o) = json.as_object_mut() {
                        if !truthy(o.get("body_required")) {
                            o.remove("body_required");
                        }
                        if let Some(Value::Array(parameters)) = o.get_mut("parameters") {
                            for p in parameters.iter_mut() {
                                *p = compact(p.take());
                            }
                        }
                    }
                    json
                })
                .collect(),
        );
        Ok(row)
    }
}

/// Without the keys whose value is `null`.
fn compact(value: Value) -> Value {
    match value {
        Value::Object(o) => Value::Object(o.into_iter().filter(|(_, v)| !v.is_null()).collect()),
        other => other,
    }
}

/// An API's command: the one it has in the core, or the one it will have.
fn cli_of(rows: &[Map<String, Value>], entry: &ApiEntry) -> String {
    row_of(rows, &entry.id)
        .and_then(|r| str_of(r, "cli"))
        .map(str::to_owned)
        .or_else(|| entry.command.clone())
        .unwrap_or_else(|| cli_names::from(&entry.name))
}

/// An API for the agent: the access, and whether the secret exists; not the secret.
pub(crate) fn api_row(row: &Map<String, Value>) -> Value {
    let auth = str_of(row, "auth");
    let label = str_of(row, "auth_label").unwrap_or("");
    let access = match auth {
        None | Some("none") => str_of(row, "auth_label").map(str::to_owned),
        Some(kind) => {
            let missing = if truthy(row.get("has_secret")) {
                ""
            } else if kind == AuthKind::OAuth.as_str() || kind == AuthKind::OAuthApp.as_str() {
                ", not signed in"
            } else {
                ", no secret saved"
            };
            Some(format!("{label}{missing}"))
        }
    };
    json!({
        "name": str_of(row, "name"),
        "command": str_of(row, "cli")
            .or_else(|| str_of(row, "command"))
            .map(str::to_owned)
            .unwrap_or_else(|| cli_names::from(str_of(row, "name").unwrap_or(""))),
        "base_url": str_of(row, "base_url"),
        "enabled": truthy(row.get("enabled")),
        "state": str_of(row, "state"),
        "access": access,
        "endpoints": row.get("endpoints").cloned().unwrap_or(Value::Null),
        "openapi": str_of(row, "source"),
        "id": str_of(row, "id"),
    })
}

/// What to do now, as told to the agent: an API ready, or what's missing.
fn next(mut api: Value, entry: &ApiEntry) -> Value {
    let name = api["name"].as_str().unwrap_or("").to_owned();
    let text = if !entry.enabled {
        "Turned off: agents don't see it until it's turned on again.".to_owned()
    } else if api["state"].as_str() == Some("sign-in required") {
        if entry.auth.kind == AuthKind::OAuthApp {
            format!(
                "OAuth needs the client secret: ask the user to open Clak, APIs tab, and choose Sign in on “{name}”."
            )
        } else {
            format!(
                "Sign-in needed: ask the user to open Clak, APIs tab, and choose Sign in on “{name}”."
            )
        }
    } else if api["access"]
        .as_str()
        .is_some_and(|a| a.ends_with("no secret saved"))
    {
        format!(
            "No secret saved: pass it with edit-api --secret, or ask the user to add it (Clak, APIs tab, Edit on “{name}”)."
        )
    } else {
        format!(
            "Ready: `clak {} list` shows its commands.",
            api["command"].as_str().unwrap_or("")
        )
    };
    api["next"] = text.into();
    api
}

// --- access ----------------------------------------------------------------

/// The access from the previous one (or from an OpenAPI) and from what the agent changes. A
/// different type starts from scratch.
fn auth_from(args: &Args<'_>, basis: &ApiAuth) -> Result<ApiAuth, Stop> {
    let kind = match args.text("auth")? {
        Some(text) => text.trim().to_lowercase(),
        None => basis.kind.as_str().to_owned(),
    };
    let Some(kind) = AuthKind::parse(&kind) else {
        let kinds: Vec<&str> = AuthKind::ALL.iter().map(|k| k.as_str()).collect();
        return Err(say(format!("--auth is one of: {}.", kinds.join(", "))));
    };
    let keep = kind == basis.kind;
    let pick = |flag: &str, old: &Option<String>| -> Result<Option<String>, Stop> {
        if args.has(flag) {
            Ok(args
                .text(flag)?
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()))
        } else if keep {
            Ok(old.clone())
        } else {
            Ok(None)
        }
    };
    let name = pick("auth_name", &basis.name)?;
    let authorize = pick("authorization_url", &basis.authorization_url)?;
    let token = pick("token_url", &basis.token_url)?;
    let scope = pick("scope", &basis.scope)?;
    let client = pick("client_id", &basis.client_id)?;
    Ok(match kind {
        AuthKind::None | AuthKind::Bearer => ApiAuth::new(kind),
        AuthKind::Header | AuthKind::Query | AuthKind::Basic => ApiAuth {
            name,
            ..ApiAuth::new(kind)
        },
        AuthKind::OAuthApp => ApiAuth {
            token_url: token,
            scope,
            client_id: client,
            ..ApiAuth::new(kind)
        },
        AuthKind::OAuth => ApiAuth {
            authorization_url: authorize,
            token_url: token,
            scope,
            client_id: client,
            ..ApiAuth::new(kind)
        },
    })
}

/// A secret from the agent: only where there's a place for it, and never for OAuth (the user signs in).
fn secret_for(auth: &ApiAuth, secret: Option<String>) -> Result<Option<String>, Stop> {
    let Some(secret) = secret else {
        return Ok(None);
    };
    match auth.kind {
        AuthKind::None => Err(say(
            "This API has no authentication: the secret has nowhere to go (set --auth).",
        )),
        AuthKind::OAuth | AuthKind::OAuthApp => Err(say(
            "With OAuth the user signs in from the panel (Clak, APIs tab, Sign in): there's no secret to pass.",
        )),
        _ => Ok(Some(secret)),
    }
}

fn access(auth: &ApiAuth, secret: bool, had_secret: bool) -> String {
    match auth.kind {
        AuthKind::None => auth.label(),
        AuthKind::OAuth | AuthKind::OAuthApp => format!(
            "{}, client {}: you sign in from the panel",
            auth.label(),
            auth.client_id.as_deref().unwrap_or("")
        ),
        _ if secret => format!("{}, with a secret given by the agent", auth.label()),
        _ if had_secret => auth.label(),
        _ => format!("{}, no secret yet: you add it from the panel", auth.label()),
    }
}

// --- endpoints -------------------------------------------------------------

fn read_endpoints(list: &[Value]) -> Result<Vec<ApiEndpoint>, Stop> {
    flatten(list)
        .iter()
        .map(|item| ApiEndpoint::read(item).map_err(say))
        .collect()
}

/// From the command line an object can arrive as text, and a list as a list inside a
/// list.
fn flatten(list: &[Value]) -> Vec<Value> {
    let mut items = Vec::new();
    for item in list {
        let node = match item {
            Value::String(text) => {
                clak_wire::json::parse_str(text).unwrap_or_else(|_| item.clone())
            }
            _ => item.clone(),
        };
        match node {
            Value::Array(inner) => items.extend(inner),
            other => items.push(other),
        }
    }
    items
}

fn line(e: &ApiEndpoint, note: Option<&str>) -> ConsentTool {
    let summary = e
        .description
        .as_deref()
        .and_then(|d| d.split('\n').next())
        .map(str::trim)
        .unwrap_or("");
    ConsentTool {
        name: e.name.clone(),
        description: format!(
            "{} {}{}",
            e.method,
            e.path,
            if summary.is_empty() {
                String::new()
            } else {
                format!(" — {summary}")
            }
        ),
        note: note.map(str::to_owned),
    }
}

/// The new, changed and removed endpoints, for the confirmation window.
fn diff(before: &[ApiEndpoint], after: &[ApiEndpoint]) -> Vec<ConsentTool> {
    let mut old: HashMap<&str, &ApiEndpoint> = HashMap::new();
    for e in before {
        old.entry(e.name.as_str()).or_insert(e);
    }
    let mut list = Vec::new();
    for e in after {
        match old.get(e.name.as_str()) {
            None => list.push(line(e, Some("new"))),
            Some(was)
                if clak_wire::json::text(&was.to_json()) != clak_wire::json::text(&e.to_json()) =>
            {
                list.push(line(e, Some("changed")))
            }
            Some(_) => {}
        }
    }
    for e in before
        .iter()
        .filter(|e| after.iter().all(|a| a.name != e.name))
    {
        list.push(line(e, Some("removed")));
    }
    list
}
