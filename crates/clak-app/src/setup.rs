//! What the app does on the computer: start with the system, and the install and
//! uninstall called by the installer. The rules live in `clak-setup`.

use clak_setup::autostart::{Autostart, AutostartState};
use std::path::PathBuf;

fn exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from(clak_setup::installation::TRAY_APP))
}

pub fn autostart_on() -> bool {
    matches!(Autostart::new(exe()).state(), AutostartState::On)
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    let autostart = Autostart::new(exe());
    if on { autostart.enable() } else { autostart.disable() }.map_err(|e| e.0)
}

/// `--setup` and `--uninstall`, from the installer: they do their work, log it, and exit.
pub fn installer(home: &std::path::Path, uninstall: bool) -> i32 {
    let folder = exe().parent().map(PathBuf::from).unwrap_or_default();
    let result = run(&folder, uninstall);
    match result {
        Ok(done) => {
            clak_setup::installation::append_log(home, uninstall, &done);
            0
        }
        Err(e) => {
            clak_setup::installation::append_log(home, uninstall, &[format!("error: {e}")]);
            1
        }
    }
}

#[cfg(windows)]
fn run(folder: &std::path::Path, uninstall: bool) -> Result<Vec<String>, String> {
    use clak_setup::preferences::Preferences;
    use clak_setup::uri_scheme::UriScheme;
    use clak_setup::user_path::UserPath;
    let places = clak_setup::Places::real();
    let path = UserPath::new(folder);
    let autostart = Autostart::new(exe());
    let scheme = UriScheme::new(exe());
    if uninstall {
        clak_setup::installation::cleanup(folder, &places, &path, &autostart, &scheme).map_err(|e| e.0)
    } else {
        let preferences = Preferences::new(clak_wire::location::home());
        clak_setup::installation::setup(folder, &places, &path, &autostart, &scheme, &preferences).map_err(|e| e.0)
    }
}

#[cfg(not(windows))]
fn run(folder: &std::path::Path, uninstall: bool) -> Result<Vec<String>, String> {
    // On the Mac no installer calls the app: the app sets itself up on first launch.
    let _ = (folder, uninstall);
    Ok(Vec::new())
}
