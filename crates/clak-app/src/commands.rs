//! What the pages can ask for. Nothing here executes what a page writes: the messages for
//! the hub go through the same checks as the control channel.

use crate::bridge;
use serde_json::{Map, Value, json};
use tauri::{AppHandle, WebviewWindow};

/// How the app is connected, the hub state and the log backlog.
#[tauri::command]
pub fn hub_hello(app: AppHandle) -> Value {
    bridge::of(&app).hello()
}

/// Revoke, forget, describe, history, a call in full: the replies arrive as a `hub` event.
#[tauri::command]
pub fn hub_send(app: AppHandle, message: Map<String, Value>) {
    const ALLOWED: [&str; 6] = [
        "revoke",
        "forget_session",
        "forget_app",
        "describe",
        "history",
        "call",
    ];
    if message
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| ALLOWED.contains(&t))
    {
        bridge::of(&app).send(message);
    }
}

/// The question a consent window shows.
#[tauri::command]
pub fn consent_request(app: AppHandle, id: String) -> Option<Value> {
    bridge::of(&app).prompt().request(&id)
}

/// The user's answer. It only counts from that question's window.
#[tauri::command]
pub fn consent_answer(app: AppHandle, window: WebviewWindow, id: String, allow: bool) {
    if window.label() == crate::prompt::label(&id) {
        bridge::of(&app).prompt().answer(&id, allow);
    }
}

#[tauri::command]
pub fn consent_resize(app: AppHandle, window: WebviewWindow, id: String, height: f64) {
    if window.label() == crate::prompt::label(&id) {
        bridge::of(&app).prompt().resize(&id, height);
    }
}

#[tauri::command]
pub fn open_panel(app: AppHandle, tab: Option<String>) {
    crate::windows::hide_flyout(&app);
    crate::windows::panel(&app, tab.as_deref());
}

/// Starts one of the allowed applications. The page names it by its key, not by its path:
/// only what the hub already remembers can be started.
#[tauri::command]
pub fn app_launch(app: AppHandle, key: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let state = bridge::of(&app).state();
    let exe = state
        .get("apps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|a| a.get("key").and_then(Value::as_str) == Some(key.as_str()))
        .and_then(|a| a.get("exe").and_then(Value::as_str))
        .ok_or("this application is no longer among the allowed ones")?;
    // On the Mac the app, not the executable inside it: opening that would start a Terminal.
    #[cfg(target_os = "macos")]
    let target = crate::app_icon::bundle_of(std::path::Path::new(exe))
        .map_or(exe.to_owned(), |b| b.to_string_lossy().into_owned());
    #[cfg(not(target_os = "macos"))]
    let target = exe.to_owned();
    app.opener()
        .open_path(target, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn quit(app: AppHandle) {
    crate::windows::hide_flyout(&app);
    crate::quit(app, true);
}

#[tauri::command]
pub fn prefs_get(app: AppHandle) -> Value {
    let bridge = bridge::of(&app);
    json!({
        "notifications": bridge.prefs.notifications(),
        "consent_sound": bridge.prefs.consent_sound(),
        "autostart": crate::setup::autostart_on(),
    })
}

#[tauri::command]
pub fn prefs_set(app: AppHandle, name: String, value: bool) -> Result<(), String> {
    let bridge = bridge::of(&app);
    match name.as_str() {
        "notifications" => bridge.prefs.set_notifications(value).map_err(|e| e.0),
        "consent_sound" => bridge.prefs.set_consent_sound(value).map_err(|e| e.0),
        "autostart" => {
            crate::setup::set_autostart(value)?;
            bridge.prefs.set_autostart(Some(value)).map_err(|e| e.0)
        }
        _ => Err(format!("unknown preference: {name}")),
    }
}
