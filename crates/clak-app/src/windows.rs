//! The panel and the icon's box. There is only one of each: opening it again brings it
//! to the front.

use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Position, Rect, Size, WebviewUrl, WebviewWindowBuilder};

const PANEL: &str = "panel";
const FLYOUT: &str = "flyout";
const FLYOUT_WIDTH: f64 = 360.0;
const FLYOUT_HEIGHT: f64 = 440.0;

/// The panel, on a tab if given.
pub fn panel(app: &AppHandle, tab: Option<&str>) {
    if let Some(window) = app.get_webview_window(PANEL) {
        if let Some(tab) = tab {
            let _ = window.emit("panel-tab", tab);
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let url = match tab {
        Some(tab) => format!("panel.html#{tab}"),
        None => "panel.html".to_owned(),
    };
    let builder = WebviewWindowBuilder::new(app, PANEL, WebviewUrl::App(url.into()))
        .title("Clak")
        .inner_size(920.0, 640.0)
        .min_inner_size(640.0, 420.0)
        .center();
    let _ = crate::look::window(builder).build();
}

pub fn hide_flyout(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(FLYOUT) {
        let _ = window.hide();
    }
}

/// The box next to the icon: it opens on click and vanishes when it loses focus.
pub fn toggle_flyout(app: &AppHandle, icon: Rect) {
    if let Some(window) = app.get_webview_window(FLYOUT) {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
            return;
        }
        place(&window, icon);
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("flyout-shown", ());
        return;
    }
    let builder = WebviewWindowBuilder::new(app, FLYOUT, WebviewUrl::App("flyout.html".into()))
        .title("Clak")
        .inner_size(FLYOUT_WIDTH, FLYOUT_HEIGHT)
        .visible(false);
    let Ok(window) = crate::look::flyout(builder).build() else { return };
    place(&window, icon);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Above the icon (Windows, bar at the bottom) or below it (bar at the top, Mac), inside the screen.
fn place(window: &tauri::WebviewWindow, icon: Rect) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let (ix, iy) = match icon.position {
        Position::Physical(p) => (p.x as f64, p.y as f64),
        Position::Logical(p) => (p.x * scale, p.y * scale),
    };
    let (iw, ih) = match icon.size {
        Size::Physical(s) => (s.width as f64, s.height as f64),
        Size::Logical(s) => (s.width * scale, s.height * scale),
    };
    let _ = window.set_size(LogicalSize::new(FLYOUT_WIDTH, FLYOUT_HEIGHT));
    let (w, h) = (FLYOUT_WIDTH * scale, FLYOUT_HEIGHT * scale);
    let gap = 8.0 * scale;
    let monitor = window.monitor_from_point(ix, iy).ok().flatten().or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else { return };
    let area = monitor.work_area();
    let (left, top) = (area.position.x as f64, area.position.y as f64);
    let (right, bottom) = (left + area.size.width as f64, top + area.size.height as f64);
    let x = (ix + iw / 2.0 - w / 2.0).clamp(left + gap, right - w - gap);
    let y = if iy > top + (bottom - top) / 2.0 { (iy - h - gap).max(top + gap) } else { (iy + ih + gap).min(bottom - h - gap) };
    let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
}
