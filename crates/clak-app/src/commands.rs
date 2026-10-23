//! What the pages can ask for. Nothing here executes what a page writes: the messages for
//! the hub go through the same checks as the control channel.

use crate::bridge;
use serde_json::{Map, Value, json};
use tauri::{AppHandle, Emitter, WebviewWindow};

/// How the app is connected, the hub state and the log backlog.
#[tauri::command]
pub fn hub_hello(app: AppHandle) -> Value {
    bridge::of(&app).hello()
}

/// Revoke, forget, describe, history, a call in full: the replies arrive as a `hub` event.
#[tauri::command]
pub fn hub_send(app: AppHandle, message: Map<String, Value>) {
    const ALLOWED: [&str; 8] = [
        "revoke",
        "forget_session",
        "forget_app",
        "forget_rule",
        "describe",
        "history",
        "call",
        "usage",
    ];
    if message
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| ALLOWED.contains(&t))
    {
        bridge::of(&app).send(message);
    }
}

/// Runs a program's command as the user, from the Status tab: only from the panel, and only
/// with the hub in here (the control channel never offers it: any process could ask).
#[tauri::command]
pub async fn program_try(
    app: AppHandle,
    window: WebviewWindow,
    resource_id: String,
    tool: String,
    arguments: Map<String, Value>,
) -> Result<Value, String> {
    if window.label() != "panel" {
        return Err("Only from the panel.".into());
    }
    let broker = bridge::of(&app)
        .broker()
        .ok_or("The hub runs in another process: commands can be tried only where it runs.")?;
    Ok(broker.try_command(&resource_id, &tool, arguments).await)
}

/// The question a consent window shows.
#[tauri::command]
pub fn consent_request(app: AppHandle, id: String) -> Option<Value> {
    bridge::of(&app).prompt().request(&id)
}

/// The user's answer. It only counts from that question's window.
#[tauri::command]
pub fn consent_answer(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    allow: bool,
    always: Option<bool>,
) {
    if window.label() == crate::prompt::label(&id) {
        bridge::of(&app)
            .prompt()
            .answer(&id, allow, always.unwrap_or(false));
    }
}

#[tauri::command]
pub fn consent_resize(app: AppHandle, window: WebviewWindow, id: String, height: f64) {
    if window.label() == crate::prompt::label(&id) {
        bridge::of(&app).prompt().resize(&id, height);
    }
}

/// The box's page says how tall it is. Only from the box.
#[tauri::command]
pub fn flyout_resize(app: AppHandle, window: WebviewWindow, height: f64) {
    if window.label() == "flyout" {
        crate::windows::fit_flyout(&app, height);
    }
}

/// Async: on Windows a window built inside a synchronous command waits forever for its
/// WebView2, and the whole app stops (a blank panel that no longer quits).
#[tauri::command]
pub async fn open_panel(app: AppHandle, tab: Option<String>) {
    crate::windows::hide_flyout(&app);
    crate::windows::panel(&app, tab.as_deref());
}

/// Brings an agent (`session`) or a program (`resource`) to the front. The page names it
/// by its id: only what the hub sees connected now, never a pid chosen by the page.
#[tauri::command]
pub async fn bring_to_front(app: AppHandle, kind: String, id: String) -> Result<(), String> {
    let pid = crate::front::pid_of(&bridge::of(&app).state(), &kind, &id)
        .ok_or("it is no longer connected, or it has no window")?;
    tauri::async_runtime::spawn_blocking(move || crate::front::bring(pid))
        .await
        .map_err(|e| e.to_string())?
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

/// The hub, for what only the user may do from the panel: never from the control channel,
/// where any process could ask.
fn panel_broker(app: &AppHandle, window: &WebviewWindow) -> Result<clak_hub::Broker, String> {
    if window.label() != "panel" {
        return Err("Only from the panel.".into());
    }
    bridge::of(app)
        .broker()
        .ok_or_else(|| "The hub runs in another process: change it where it runs.".into())
}

/// The name an allowed application is shown by: the user's, or its own.
fn shown_name(broker: &clak_hub::Broker, key: &str) -> Result<String, String> {
    broker
        .run(|c| {
            c.permissions()
                .get(key)
                .map(|e| e.label.clone().unwrap_or_else(|| e.name.clone()))
        })
        .ok_or_else(|| "This application is no longer among the allowed ones.".into())
}

/// Gives one opening of a program, from Status, a suffix of the user's own («revit-test»
/// instead of «revit-2»); empty, the automatic one. Its new name.
#[tauri::command]
pub fn program_suffix(
    app: AppHandle,
    window: WebviewWindow,
    resource_id: String,
    suffix: String,
) -> Result<String, String> {
    panel_broker(&app, &window)?.run(|c| c.set_suffix(&resource_id, Some(&suffix)))
}

/// Gives an allowed application a name and a command of its own (empty: its own ones) and,
/// with `exe`, the place it has moved to. The presets that name it follow.
#[tauri::command]
pub async fn app_edit(
    app: AppHandle,
    window: WebviewWindow,
    key: String,
    label: String,
    command: String,
    exe: Option<String>,
) -> Result<(), String> {
    let broker = panel_broker(&app, &window)?;
    broker.run(|c| c.edit_app(&key, Some(&label), Some(&command)))?;
    let current = broker.run(|c| c.permissions().get(&key).map(|e| e.exe.clone()));
    let to = match exe.filter(|e| current.as_ref() != Some(e)) {
        Some(exe) => broker.move_app(&key, exe).await?,
        None => key.clone(),
    };
    let shown = shown_name(&broker, &to)?;
    let home = bridge::of(&app).home().clone();
    tauri::async_runtime::spawn_blocking(move || {
        clak_setup::presets::follow(&home, &key, &to, &shown)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Where an allowed application is now, from the system picker, starting next to where it
/// was: the executable (on the Mac the user picks the app, and this is the one inside it),
/// or nothing if the user closed the picker.
#[tauri::command]
pub async fn app_choose(window: WebviewWindow, exe: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    #[cfg(target_os = "macos")]
    let was = crate::app_icon::bundle_of(std::path::Path::new(&exe))
        .unwrap_or_else(|| std::path::PathBuf::from(&exe));
    #[cfg(not(target_os = "macos"))]
    let was = std::path::PathBuf::from(&exe);
    let mut picker = window
        .dialog()
        .file()
        .set_title("Where the application is now");
    #[cfg(target_os = "macos")]
    {
        picker = picker.add_filter("Applications", &["app"]);
    }
    #[cfg(windows)]
    {
        picker = picker.add_filter("Programs (*.exe)", &["exe"]);
    }
    if let Some(folder) = was.parent().filter(|f| f.is_dir()) {
        picker = picker.set_directory(folder);
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    picker.set_parent(&window).pick_file(move |path| {
        let _ = sender.send(path);
    });
    let Some(path) = receiver
        .await
        .ok()
        .flatten()
        .and_then(|p| p.into_path().ok())
    else {
        return Ok(None);
    };
    #[cfg(target_os = "macos")]
    let path = if path.extension().is_some_and(|e| e == "app") {
        crate::app_icon::executable_in(&path).ok_or("That app has no program to start.")?
    } else {
        path
    };
    Ok(Some(path.to_string_lossy().into_owned()))
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
        "connection_sound": bridge.prefs.connection_sound(),
        "update_check": bridge.prefs.update_check(),
        "auto_update": bridge.prefs.auto_update(),
        "server_icons": bridge.prefs.server_icons(),
        "autostart": crate::setup::autostart_on(),
    })
}

#[tauri::command]
pub fn prefs_set(app: AppHandle, name: String, value: bool) -> Result<(), String> {
    let bridge = bridge::of(&app);
    match name.as_str() {
        "notifications" => bridge.prefs.set_notifications(value).map_err(|e| e.0),
        "consent_sound" => bridge.prefs.set_consent_sound(value).map_err(|e| e.0),
        "connection_sound" => bridge.prefs.set_connection_sound(value).map_err(|e| e.0),
        "update_check" => bridge.prefs.set_update_check(value).map_err(|e| e.0),
        "auto_update" => bridge.prefs.set_auto_update(value).map_err(|e| e.0),
        "server_icons" => {
            bridge.prefs.set_server_icons(value).map_err(|e| e.0)?;
            // Every page asks again for the icons it shows, and the tray menu is redrawn.
            let _ = app.emit("server-icons", value);
            crate::tray::refresh(&app);
            Ok(())
        }
        "autostart" => {
            crate::setup::set_autostart(value)?;
            bridge.prefs.set_autostart(Some(value)).map_err(|e| e.0)
        }
        _ => Err(format!("unknown preference: {name}")),
    }
}
