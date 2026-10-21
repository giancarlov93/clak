//! The panel and the icon's box. There is only one of each: opening it again brings it
//! to the front.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Position, Rect, Size, WebviewUrl,
    WebviewWindowBuilder,
};

const PANEL: &str = "panel";
const FLYOUT: &str = "flyout";
const FLYOUT_WIDTH: f64 = 360.0;
/// The box takes the height of its content (the page says it), up to this: past it, it scrolls.
const FLYOUT_MIN_HEIGHT: f64 = 120.0;
const FLYOUT_MAX_HEIGHT: f64 = 440.0;
/// If the page never says its height, the box opens anyway after this.
const FLYOUT_FALLBACK: Duration = Duration::from_secs(2);
/// A click on the icon this soon after the box lost the focus is the click that took it away.
const FLYOUT_CLOSING_CLICK: Duration = Duration::from_millis(300);

/// What the box needs between a click on the icon and the page that measures itself.
struct Flyout {
    icon: Option<Rect>,
    height: f64,
    /// Built, waiting for its page: it opens when the page says its height.
    loading: bool,
    hidden_at: Option<Instant>,
}

static STATE: Mutex<Flyout> = Mutex::new(Flyout {
    icon: None,
    height: FLYOUT_MAX_HEIGHT,
    loading: false,
    hidden_at: None,
});

/// Never held while calling the window: on Windows, showing or focusing it delivers its
/// focus events right away, and their handler takes this lock.
fn state() -> std::sync::MutexGuard<'static, Flyout> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

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

/// The box lost the focus: it vanishes.
pub fn flyout_blurred(window: &tauri::Window) {
    let _ = window.hide();
    state().hidden_at = Some(Instant::now());
}

/// The box next to the icon: it opens on click and vanishes when it loses focus.
pub fn toggle_flyout(app: &AppHandle, icon: Rect) {
    let (height, closing, loading) = {
        let mut state = state();
        state.icon = Some(icon);
        let closing = state
            .hidden_at
            .is_some_and(|at| at.elapsed() < FLYOUT_CLOSING_CLICK);
        (state.height, closing, state.loading)
    };
    if let Some(window) = app.get_webview_window(FLYOUT) {
        // On Windows the press on the icon takes the focus from the box, which has already
        // vanished: this click closed it, it doesn't open it again.
        if loading || closing {
            return;
        }
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
            return;
        }
        place(&window, icon, height);
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("flyout-shown", ());
        return;
    }
    // Hidden until its page says how tall it is: on Windows, the WebView2 that starts up takes
    // the focus from a box already shown, and the box would vanish at once.
    let builder = WebviewWindowBuilder::new(app, FLYOUT, WebviewUrl::App("flyout.html".into()))
        .title("Clak")
        .inner_size(FLYOUT_WIDTH, height)
        .visible(false);
    state().loading = true;
    if crate::look::flyout(builder).build().is_err() {
        state().loading = false;
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FLYOUT_FALLBACK);
        reveal(&app);
    });
}

/// The page says how tall it is: the box takes that height, and opens if it was loading.
pub fn fit_flyout(app: &AppHandle, height: f64) {
    let height = height.clamp(FLYOUT_MIN_HEIGHT, FLYOUT_MAX_HEIGHT);
    let (icon, loading) = {
        let mut state = state();
        let changed = (state.height - height).abs() >= 1.0;
        state.height = height;
        if !changed && !state.loading {
            return;
        }
        (state.icon, state.loading)
    };
    if loading {
        reveal(app);
    } else if let (Some(window), Some(icon)) = (app.get_webview_window(FLYOUT), icon)
        && window.is_visible().unwrap_or(false)
    {
        place(&window, icon, height);
    }
}

/// Shows the box built hidden, once: when its page is ready, or after the fallback.
fn reveal(app: &AppHandle) {
    let (icon, height) = {
        let mut state = state();
        if !std::mem::take(&mut state.loading) {
            return;
        }
        (state.icon, state.height)
    };
    let (Some(window), Some(icon)) = (app.get_webview_window(FLYOUT), icon) else {
        return;
    };
    place(&window, icon, height);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Above the icon (Windows, bar at the bottom) or below it (bar at the top, Mac), inside the screen.
fn place(window: &tauri::WebviewWindow, icon: Rect, height: f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let (ix, iy) = match icon.position {
        Position::Physical(p) => (p.x as f64, p.y as f64),
        Position::Logical(p) => (p.x * scale, p.y * scale),
    };
    let (iw, ih) = match icon.size {
        Size::Physical(s) => (s.width as f64, s.height as f64),
        Size::Logical(s) => (s.width * scale, s.height * scale),
    };
    let _ = window.set_size(LogicalSize::new(FLYOUT_WIDTH, height));
    let (w, h) = (FLYOUT_WIDTH * scale, height * scale);
    let gap = 8.0 * scale;
    let monitor = window
        .monitor_from_point(ix, iy)
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else { return };
    let area = monitor.work_area();
    let (left, top) = (area.position.x as f64, area.position.y as f64);
    let (right, bottom) = (left + area.size.width as f64, top + area.size.height as f64);
    let x = (ix + iw / 2.0 - w / 2.0).clamp(left + gap, right - w - gap);
    let y = if iy > top + (bottom - top) / 2.0 {
        (iy - h - gap).max(top + gap)
    } else {
        (iy + ih + gap).min(bottom - h - gap)
    };
    let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
}
