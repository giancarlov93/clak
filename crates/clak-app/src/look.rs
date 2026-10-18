//! Each system's look: Mica on Windows 11, glass (vibrancy) on the Mac, the accent color
//! chosen in the system's settings. The pages are transparent and let the material show through;
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
    let key = clak_setup::registry::open(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
        false,
    )?;
    accent_script(&key.binary(Some("AccentPalette"))?)
}

/// The palette is eight RGBA colors, from Light3 to Dark3.
#[cfg(any(windows, test))]
fn accent_script(palette: &[u8]) -> Option<String> {
    if palette.len() < 32 {
        return None;
    }
    let hex = |i: usize| {
        format!(
            "#{:02x}{:02x}{:02x}",
            palette[i * 4],
            palette[i * 4 + 1],
            palette[i * 4 + 2]
        )
    };
    Some(accent_properties(&hex(4), &hex(1)))
}

#[cfg(any(windows, target_os = "macos", test))]
fn accent_properties(light: &str, dark: &str) -> String {
    format!(
        "e.style.setProperty('--system-accent-light','{light}');e.style.setProperty('--system-accent-dark','{dark}')"
    )
}

/// The accent chosen in System Settings, as AppKit draws it in the light and in the dark
/// appearance. With "Multicolor" it is the system blue.
#[cfg(target_os = "macos")]
fn accent() -> Option<String> {
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSColor, NSColorSpace,
    };
    let shade = |name| {
        let appearance = NSAppearance::appearanceNamed(name)?;
        let found = std::cell::Cell::new(None);
        // A dynamic color: it takes its value only inside an appearance.
        let resolve = block2::StackBlock::new(|| {
            let color =
                NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace());
            found.set(color.map(|c| {
                let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                format!(
                    "#{:02x}{:02x}{:02x}",
                    byte(c.redComponent()),
                    byte(c.greenComponent()),
                    byte(c.blueComponent())
                )
            }));
        });
        appearance.performAsCurrentDrawingAppearance(&resolve);
        found.take()
    };
    let (light, dark) = unsafe {
        (
            shade(NSAppearanceNameAqua)?,
            shade(NSAppearanceNameDarkAqua)?,
        )
    };
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
        let effects = if mac == Effect::Popover {
            effects.radius(12.0)
        } else {
            effects
        };
        builder.effects(effects.build())
    } else {
        builder.effects(EffectsBuilder::new().effect(windows).build())
    }
}

/// The normal windows: the panel and the dialogs.
pub fn window<'a, R: Runtime, M: tauri::Manager<R>>(
    builder: WebviewWindowBuilder<'a, R, M>,
) -> WebviewWindowBuilder<'a, R, M> {
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    with(
        builder,
        Effect::Mica,
        Effect::Sidebar,
        EffectState::FollowsWindowActiveState,
    )
}

/// The questions: on top of the others, without taking focus, and on the Mac with the HUD panel glass.
pub fn transient<'a, R: Runtime, M: tauri::Manager<R>>(
    builder: WebviewWindowBuilder<'a, R, M>,
) -> WebviewWindowBuilder<'a, R, M> {
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .visible_on_all_workspaces(true);
    with(
        builder,
        Effect::Mica,
        Effect::HudWindow,
        EffectState::Active,
    )
}

/// The box that opens from the icon: borderless, like the system flyouts.
pub fn flyout<'a, R: Runtime, M: tauri::Manager<R>>(
    builder: WebviewWindowBuilder<'a, R, M>,
) -> WebviewWindowBuilder<'a, R, M> {
    let builder = builder
        .decorations(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .resizable(false)
        .shadow(true);
    with(
        builder,
        Effect::Acrylic,
        Effect::Popover,
        EffectState::Active,
    )
}

/// Read the current native preference, so changing it doesn't require restarting Clak.
pub fn reduced_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
        };
        let mut enabled: i32 = 1;
        // SAFETY: the output points to a writable BOOL for this SPI query.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut enabled as *mut i32).cast(),
                0,
            )
        };
        ok != 0 && enabled == 0
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        true
    }
}

/// Clak's own sounds, embedded in the app. They come as a pair, so neither uses a system sound.
#[derive(Clone, Copy)]
pub enum Sound {
    /// An agent connects. Optional, off by default.
    Connection,
    /// A question appears. On by default.
    Consent,
}

impl Sound {
    fn wav(self) -> &'static [u8] {
        match self {
            Sound::Connection => include_bytes!("../sounds/connect.wav"),
            Sound::Consent => include_bytes!("../sounds/consent.wav"),
        }
    }
}

/// Main thread: the Mac keeps its sounds in thread-local storage.
pub fn play(sound: Sound) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Media::Audio::{
            PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT, SND_NOSTOP, SND_SYSTEM,
        };
        // A process plays one sound at a time: a question interrupts a connection, never the reverse.
        let wait = match sound {
            Sound::Connection => SND_NOSTOP,
            Sound::Consent => 0,
        };
        // SAFETY: SND_MEMORY interprets this as WAV bytes; static storage outlives async playback.
        unsafe {
            PlaySoundW(
                sound.wav().as_ptr().cast(),
                std::ptr::null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT | SND_SYSTEM | wait,
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::AnyThread;
        use objc2::rc::Retained;
        use objc2_app_kit::NSSound;
        use objc2_foundation::NSData;
        fn load(sound: Sound) -> Option<Retained<NSSound>> {
            NSSound::initWithData(NSSound::alloc(), &NSData::with_bytes(sound.wav()))
        }
        // Keep each sound alive through playback and reuse it the next time.
        thread_local! {
            static CONNECTION: Option<Retained<NSSound>> = load(Sound::Connection);
            static CONSENT: Option<Retained<NSSound>> = load(Sound::Consent);
        }
        let player = match sound {
            Sound::Connection => &CONNECTION,
            Sound::Consent => &CONSENT,
        };
        player.with(|player| {
            if let Some(player) = player {
                // Like Windows: a new question starts its sound again, a new connection does not.
                if matches!(sound, Sound::Consent) {
                    player.stop();
                }
                let _ = player.play();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::Sound;

    /// Both systems play the bytes as they are: mono, 48 kHz, 16-bit PCM, with the samples at once.
    #[test]
    fn the_sounds_are_plain_pcm_wavs() {
        for sound in [Sound::Connection, Sound::Consent] {
            let wav = sound.wav();
            let u16_at = |i: usize| u16::from_le_bytes([wav[i], wav[i + 1]]);
            let u32_at = |i: usize| u32::from_le_bytes(wav[i..i + 4].try_into().unwrap());
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(u32_at(4) as usize, wav.len() - 8);
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!(u32_at(16), 16, "fmt chunk size");
            assert_eq!(u16_at(20), 1, "PCM");
            assert_eq!(u16_at(22), 1, "mono");
            assert_eq!(u32_at(24), 48_000);
            assert_eq!(u16_at(34), 16);
            assert_eq!(&wav[36..40], b"data");
            assert_eq!(u32_at(40) as usize, wav.len() - 44);
        }
    }

    #[test]
    fn the_accent_takes_winui_s_shades() {
        let palette: [u8; 32] = [
            0xF0, 0xC0, 0xF4, 0, 0xDB, 0x9E, 0xE5, 0, 0xB7, 0x63, 0xCB, 0, 0xA9, 0x4D, 0xC1, 0,
            0x8E, 0x3A, 0xA7, 0, 0x69, 0x27, 0x82, 0, 0x40, 0x0E, 0x59, 0, 0x88, 0x17, 0x98, 0,
        ];
        let script = super::accent_script(&palette).unwrap();
        assert!(
            script.contains("'--system-accent-light','#8e3aa7'"),
            "{script}"
        );
        assert!(script.contains("'--system-accent-dark','#db9ee5'"));
        assert_eq!(super::accent_script(&palette[..16]), None);
    }
}
