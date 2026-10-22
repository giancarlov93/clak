//! The Agents tab: the agents installed here, launched from the app, their instructions
//! and the presets. Finding and launching live in `clak_setup::launcher`, the presets in
//! `clak_setup::presets`, the authorization in `clak_hub::launch`: here only the commands.
//!
//! A launch is prepared on the hub before the agent starts, so that it is authorized when
//! it shows up. With the hub in another process the agent still starts, and asks as usual.

use crate::bridge;
use clak_hub::launch::{Preset, PresetProgram};
use clak_setup::{Places, conversations, instructions, launcher, presets};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

/// The agents installed (with their global instructions file, for those that have one), the
/// presets, and whether a launch is authorized (hub in here).
#[tauri::command]
pub async fn launch_status(app: AppHandle) -> Value {
    let bridge = bridge::of(&app);
    let home = bridge.home().clone();
    let authorizes = bridge.mode() == bridge::Mode::Local;
    tauri::async_runtime::spawn_blocking(move || {
        let places = Places::real();
        json!({
            "installed": launcher::installed(&places)
                .iter()
                .map(|agent| {
                    let mut json = agent.to_json();
                    json["instructions"] = json!(instructions::global(&places, &agent.id));
                    json["conversations"] = json!(conversations::supported(&agent.id));
                    json["conversations_editable"] = json!(conversations::editable(&agent.id));
                    json["project_file"] = json!(instructions::project(&agent.id, std::path::Path::new(""))
                        .map(|p| p.to_string_lossy().into_owned()));
                    json
                })
                .collect::<Vec<_>>(),
            "presets": presets::all(&home),
            "authorizes": authorizes,
        })
    })
    .await
    .unwrap_or_else(|_| json!({ "installed": [], "presets": [], "authorizes": false }))
}

/// Launches an agent (by its catalog id) or a preset (by its id): the preset brings its
/// agent, its folder, its programs, its system prompt and its model. `folder`, if given, wins over the preset's.
/// `resume` takes up one of the agent's past conversations: a command-line agent in the folder
/// where it took place, Claude Desktop in the app.
#[tauri::command]
pub async fn launch_agent(
    app: AppHandle,
    agent: Option<String>,
    preset: Option<String>,
    folder: Option<String>,
    resume: Option<String>,
) -> Result<(), String> {
    let bridge = bridge::of(&app);
    let home = bridge.home().clone();
    let places = Places::real();
    let (installed, chosen) = tauri::async_runtime::spawn_blocking({
        let (home, places, preset) = (home.clone(), places.clone(), preset.clone());
        move || {
            let chosen = preset.as_deref().and_then(|id| presets::get(&home, id));
            (launcher::installed(&places), chosen)
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    if preset.is_some() && chosen.is_none() {
        return Err("this preset no longer exists".into());
    }
    let agent_id = chosen
        .as_ref()
        .and_then(|p| p.get("agent").and_then(Value::as_str).map(str::to_owned))
        .or(agent)
        .ok_or("no agent chosen")?;
    let agent = launcher::find(&installed, &agent_id)
        .cloned()
        .ok_or("this agent is not installed on this computer")?;
    let folder = folder
        .filter(|f| !f.is_empty())
        .or_else(|| {
            chosen
                .as_ref()
                .and_then(|p| p.get("folder").and_then(Value::as_str).map(str::to_owned))
        })
        .map(PathBuf::from);
    if let Some(id) = resume.clone().filter(|_| agent.app) {
        let (found, agent_id) = (places.clone(), agent.id.clone());
        let link = tauri::async_runtime::spawn_blocking(move || {
            conversations::link(&found, &agent_id, &id)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        // Prepared as a launch of the app: the session that takes up again is authorized.
        bridge.with_hub(|c| c.prepare_launch(&agent.name, false, None));
        use tauri_plugin_opener::OpenerExt;
        return app
            .opener()
            .open_url(link, None::<&str>)
            .map_err(|e| e.to_string());
    }
    if let Some(id) = resume.filter(|_| !agent.app) {
        let (found, agent_id) = (places.clone(), agent.id.clone());
        let (args, folder) = tauri::async_runtime::spawn_blocking(move || {
            conversations::resume(&found, &agent_id, &id)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        let extra = launcher::Extra {
            args,
            ..Default::default()
        };
        return start(&bridge, places, agent, folder, None, extra).await;
    }
    // What the preset adds to a command-line agent, ready before anything waits for it.
    let extra = match chosen.clone().filter(|_| !agent.app) {
        Some(chosen) => {
            let (home, id) = (home.clone(), agent.id.clone());
            tauri::async_runtime::spawn_blocking(move || presets::extra(&home, &chosen, &id))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?
        }
        None => launcher::Extra::default(),
    };
    let preset = chosen.as_ref().map(hub_preset);
    start(&bridge, places, agent, folder, preset, extra).await
}

/// Prepares the launch on the hub, then starts the agent: its first request finds the launch
/// waiting.
async fn start(
    bridge: &bridge::Bridge,
    places: Places,
    agent: launcher::Installed,
    folder: Option<PathBuf>,
    preset: Option<Preset>,
    extra: launcher::Extra,
) -> Result<(), String> {
    let home = bridge.home().clone();
    let token = bridge
        .with_hub(|c| c.prepare_launch(&agent.name, !agent.app, preset))
        .flatten();
    tauri::async_runtime::spawn_blocking(move || {
        launcher::launch(
            &places,
            &agent,
            folder.as_deref(),
            token.as_deref(),
            &extra,
            &home.join("launch"),
        )
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// The preset as the hub wants it: its name and its programs.
fn hub_preset(preset: &Map<String, Value>) -> Preset {
    Preset {
        name: preset
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        programs: preset
            .get("programs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(PresetProgram::from_json)
            .collect(),
    }
}

#[tauri::command]
pub async fn preset_save(app: AppHandle, preset: Map<String, Value>) -> Result<Value, String> {
    let home = bridge::of(&app).home().clone();
    tauri::async_runtime::spawn_blocking(move || presets::save(&home, preset))
        .await
        .map_err(|e| e.to_string())?
        .map(Value::Object)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn preset_remove(app: AppHandle, id: String) -> Result<(), String> {
    let home = bridge::of(&app).home().clone();
    tauri::async_runtime::spawn_blocking(move || presets::remove(&home, &id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// The instructions file of an agent: its global one, or the one in `folder`.
fn instructions_file(agent: &str, folder: Option<&str>) -> Result<PathBuf, String> {
    match folder.filter(|f| !f.is_empty()) {
        Some(folder) => instructions::project(agent, std::path::Path::new(folder))
            .ok_or_else(|| "this agent has no instructions file in a folder".into()),
        None => instructions::global(&Places::real(), agent)
            .ok_or_else(|| "this agent has no instructions file of its own".into()),
    }
}

/// An instructions file: where it is and its text, empty if there is none yet.
#[tauri::command]
pub async fn instructions_read(agent: String, folder: Option<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = instructions_file(&agent, folder.as_deref())?;
        let text = instructions::read(&path).map_err(|e| e.to_string())?;
        Ok(json!({ "path": path, "text": text, "exists": path.is_file() }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn instructions_write(
    agent: String,
    folder: Option<String>,
    text: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = instructions_file(&agent, folder.as_deref())?;
        instructions::write(&path, &text).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The past conversations of a command-line agent, the latest first.
#[tauri::command]
pub async fn conversations_list(agent: String) -> Result<Vec<Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        conversations::list(&Places::real(), &agent)
            .map(|all| {
                all.iter()
                    .map(conversations::Conversation::to_json)
                    .collect()
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn conversations_rename(agent: String, id: String, title: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        conversations::rename(&Places::real(), &agent, &id, &title).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Deletes one of the agent's past conversations, for good.
#[tauri::command]
pub async fn conversations_delete(agent: String, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        conversations::delete(&Places::real(), &agent, &id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Opens the folder where a conversation took place, in Finder or File Explorer. The page
/// names the conversation, not the folder: only a conversation's own folder opens.
#[tauri::command]
pub async fn conversations_open_folder(
    app: AppHandle,
    agent: String,
    id: String,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let folder = tauri::async_runtime::spawn_blocking(move || {
        conversations::folder(&Places::real(), &agent, &id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??
    .ok_or("This conversation doesn't say where it took place.")?;
    app.opener()
        .open_path(folder.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// The folder an agent works in, chosen by the user.
#[tauri::command]
pub async fn launch_choose_folder(window: WebviewWindow) -> Option<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_title("The folder the agent works in")
        .set_parent(&window)
        .pick_folder(move |path| {
            let _ = sender.send(path);
        });
    let path = receiver.await.ok().flatten()?;
    path.into_path()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}
