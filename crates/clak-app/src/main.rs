//! The Clak app: the hub inside, the icon in the tray (Windows) or the menu bar (Mac),
//! system notifications, the consent windows and the panel.
//!
//! ```text
//!     ClakApp                  in the tray, with the hub (or watching the one already running)
//!     ClakApp --show           and the panel open
//!     ClakApp --no-broker      only watch, without starting a hub
//!     ClakApp --quit [--update]  closes the running copy (and reopens it after the update)
//!     ClakApp --setup | --uninstall   from the installer
//!     ClakApp clak://…         a link, to the running copy
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod app_icon;
mod bridge;
#[cfg(windows)]
mod closing;
mod commands;
mod editors;
mod icon;
mod look;
#[cfg(target_os = "macos")]
mod menubar;
mod notices;
mod prompt;
mod setup;
mod tray;
mod updates;
mod windows;

use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// How long a question stays open before it withdraws by itself.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn link(args: &[String]) -> Option<&String> {
    args.iter()
        .find(|a| a.to_ascii_lowercase().starts_with("clak://"))
}

/// What a second copy, launched while this one is running, asks for.
fn forwarded(app: &AppHandle, args: Vec<String>) {
    let home = bridge::of(app).home().clone();
    if has(&args, "--quit") {
        if has(&args, "--update") {
            clak_setup::installation::mark_reopen(&home);
        }
        quit(app.clone(), false);
    } else if let Some(uri) = link(&args) {
        open_link(app, uri);
    } else {
        // Launching it again means wanting to see it.
        windows::panel(app, None);
    }
}

/// A `clak://connect?…` link: the connector window, already filled in (editors.rs).
fn open_link(app: &AppHandle, uri: &str) {
    editors::open_link(app, uri);
}

/// Quits the app. From the menu it asks first, if someone is connected, and leaves the hub
/// paused: reopening the app resumes it.
pub fn quit(app: AppHandle, user: bool) {
    let bridge = bridge::of(&app);
    let busy = notices::busy(&bridge.state()) > 0;
    let finish = move |app: AppHandle, paused: bool| {
        tauri::async_runtime::spawn(async move {
            let bridge = bridge::of(&app);
            if paused && bridge.mode() == bridge::Mode::Local {
                let _ = std::fs::write(clak_wire::location::pause_flag(bridge.home()), b"");
            }
            bridge.stop().await;
            app.exit(0);
        });
    };
    if user && busy && bridge.mode() == bridge::Mode::Local {
        app.dialog()
            .message("If you quit, agents and programs are disconnected, and no agent can use a program until you reopen Clak.")
            .title("Quit Clak?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom("Quit".into(), "Cancel".into()))
            .show(move |yes| {
                if yes {
                    finish(app, true);
                }
            });
    } else {
        finish(app, user);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let home = clak_wire::location::home();

    if has(&args, "--uninstall") {
        std::process::exit(setup::installer(&home, true));
    }
    if has(&args, "--setup") {
        // The installer waits for it to finish: the work, then the app reopens by itself, detached.
        let code = setup::installer(&home, false);
        if code == 0
            && let Ok(exe) = std::env::current_exe()
        {
            let _ = std::process::Command::new(exe).spawn();
        }
        std::process::exit(code);
    }
    // From the installer: it replaces the files as soon as this returns, so wait until the
    // running copy has really gone.
    #[cfg(windows)]
    if has(&args, "--quit") && std::env::var_os(closing::FORWARD).is_none() {
        std::process::exit(closing::quit_and_wait(&args));
    }
    let own = !has(&args, "--no-broker");
    let show = has(&args, "--show");
    let first_link = link(&args).cloned();
    let quitting = has(&args, "--quit");

    let app = tauri::Builder::default()
        // First: a second copy passes its arguments to this one and exits.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            forwarded(app, args)
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::hub_hello,
            commands::hub_send,
            commands::consent_request,
            commands::consent_answer,
            commands::consent_resize,
            commands::open_panel,
            commands::app_launch,
            commands::quit,
            commands::prefs_get,
            commands::prefs_set,
            app_icon::app_icon,
            updates::update_status,
            updates::update_open,
            updates::update_install,
            // The Agents and Settings tabs (agents.rs).
            agents::agents_status,
            agents::agents_add,
            agents::agents_remove,
            agents::agents_snippet,
            agents::agents_choose_shim,
            agents::agents_repair,
            agents::settings_status,
            agents::settings_open,
            // The MCP and APIs tabs (editors.rs).
            editors::editor_fit,
            editors::editor_stop,
            editors::editor_close,
            editors::connector_open,
            editors::connector_draft,
            editors::connector_check,
            editors::connector_probe,
            editors::connector_save,
            editors::connector_enable,
            editors::connector_remove,
            editors::api_open,
            editors::api_draft,
            editors::api_check,
            editors::api_import,
            editors::api_browse,
            editors::api_save,
            editors::api_enable,
            editors::api_remove,
            editors::endpoint_open,
            editors::endpoint_draft,
            editors::endpoint_check,
            editors::endpoint_done,
        ])
        .setup(move |app| {
            // `--quit` without a running copy: there is nothing to close.
            if quitting {
                std::process::exit(0);
            }
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            #[cfg(target_os = "macos")]
            look::follow_accent(app.handle());

            let handle = app.handle().clone();
            let prefs = clak_setup::preferences::Preferences::new(&home);
            let sound = Arc::new(move || prefs.consent_sound());
            let prompt = prompt::WindowPrompt::new(handle.clone(), CONSENT_TIMEOUT, sound);
            let bridge = bridge::Bridge::new(handle.clone(), home.clone(), prompt);
            app.manage(bridge.clone());
            tray::create(&handle)?;
            // On the Mac there is no installer to set things up: whatever pointed at a copy that
            // is gone (a deleted build, a moved app) switches to this one at startup.
            if cfg!(target_os = "macos") {
                let home = home.clone();
                std::thread::spawn(move || agents::repair(&home));
            }
            tauri::async_runtime::spawn(bridge.run(own));
            updates::start(handle.clone());
            if show {
                windows::panel(&handle, None);
            }
            if let Some(uri) = &first_link {
                open_link(&handle, uri);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            let label = window.label();
            match event {
                WindowEvent::Focused(false) if label == "flyout" => {
                    let _ = window.hide();
                }
                WindowEvent::Destroyed => {
                    if let Some(app) = window.app_handle().try_state::<Arc<bridge::Bridge>>() {
                        app.prompt().closed_window(label);
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("the app couldn't start");

    app.run(|app, event| match event {
        // Closing the last window doesn't quit the app: it lives in the tray.
        RunEvent::ExitRequested {
            api, code: None, ..
        } => api.prevent_exit(),
        #[cfg(target_os = "macos")]
        RunEvent::Opened { urls } => {
            for url in urls {
                open_link(app, url.as_str());
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => windows::panel(app, None),
        _ => {
            let _ = app;
        }
    });
}
