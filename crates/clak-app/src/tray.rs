//! The icon in the Windows tray or in the Mac menu bar, with its native menu.
//!
//! On Windows the left click opens the box (flyout), the double click the panel, the
//! right click the menu. On the Mac the click opens the menu, as for all the bar icons.

use crate::icon::{self, Kind};
use crate::{bridge, notices, windows};
use serde_json::Value;
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Runtime};

const ID: &str = "clak";

/// What the icon shows now: it is redrawn only when it changes.
static SHOWN: Mutex<Option<(Kind, String, Vec<(String, String)>)>> = Mutex::new(None);

fn image(kind: Kind) -> Image<'static> {
    let drawn = icon::current(kind);
    Image::new_owned(drawn.rgba, drawn.size, drawn.size)
}

fn menu<R: Runtime>(app: &AppHandle<R>, headline: &str, revocations: &[(String, String)]) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "headline", headline, false, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "open", "Open the panel", true, None::<&str>)?)?;
    if !revocations.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        for (grant, text) in revocations {
            menu.append(&MenuItem::with_id(app, format!("revoke:{grant}"), text, true, None::<&str>)?)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    Ok(menu)
}

pub fn create(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let drawn = icon::current(Kind::Offline);
    let template = drawn.template;
    TrayIconBuilder::with_id(ID)
        .icon(Image::new_owned(drawn.rgba, drawn.size, drawn.size))
        .icon_as_template(template)
        .tooltip("Clak — not running")
        .menu(&menu(app, "Not running", &[])?)
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(on_menu)
        .on_tray_icon_event(on_icon)
        .build(app)
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref();
    match id {
        "open" => windows::panel(app, None),
        "quit" => crate::quit(app.clone(), true),
        _ => {
            if let Some(grant) = id.strip_prefix("revoke:") {
                let mut message = serde_json::Map::new();
                message.insert("type".into(), "revoke".into());
                message.insert("grant_id".into(), grant.into());
                bridge::of(app).send(message);
            }
        }
    }
}

fn on_icon(tray: &TrayIcon, event: TrayIconEvent) {
    let app = tray.app_handle();
    match event {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } if !cfg!(target_os = "macos") => {
            windows::toggle_flyout(app, rect);
        }
        TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } if !cfg!(target_os = "macos") => {
            windows::hide_flyout(app);
            windows::panel(app, None);
        }
        _ => {}
    }
}

/// The icon, the tooltip and the menu after a new hub state.
pub fn update(app: &AppHandle, state: &Value, offline: bool) {
    let kind = if offline {
        Kind::Offline
    } else if notices::pending(state) > 0 {
        Kind::Pending
    } else if notices::grants(state) > 0 {
        Kind::Linked
    } else {
        Kind::Idle
    };
    let headline = if offline { "Not running".to_owned() } else { notices::headline(state) };
    let revocations = if offline { Vec::new() } else { notices::revocations(state) };
    let shown = (kind, headline.clone(), revocations.clone());
    {
        let mut last = SHOWN.lock().unwrap_or_else(|p| p.into_inner());
        if last.as_ref() == Some(&shown) {
            return;
        }
        *last = Some(shown);
    }
    let app2 = app.clone();
    // Tray and menu are touched from the main thread.
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = app2.tray_by_id(ID) else { return };
        let drawn = icon::current(kind);
        let template = drawn.template;
        let _ = tray.set_icon(Some(image(kind)));
        let _ = tray.set_icon_as_template(template);
        let tip = if offline { "Clak — not running".to_owned() } else { format!("Clak — {headline}") };
        let _ = tray.set_tooltip(Some(tip));
        if let Ok(menu) = menu(&app2, &headline, &revocations) {
            let _ = tray.set_menu(Some(menu));
        }
    });
}
