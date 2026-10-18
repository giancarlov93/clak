//! The icon in the Windows tray or in the Mac menu bar.
//!
//! The left click opens the box (flyout). On Windows the right click opens the native menu
//! and the double click the panel; on the Mac there is no menu, and both clicks open the box,
//! as for the bar icons that draw their own.

/// Whether the icon has the native menu: on the Mac the box takes its place.
const NATIVE_MENU: bool = !cfg!(target_os = "macos");

use crate::icon::{self, Kind};
use crate::notices::Agent;
use crate::{app_icon, bridge, notices, server_icon, windows};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Runtime};

const ID: &str = "clak";

/// The programs' icons in the menu, in pixels: the menu draws them 18 points tall.
const ICON_PIXELS: u32 = 36;

/// What the icon shows now: it is redrawn only when it changes.
static SHOWN: Mutex<Option<(Kind, String, Vec<Agent>)>> = Mutex::new(None);
/// Which of the states sent to the main thread is the last one.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static CLICK: AtomicBool = AtomicBool::new(false);

/// A burst produces one effect; reconnect storms must become quiet before another.
#[derive(Default)]
pub struct Connections {
    last: Option<Instant>,
}

impl Connections {
    pub fn accept(&mut self, now: Instant) -> bool {
        let previous = self.last.replace(now);
        previous.is_none_or(|last| now.duration_since(last) >= Duration::from_secs(2))
    }
}

/// Main thread only. Both state updates and the animation use the same rendering path.
fn draw(tray: &TrayIcon, kind: Kind) {
    let click =
        kind != Kind::Offline && CLICK.load(Ordering::SeqCst) && !crate::look::reduced_motion();
    let drawn = icon::current(kind, click);
    let _ = tray.set_icon(Some(Image::new_owned(drawn.rgba, drawn.size, drawn.size)));
    let _ = tray.set_icon_as_template(drawn.template);
    #[cfg(target_os = "macos")]
    if let Some((head, dot)) = icon::macos_layers(kind, 36, click) {
        let _ = tray.with_inner_tray_icon(move |inner| {
            if let Some(item) = inner.ns_status_item() {
                crate::menubar::show(&item, &head, &dot);
            }
        });
    }
}

fn redraw(app: &AppHandle) {
    let kind = SHOWN
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|s| s.0)
        .unwrap_or(Kind::Offline);
    if let Some(tray) = app.tray_by_id(ID) {
        draw(&tray, kind);
    }
}

/// A one-shot effect, independent of system notifications. No cursor blink in the tray.
pub fn connected(app: &AppHandle, sound: bool) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if SHOWN
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_none_or(|s| s.0 == Kind::Offline)
        {
            return;
        }
        if sound {
            crate::look::play(crate::look::Sound::Connection);
        }
        if crate::look::reduced_motion() {
            return;
        }
        CLICK.store(true, Ordering::SeqCst);
        redraw(&app2);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let app3 = app2.clone();
            let _ = app2.run_on_main_thread(move || {
                CLICK.store(false, Ordering::SeqCst);
                // Read the latest state, not the one at the start of the animation.
                redraw(&app3);
            });
        });
    });
}

/// The programs' icons, by executable or server address, read before going to the main thread.
type Icons = HashMap<String, Vec<u8>>;

/// The program's icon, or a transparent one of the same size: the programs stay indented
/// under their agent even without an icon of their own.
fn program_icon(icons: &Icons, key: Option<&String>) -> Image<'static> {
    key.and_then(|key| icons.get(key))
        .and_then(|png| Image::from_bytes(png).ok())
        .map(|image| image.to_owned())
        .unwrap_or_else(|| {
            Image::new_owned(
                vec![0; (ICON_PIXELS * ICON_PIXELS * 4) as usize],
                ICON_PIXELS,
                ICON_PIXELS,
            )
        })
}

/// Each agent as a heading, and under it the programs it may use, each with its "Revoke".
fn menu<R: Runtime>(
    app: &AppHandle<R>,
    headline: &str,
    agents: &[Agent],
    icons: &Icons,
) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(
        app,
        "headline",
        headline,
        false,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "open",
        "Open the panel",
        true,
        None::<&str>,
    )?)?;
    for (i, agent) in agents.iter().enumerate() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        menu.append(&MenuItem::with_id(
            app,
            format!("agent:{i}"),
            &agent.label,
            false,
            None::<&str>,
        )?)?;
        if agent.programs.is_empty() {
            menu.append(&MenuItem::with_id(
                app,
                format!("agent:{i}:none"),
                "No program yet",
                false,
                None::<&str>,
            )?)?;
        }
        for program in &agent.programs {
            let revoke = MenuItem::with_id(
                app,
                format!("revoke:{}", program.grant),
                "Revoke permission",
                true,
                None::<&str>,
            )?;
            let submenu = Submenu::with_id_and_items(
                app,
                format!("program:{}", program.grant),
                &program.label,
                true,
                &[&revoke],
            )?;
            submenu.set_icon(Some(program_icon(
                icons,
                program.exe.as_ref().or(program.icon.as_ref()),
            )))?;
            menu.append(&submenu)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    Ok(menu)
}

pub fn create(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let drawn = icon::current(Kind::Offline, false);
    let template = drawn.template;
    let builder = TrayIconBuilder::with_id(ID)
        .icon(Image::new_owned(drawn.rgba, drawn.size, drawn.size))
        .icon_as_template(template)
        .tooltip("Clak — not running");
    let builder = if NATIVE_MENU {
        builder.menu(&menu(app, "Not running", &[], &Icons::new())?)
    } else {
        builder
    };
    builder
        .show_menu_on_left_click(false)
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
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            rect,
            ..
        } => {
            windows::toggle_flyout(app, rect);
        }
        TrayIconEvent::Click {
            button: MouseButton::Right,
            button_state: MouseButtonState::Up,
            rect,
            ..
        } if !NATIVE_MENU => {
            windows::toggle_flyout(app, rect);
        }
        TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } if !cfg!(target_os = "macos") => {
            windows::hide_flyout(app);
            windows::panel(app, None);
        }
        _ => {}
    }
}

/// Draws the menu again with the same state: its icons have changed.
pub fn refresh(app: &AppHandle) {
    *SHOWN.lock().unwrap_or_else(|p| p.into_inner()) = None;
    let bridge = bridge::of(app);
    update(app, &bridge.state(), bridge.mode() == bridge::Mode::Offline);
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
    let headline = if offline {
        "Not running".to_owned()
    } else {
        notices::headline(state)
    };
    let agents = if offline {
        Vec::new()
    } else {
        notices::agents(state)
    };
    let shown = (kind, headline.clone(), agents.clone());
    let generation = {
        let mut last = SHOWN.lock().unwrap_or_else(|p| p.into_inner());
        if last.as_ref() == Some(&shown) {
            return;
        }
        *last = Some(shown);
        GENERATION.fetch_add(1, Ordering::SeqCst) + 1
    };
    let app = app.clone();
    // The menu's icons are read off the main thread.
    tauri::async_runtime::spawn_blocking(move || {
        let icons: Icons = if !NATIVE_MENU {
            Icons::new()
        } else {
            agents
                .iter()
                .flat_map(|a| &a.programs)
                .filter_map(|p| match (&p.exe, &p.icon) {
                    (Some(exe), _) => Some((exe.clone(), app_icon::png(exe, ICON_PIXELS)?)),
                    (None, Some(source)) => {
                        Some((source.clone(), server_icon::png(&app, source, ICON_PIXELS)?))
                    }
                    (None, None) => None,
                })
                .collect()
        };
        let app2 = app.clone();
        // Tray and menu are touched from the main thread.
        let _ = app.run_on_main_thread(move || {
            // A newer state got here first: this one is already old.
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            let Some(tray) = app2.tray_by_id(ID) else {
                return;
            };
            draw(&tray, kind);
            let tip = if offline {
                "Clak — not running".to_owned()
            } else {
                format!("Clak — {headline}")
            };
            let _ = tray.set_tooltip(Some(tip));
            if NATIVE_MENU && let Ok(menu) = menu(&app2, &headline, &agents, &icons) {
                let _ = tray.set_menu(Some(menu));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_coalesce_until_the_burst_has_been_quiet() {
        let mut connections = Connections::default();
        let now = Instant::now();
        assert!(connections.accept(now));
        assert!(!connections.accept(now + Duration::from_millis(100)));
        assert!(!connections.accept(now + Duration::from_millis(2000)));
        assert!(!connections.accept(now + Duration::from_millis(3900)));
        assert!(connections.accept(now + Duration::from_millis(5900)));
    }
}
