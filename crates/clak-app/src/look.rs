//! Each system's look: Mica on Windows 11, glass (vibrancy) on the Mac, the accent color
//! chosen in the system's settings, the system sound when a question appears. The pages are transparent and let the material show through;
//! where there is no material (Windows 10) the window is opaque and the page draws a solid
//! background.

use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{Runtime, WebviewWindowBuilder};

/// Mica exists from Windows 11 (build 22000); the Mac's glass is always there.
pub fn material() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
        let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
        info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
        // RtlGetVersion doesn't lie about the version, unlike GetVersionEx.
        let ok = unsafe { windows_sys::Wdk::System::SystemServices::RtlGetVersion(&mut info) } == 0;
        ok && info.dwBuildNumber >= 22000
    }
    #[cfg(not(windows))]
    {
        true
    }
}

const SOLID: &str = "e.classList.add('solid')";

/// The initialization scripts run before the page is parsed, when there is no `<html>` yet:
/// `body` runs on it, `e`, as soon as it exists.
fn on_root(body: &str) -> String {
    format!(
        "(()=>{{const f=e=>{{{body}}};const r=document.documentElement;if(r)f(r);else new MutationObserver((_,o)=>{{const r=document.documentElement;if(r){{o.disconnect();f(r)}}}}).observe(document,{{childList:true}})}})()"
    )
}

/// The accent chosen in Windows, in the shades WinUI uses for it: Dark1 on light, Light2 on
/// dark. The pages read them as `--system-accent-light` and `--system-accent-dark`.
#[cfg(windows)]
fn accent() -> Option<String> {
    let key = clak_setup::registry::open(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent", false)?;
    accent_script(&key.binary(Some("AccentPalette"))?)
}

/// The palette is eight RGBA colors, from Light3 to Dark3.
#[cfg(any(windows, test))]
fn accent_script(palette: &[u8]) -> Option<String> {
    if palette.len() < 32 {
        return None;
    }
    let hex = |i: usize| format!("#{:02x}{:02x}{:02x}", palette[i * 4], palette[i * 4 + 1], palette[i * 4 + 2]);
    Some(accent_properties(&hex(4), &hex(1)))
}

#[cfg(any(windows, target_os = "macos", test))]
fn accent_properties(light: &str, dark: &str) -> String {
    format!("e.style.setProperty('--system-accent-light','{light}');e.style.setProperty('--system-accent-dark','{dark}')")
}

/// The accent chosen in System Settings, as AppKit draws it in the light and in the dark
/// appearance. With "Multicolor" it is the system blue.
#[cfg(target_os = "macos")]
fn accent() -> Option<String> {
    use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSColor, NSColorSpace};
    let shade = |name| {
        let appearance = NSAppearance::appearanceNamed(name)?;
        let found = std::cell::Cell::new(None);
        // A dynamic color: it takes its value only inside an appearance.
        let resolve = block2::StackBlock::new(|| {
            let color = NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace());
            found.set(color.map(|c| {
                let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                format!("#{:02x}{:02x}{:02x}", byte(c.redComponent()), byte(c.greenComponent()), byte(c.blueComponent()))
            }));
        });
        appearance.performAsCurrentDrawingAppearance(&resolve);
        found.take()
    };
    let (light, dark) = unsafe { (shade(NSAppearanceNameAqua)?, shade(NSAppearanceNameDarkAqua)?) };
    Some(accent_properties(&light, &dark))
}

/// When the accent changes in System Settings, the open windows take it at once; the new
/// ones read it when they are built.
#[cfg(target_os = "macos")]
pub fn follow_accent<R: Runtime>(app: &tauri::AppHandle<R>) {
    use objc2_app_kit::NSSystemColorsDidChangeNotification;
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSOperationQueue};
    use tauri::Manager;
    let app = app.clone();
    let changed = block2::RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| {
        let Some(script) = accent() else { return };
        for window in app.webview_windows().values() {
            let _ = window.eval(on_root(&script));
        }
    });
    let observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSSystemColorsDidChangeNotification),
            None,
            Some(&NSOperationQueue::mainQueue()),
            &changed,
        )
    };
    // For the whole life of the app.
    std::mem::forget(observer);
}

fn with<'a, R: Runtime, M: tauri::Manager<R>>(
    builder: WebviewWindowBuilder<'a, R, M>,
    windows: Effect,
    mac: Effect,
    state: EffectState,
) -> WebviewWindowBuilder<'a, R, M> {
    #[cfg(any(windows, target_os = "macos"))]
    let builder = match accent() {
        Some(script) => builder.initialization_script(on_root(&script)),
        None => builder,
    };
    if !material() {
        return builder.initialization_script(on_root(SOLID));
    }
    let builder = builder.transparent(true);
    if cfg!(target_os = "macos") {
        // The box has no frame of its own: the glass gets the popovers' rounded corners.
        let effects = EffectsBuilder::new().effect(mac).state(state);
        let effects = if mac == Effect::Popover { effects.radius(12.0) } else { effects };
        builder.effects(effects.build())
    } else {
        builder.effects(EffectsBuilder::new().effect(windows).build())
    }
}

/// The normal windows: the panel and the dialogs.
pub fn window<'a, R: Runtime, M: tauri::Manager<R>>(builder: WebviewWindowBuilder<'a, R, M>) -> WebviewWindowBuilder<'a, R, M> {
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
    with(builder, Effect::Mica, Effect::Sidebar, EffectState::FollowsWindowActiveState)
}

/// The questions: on top of the others, without taking focus, and on the Mac with the HUD panel glass.
pub fn transient<'a, R: Runtime, M: tauri::Manager<R>>(builder: WebviewWindowBuilder<'a, R, M>) -> WebviewWindowBuilder<'a, R, M> {
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true).visible_on_all_workspaces(true);
    with(builder, Effect::Mica, Effect::HudWindow, EffectState::Active)
}

/// The box that opens from the icon: borderless, like the system flyouts.
pub fn flyout<'a, R: Runtime, M: tauri::Manager<R>>(builder: WebviewWindowBuilder<'a, R, M>) -> WebviewWindowBuilder<'a, R, M> {
    let builder = builder.decorations(false).skip_taskbar(true).always_on_top(true).resizable(false).shadow(true);
    with(builder, Effect::Acrylic, Effect::Popover, EffectState::Active)
}

/// The sound of a new question: the one the system uses for information.
pub fn announce() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::MessageBeep(windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("/usr/bin/afplay").arg("/System/Library/Sounds/Glass.aiff").spawn();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_accent_takes_winui_s_shades() {
        let palette: [u8; 32] = [
            0xF0, 0xC0, 0xF4, 0, 0xDB, 0x9E, 0xE5, 0, 0xB7, 0x63, 0xCB, 0, 0xA9, 0x4D, 0xC1, 0,
            0x8E, 0x3A, 0xA7, 0, 0x69, 0x27, 0x82, 0, 0x40, 0x0E, 0x59, 0, 0x88, 0x17, 0x98, 0,
        ];
        let script = super::accent_script(&palette).unwrap();
        assert!(script.contains("'--system-accent-light','#8e3aa7'"), "{script}");
        assert!(script.contains("'--system-accent-dark','#db9ee5'"));
        assert_eq!(super::accent_script(&palette[..16]), None);
    }
}
