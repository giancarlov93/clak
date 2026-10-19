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
    if on {
        autostart.enable()
    } else {
        autostart.disable()
    }
    .map_err(|e| e.0)
}

/// The package of another app this copy runs inside, if any: on Windows the children of a
/// packaged app (Claude Desktop is one) run in its package, where the writes to
/// `%LOCALAPPDATA%` go to the package's own folder. Nothing outside sees what Clak installs
/// or sets up from there.
#[cfg(windows)]
pub fn foreign_package() -> Option<String> {
    clak_agent::launcher::package()
}

#[cfg(not(windows))]
pub fn foreign_package() -> Option<String> {
    None
}

/// Started inside another app's package (an agent started the hub): this copy opens again
/// outside, through Explorer, and the caller exits. Once only within a short while: if the
/// copy started outside is still inside, it stays, without updating or setting itself up.
#[cfg(windows)]
pub fn leave_package(home: &std::path::Path) -> bool {
    if foreign_package().is_none() {
        return false;
    }
    let flag = home.join("leave-package");
    let recent = std::fs::metadata(&flag)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(30));
    if recent {
        return false;
    }
    let _ = std::fs::create_dir_all(home);
    let _ = std::fs::write(&flag, b"");
    clak_agent::launcher::start_outside(&format!("\"{}\"", exe().display()))
}

/// `--setup` and `--uninstall`, from the installer: they do their work, log it, and exit.
/// Inside another app's package the setup does nothing: the PATH, start with Windows and the
/// links would point to a folder that exists only there.
pub fn installer(home: &std::path::Path, uninstall: bool) -> i32 {
    if !uninstall && let Some(package) = foreign_package() {
        clak_setup::installation::append_log(
            home,
            false,
            &[format!(
                "error: the installer ran inside the app package {package}: open it from File Explorer"
            )],
        );
        return 1;
    }
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
        clak_setup::installation::cleanup(folder, &places, &path, &autostart, &scheme)
            .map_err(|e| e.0)
    } else {
        let preferences = Preferences::new(clak_wire::location::home());
        clak_setup::installation::setup(folder, &places, &path, &autostart, &scheme, &preferences)
            .map_err(|e| e.0)
    }
}

#[cfg(not(windows))]
fn run(folder: &std::path::Path, uninstall: bool) -> Result<Vec<String>, String> {
    // On the Mac no installer calls the app: the app sets itself up on first launch.
    let _ = (folder, uninstall);
    Ok(Vec::new())
}
