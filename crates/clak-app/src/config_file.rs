//! Clak's configuration in one file, to take to another computer or keep aside: the presets,
//! the MCP servers and the REST APIs, from Settings › General.
//!
//! No secret goes in it: no token, no key, no sign-in, and of a program's environment
//! variables only the names. After an import each server asks for what it needs, as a new
//! one does. A command line goes as it is: if one carries a key, the panel says so before
//! saving.
//!
//! Nor does a permission: the allowed applications and the "Always allow" rules stay on this
//! computer. They are given only in the consent windows; a file must not be able to give
//! one.

use crate::bridge;
use clak_apis::{ApiAuth, ApiDraft, ApiEndpoint};
use clak_connectors::launch::Launch;
use clak_setup::presets;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

/// What the file says it is, and its version: a file of another kind is refused.
const KIND: &str = "clak-config";
const VERSION: u64 = 1;

/// Writes the configuration where the user chooses: how many of each went in, or `None` if
/// they changed their mind.
#[tauri::command]
pub async fn config_export(app: AppHandle, window: WebviewWindow) -> Result<Option<Value>, String> {
    let bridge = bridge::of(&app);
    let home = bridge.home().clone();
    let connectors: Vec<Value> = bridge
        .connectors()
        .map(|hub| {
            hub.entries()
                .iter()
                .map(|e| {
                    let env: Vec<String> = if e.is_stdio() {
                        hub.env(&e.id).into_keys().collect()
                    } else {
                        Vec::new()
                    };
                    json!({
                        "name": e.name,
                        "url": (!e.url.is_empty()).then_some(&e.url),
                        "launch": e.launch.as_ref().map(|l| json!({ "program": l.program, "args": l.args })),
                        "env": env,
                        "auth": e.auth,
                        "command": e.command,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let apis: Vec<Value> = bridge
        .apis()
        .map(|hub| {
            hub.entries()
                .iter()
                .map(|a| {
                    let mut json = a.to_json();
                    if let Some(o) = json.as_object_mut() {
                        for private in ["id", "since", "enabled"] {
                            o.remove(private);
                        }
                    }
                    json
                })
                .collect()
        })
        .unwrap_or_default();
    let presets: Vec<Value> = presets::all(&home)
        .into_iter()
        .map(|mut p| {
            p.remove("id");
            Value::Object(p)
        })
        .collect();
    let counts =
        json!({ "presets": presets.len(), "connectors": connectors.len(), "apis": apis.len() });
    let content = json!({
        "kind": KIND,
        "version": VERSION,
        "exported": clak_wire::json::float(clak_wire::json::now()),
        "presets": presets,
        "connectors": connectors,
        "apis": apis,
    });
    let Some(path) = pick(&window, true).await else {
        return Ok(None);
    };
    std::fs::write(&path, clak_wire::json::encode_pretty(&content))
        .map_err(|e| format!("Can't write {}: {e}", path.display()))?;
    Ok(Some(
        json!({ "path": path.to_string_lossy(), "counts": counts }),
    ))
}

/// Reads a file the user chooses and adds what isn't here yet: for each item, added, already
/// here, or why not. `None` if they changed their mind.
#[tauri::command]
pub async fn config_import(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Option<Vec<Value>>, String> {
    let Some(path) = pick(&window, false).await else {
        return Ok(None);
    };
    let bytes = std::fs::read(&path).map_err(|e| format!("Can't read {}: {e}", path.display()))?;
    let file: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "This isn't a configuration exported by Clak.".to_owned())?;
    if file.get("kind").and_then(Value::as_str) != Some(KIND) {
        return Err("This isn't a configuration exported by Clak.".into());
    }
    if file.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err("This configuration comes from another version of Clak.".into());
    }
    let bridge = bridge::of(&app);
    let list = |key: &str| {
        file.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut results = Vec::new();

    let connectors = bridge.connectors();
    for item in list("connectors") {
        let name = text(&item, "name").unwrap_or_default();
        let outcome = match &connectors {
            None => Err(crate::editors::READ_ONLY.to_owned()),
            Some(hub) => import_connector(hub, &item),
        };
        results.push(result("MCP server", &name, outcome));
    }
    let apis = bridge.apis();
    for item in list("apis") {
        let name = text(&item, "name").unwrap_or_default();
        let outcome = match &apis {
            None => Err(crate::editors::READ_ONLY.to_owned()),
            Some(hub) => import_api(hub, &item),
        };
        results.push(result("API", &name, outcome));
    }
    let home = bridge.home().clone();
    for item in list("presets") {
        let name = text(&item, "name").unwrap_or_default();
        results.push(result("Preset", &name, import_preset(&home, item)));
    }
    Ok(Some(results))
}

/// What happened to one item: `Ok(true)` added, `Ok(false)` already here.
fn result(kind: &str, name: &str, outcome: Result<bool, String>) -> Value {
    match outcome {
        Ok(added) => json!({ "kind": kind, "name": name, "ok": true, "added": added }),
        Err(problem) => json!({ "kind": kind, "name": name, "ok": false, "message": problem }),
    }
}

fn import_connector(hub: &clak_connectors::ConnectorHub, item: &Value) -> Result<bool, String> {
    let name = text(item, "name").ok_or("It has no name.")?;
    let command = text(item, "command");
    if let Some(launch) = item.get("launch").filter(|l| !l.is_null()) {
        let launch = Launch {
            program: text(launch, "program").ok_or("Its command line has no program.")?,
            args: launch
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        };
        if hub.find_by_launch(&launch).is_some() {
            return Ok(false);
        }
        // The names only: the values were never in the file.
        let env: BTreeMap<String, String> = item
            .get("env")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|k| (k.to_owned(), String::new()))
            .collect();
        hub.add_stdio(&name, &launch, &env, command.as_deref())?;
        return Ok(true);
    }
    let url = text(item, "url").ok_or("It has neither an address nor a command line.")?;
    if hub.find_by_url(&url).is_some() {
        return Ok(false);
    }
    hub.add(&name, &url, None, command.as_deref())?;
    Ok(true)
}

fn import_api(hub: &clak_apis::ApiHub, item: &Value) -> Result<bool, String> {
    let name = text(item, "name").ok_or("It has no name.")?;
    let base_url = text(item, "base_url").ok_or("It has no base URL.")?;
    let here = |url: &str| url.trim().trim_end_matches('/').to_owned();
    if hub
        .entries()
        .iter()
        .any(|a| a.name == name && here(&a.base_url) == here(&base_url))
    {
        return Ok(false);
    }
    let draft = ApiDraft {
        name,
        base_url,
        auth: ApiAuth::from_json(item.get("auth")),
        secret: None,
        endpoints: item
            .get("endpoints")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(ApiEndpoint::from_json).collect())
            .unwrap_or_default(),
        command: text(item, "command"),
        source: text(item, "source"),
        grant: None,
    };
    hub.add(draft)?;
    Ok(true)
}

fn import_preset(home: &std::path::Path, item: Value) -> Result<bool, String> {
    let Value::Object(mut preset) = item else {
        return Err("It isn't a preset.".into());
    };
    let name = preset
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if presets::all(home)
        .iter()
        .any(|p| p.get("name").and_then(Value::as_str) == Some(name.as_str()))
    {
        return Ok(false);
    }
    preset.remove("id");
    presets::save(home, preset)
        .map(|_| true)
        .map_err(|e| e.to_string())
}

fn text(item: &Value, key: &str) -> Option<String> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// Where to save the file, or which one to read.
async fn pick(window: &WebviewWindow, save: bool) -> Option<PathBuf> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let dialog = window
        .dialog()
        .file()
        .add_filter("Clak configuration", &["json"])
        .set_parent(window);
    if save {
        dialog
            .set_title("Export Clak's configuration")
            .set_file_name("Clak configuration.json")
            .save_file(move |path| {
                let _ = sender.send(path);
            });
    } else {
        dialog
            .set_title("Import a Clak configuration")
            .pick_file(move |path| {
                let _ = sender.send(path);
            });
    }
    receiver
        .await
        .ok()
        .flatten()
        .and_then(|p| p.into_path().ok())
}

/// Whether a command line looks like it carries a key: the panel warns before exporting.
#[tauri::command]
pub fn config_secrets_in_commands(app: AppHandle) -> Vec<String> {
    let Some(hub) = bridge::of(&app).connectors() else {
        return Vec::new();
    };
    let looks_secret = |word: &str| {
        let lower = word.to_lowercase();
        ["key", "token", "secret", "password", "bearer"]
            .iter()
            .any(|w| lower.contains(w))
    };
    hub.entries()
        .iter()
        .filter(|e| {
            e.launch
                .as_ref()
                .is_some_and(|l| l.args.iter().any(|a| looks_secret(a)))
        })
        .map(|e| e.name.clone())
        .collect()
}
