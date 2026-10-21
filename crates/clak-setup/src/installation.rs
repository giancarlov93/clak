//! What the installer asks of the app, because the app knows how to do it with the same
//! rules as the panel: after installation (`--setup`) start with the system, the command
//! line in the PATH, `clak://` links and the skill for Claude Code; before uninstallation
//! (`--uninstall`) remove what points to this copy, so no agent is left with a path that
//! no longer exists. What was done ends up in `install.log`, in the hub's folder.
//!
//! On the Mac there is no installer: at startup the app moves to itself what pointed to a
//! copy that is gone ([`repair_gone`]), as `--setup` does on Windows.

use crate::Result;
use crate::clients::{self, ClientState};
use crate::shim;
use crate::skill::{Skill, SkillState};
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub use windows::{cleanup, setup};

#[cfg(unix)]
pub use unix::repair_gone;

/// The command line and the app, as the installer places them.
pub const CLI: &str = if cfg!(windows) { "clak.exe" } else { "clak" };
pub const TRAY_APP: &str = if cfg!(windows) {
    "ClakApp.exe"
} else {
    "ClakApp"
};

/// The log of what `--setup` and `--uninstall` did.
pub fn log_file(home: &Path) -> PathBuf {
    home.join("install.log")
}

/// Appends to `install.log` the lines of an install or uninstall, with local date and
/// time. If it can't be written, never mind: the installation is done anyway.
pub fn append_log(home: &Path, uninstall: bool, done: &[String]) {
    use std::io::Write;
    let stamp = format!(
        "{} {}",
        local_time(),
        if uninstall { "uninstall" } else { "install" }
    );
    let _ = std::fs::create_dir_all(home);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file(home))
    {
        for line in done {
            let _ = writeln!(file, "{stamp}: {line}");
        }
    }
}

/// An update closes the app to replace its files, and `--setup` reopens it: the flag says
/// it was open. Without "paused", meanwhile, an agent using clak would restart it.
pub fn reopen_flag(home: &Path) -> PathBuf {
    home.join("reopen")
}

pub fn mark_reopen(home: &Path) {
    let _ = std::fs::create_dir_all(home);
    let _ = std::fs::write(reopen_flag(home), "");
}

/// Removes the flag, and says whether it was there: if so, the freshly installed app must
/// be reopened.
pub fn take_reopen(home: &Path) -> bool {
    let flag = reopen_flag(home);
    flag.exists() && std::fs::remove_file(flag).is_ok()
}

/// A shim of ours, written in full, that no longer exists.
pub fn is_gone_shim(command: &str) -> bool {
    #[cfg(windows)]
    let command = crate::registry::expand(command);
    let path = Path::new(&command);
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return false;
    };
    let ours = if cfg!(windows) {
        name.eq_ignore_ascii_case(shim::FILE_NAME)
    } else {
        name == shim::FILE_NAME
    };
    ours && path.is_absolute() && !path.exists()
}

/// MCP entries pointing to a shim of ours that is gone switch to `shim`: broken as they
/// were they would be of no use to anyone. Those pointing to another copy that still exists
/// are left to update in Settings › Integration: the user decides.
fn repair_clients(places: &crate::Places, shim: &Path, done: &mut Vec<(String, String)>) {
    for client in clients::all(places, &[shim.to_string_lossy().into_owned()]) {
        let status = client.status();
        if status.state != ClientState::Stale
            || !status.command.as_deref().is_some_and(is_gone_shim)
        {
            continue;
        }
        match client.add() {
            Ok(()) => done.push((client.name().to_owned(), client.note().to_owned())),
            Err(e) => done.push((client.name().to_owned(), format!("error: {e}"))),
        }
    }
}

/// The skills of this copy, one per coding agent (those not on this computer included).
fn skills(places: &crate::Places, cli: &Path) -> Vec<Skill> {
    Skill::agents(places, cli.to_string_lossy().into_owned())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::autostart::{Autostart, AutostartState};
    use crate::preferences::Preferences;
    use crate::uri_scheme::{UriScheme, UriSchemeState};
    use crate::user_path::UserPath;
    use crate::{Places, SetupError};

    /// After installation in `folder`. Another copy of Clak in the PATH (a folder with the
    /// app and the command line, for example the one it was built in, or a "Clak" folder
    /// that is gone) makes way for this one, and a skill of ours pointing to another path is
    /// updated; a skill with the same name written by others is left alone. Start with
    /// Windows is turned on, unless the user turned it off from the panel: an update doesn't
    /// turn it back on.
    ///
    /// Client MCP entries pointing to a shim that is gone switch to this copy: broken as
    /// they were, they would be of no use to anyone.
    pub fn setup(
        folder: &Path,
        places: &Places,
        path: &UserPath,
        autostart: &Autostart,
        scheme: &UriScheme,
        preferences: &Preferences,
    ) -> Result<Vec<String>> {
        let mut done = Vec::new();
        if preferences.autostart() == Some(false) {
            done.push("start with Windows left off, as chosen in the panel".to_owned());
        } else if matches!(
            autostart.state(),
            AutostartState::Off | AutostartState::Elsewhere
        ) {
            autostart.enable()?;
            done.push(format!("start with Windows: {}", autostart.exe().display()));
        }
        for other in path.remove_where(|f| is_copy(f) || was_copy(f))? {
            done.push(format!("removed another copy from PATH: {other}"));
        }
        if !path.contains() {
            path.add()?;
            done.push(format!("added to PATH: {}", folder.display()));
        }

        match scheme.state() {
            UriSchemeState::Off | UriSchemeState::Elsewhere => {
                scheme.register()?;
                done.push(format!(
                    "clak:// links opened by {}",
                    scheme.exe().display()
                ));
            }
            UriSchemeState::Foreign => {
                done.push("clak:// links left to the program that already opens them".to_owned())
            }
            UriSchemeState::On => {}
        }

        for skill in skills(places, &folder.join(CLI)) {
            let state = skill.status().state;
            if matches!(state, SkillState::Absent | SkillState::Stale) {
                skill.install()?;
                let verb = if state == SkillState::Stale {
                    "updated"
                } else {
                    "installed"
                };
                done.push(format!("skill {verb}: {}", skill.path().display()));
            } else if state != SkillState::NotInstalled {
                done.push(format!(
                    "skill left as it was ({state:?}): {}",
                    skill.path().display()
                ));
            }
        }

        let mut clients = Vec::new();
        repair_clients(places, &folder.join(shim::FILE_NAME), &mut clients);
        for (name, note) in clients {
            done.push(match note.strip_prefix("error: ") {
                Some(error) => format!("{name}: {error}"),
                None => format!("shim updated in {name}"),
            });
        }
        Ok(done)
    }

    /// Before uninstalling `folder`: only what points to this copy is removed.
    pub fn cleanup(
        folder: &Path,
        places: &Places,
        path: &UserPath,
        autostart: &Autostart,
        scheme: &UriScheme,
    ) -> Result<Vec<String>> {
        let mut done = Vec::new();
        if matches!(
            autostart.state(),
            AutostartState::On | AutostartState::DisabledBySystem
        ) {
            autostart.disable()?;
            done.push("removed start with Windows".to_owned());
        }
        if scheme.state() == UriSchemeState::On {
            scheme.unregister()?;
            done.push("removed clak:// links".to_owned());
        }
        if path.contains() {
            path.remove()?;
            done.push(format!("removed from PATH: {}", folder.display()));
        }

        for skill in skills(places, &folder.join(CLI)) {
            if skill.status().state == SkillState::Ok {
                skill.remove()?;
                done.push(format!("skill removed: {}", skill.path().display()));
            }
        }

        for client in clients::all(
            places,
            &[folder.join(shim::FILE_NAME).to_string_lossy().into_owned()],
        ) {
            if client.status().state != ClientState::Ok {
                continue;
            }
            match client.remove() {
                Ok(()) => done.push(format!("shim removed from {}", client.name())),
                Err(SetupError(e)) => done.push(format!("{}: {e}", client.name())),
            }
        }
        Ok(done)
    }

    fn is_copy(folder: &Path) -> bool {
        folder.join(CLI).is_file() && folder.join(TRAY_APP).is_file()
    }

    /// The folder of an installation that is gone.
    fn was_copy(folder: &Path) -> bool {
        !folder.is_dir() && folder.file_name().is_some_and(|n| n == "Clak")
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::Places;
    use crate::user_path::{PathLink, PathLinkState};
    use std::collections::BTreeMap;

    /// What pointed to an installation that is gone (an app removed or moved, a deleted
    /// build) moves to this one: broken as it is it would be of no use to anyone, and the
    /// agent would only notice by failing. The rest (an entry or a skill pointing to another
    /// copy that still exists) is left to update in Settings › Integration: the user decides.
    ///
    /// Returns, for each row of the tab (`path`, `skill:<agent>`, `client:<name>`), what to
    /// say under it: done, or the error.
    pub fn repair_gone(places: &Places, cli: &Path, shim: &Path) -> BTreeMap<String, String> {
        const NOTE: &str = "Updated: it pointed to an app that is gone";
        let mut done = BTreeMap::new();
        let mut record = |id: String, result: Result<String>| {
            done.insert(
                id,
                match result {
                    Ok(note) if note.is_empty() => NOTE.to_owned(),
                    Ok(note) => format!("{NOTE} · {note}"),
                    Err(e) => e.0,
                },
            );
        };

        let link = PathLink::user_bin(places, cli.to_string_lossy().into_owned());
        let status = link.status();
        if let (PathLinkState::Stale, Some(target)) = (status.state, status.target.as_deref()) {
            // A relative link is relative to its folder.
            let absolute = if target.starts_with('/') {
                target.to_owned()
            } else {
                format!("{}/{target}", link.folder())
            };
            if !Path::new(&absolute).exists() {
                record("path".into(), link.add().map(|()| String::new()));
            }
        }

        let gone = |old: &String| old.starts_with('/') && !Path::new(old).exists();
        for skill in skills(places, cli) {
            if skill.status().state == SkillState::Stale
                && skill.written_cli().is_some_and(|old| gone(&old))
            {
                record(
                    format!("skill:{}", skill.name()),
                    skill.install().map(|()| String::new()),
                );
            }
        }

        let mut clients = Vec::new();
        repair_clients(places, shim, &mut clients);
        for (name, note) in clients {
            let result = match note.strip_prefix("error: ") {
                Some(error) => Err(crate::SetupError(error.to_owned())),
                None => Ok(note),
            };
            record(format!("client:{name}"), result);
        }
        done
    }
}

/// Local date and time, `yyyy-mm-dd hh:mm:ss`.
#[cfg(windows)]
fn local_time() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime only fills the struct.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// Local date and time, `yyyy-mm-dd hh:mm:ss`.
#[cfg(unix)]
fn local_time() -> String {
    // SAFETY: time and localtime_r write only to the structs passed in.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut t: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut t);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.tm_year + 1900,
            t.tm_mon + 1,
            t.tm_mday,
            t.tm_hour,
            t.tm_min,
            t.tm_sec
        )
    }
}
