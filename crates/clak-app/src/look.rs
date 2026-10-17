//! Each system's look: Mica on Windows 11, glass (vibrancy) on the Mac, the system sound
//! when a question appears. The pages are transparent and let the material show through;
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

const SOLID: &str = "document.documentElement.classList.add('solid')";

fn with<'a, R: Runtime, M: tauri::Manager<R>>(
    builder: WebviewWindowBuilder<'a, R, M>,
    windows: Effect,
    mac: Effect,
    state: EffectState,
) -> WebviewWindowBuilder<'a, R, M> {
    if !material() {
        return builder.initialization_script(SOLID);
    }
    let builder = builder.transparent(true);
    if cfg!(target_os = "macos") {
        builder.effects(EffectsBuilder::new().effect(mac).state(state).build())
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
