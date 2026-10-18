//! Starts the hub when it is not there.
//!
//! The hub belongs to all agents, not to the first one that arrives: it must be launched
//! detached from whoever launches it, otherwise it dies with it.
//!
//! - On Windows many agents kill their children all together (a job object):
//!   `std::process::Command` cannot break out of it, so here `CreateProcessW` is called
//!   directly, with `CREATE_BREAKAWAY_FROM_JOB`.
//! - On the Mac it is started through LaunchServices (`open`), as if the user had
//!   opened it from the Finder: its parent is launchd.

/// The variable that says what to launch.
pub const BROKER_VARIABLE: &str = "CLAK_BROKER";

/// What `CLAK_BROKER` says, if it says anything.
fn custom() -> Option<String> {
    std::env::var(BROKER_VARIABLE)
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// The folder of this executable.
#[cfg(windows)]
fn base_directory() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(std::path::Path::to_path_buf)
}

// =============================================================================
// Windows
// =============================================================================

#[cfg(windows)]
pub const TRAY_APP: &str = "ClakApp.exe";
#[cfg(windows)]
pub const DAEMON: &str = "clak-daemon.exe";

/// What to launch, as a command line: what `CLAK_BROKER` says, otherwise the tray app
/// or the windowless hub, if they sit next to this executable.
#[cfg(windows)]
pub fn command() -> Option<String> {
    if let Some(custom) = custom() {
        return Some(custom);
    }
    let base = base_directory()?;
    [TRAY_APP, DAEMON]
        .iter()
        .map(|name| base.join(name))
        .find(|path| path.is_file())
        .map(|path| format!("\"{}\"", path.display()))
}

/// Starts the hub, if it is known where it is.
#[cfg(windows)]
pub fn start() -> bool {
    command().is_some_and(|line| start_command(&line))
}

/// Launches a detached command line: in its own group, without a console, outside the job.
#[cfg(windows)]
pub fn start_command(command_line: &str) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW,
        PROCESS_INFORMATION, STARTUPINFOW,
    };
    // If the harness's job doesn't allow breaking away, too bad: we stay inside.
    for flags in [
        CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB,
        CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
    ] {
        // CreateProcessW may write into the command line: it goes in its own buffer.
        let mut buffer: Vec<u16> = command_line.encode_utf16().chain(Some(0)).collect();
        // SAFETY: the structs hold only integers and null pointers, with the size in cb;
        // the buffer is null-terminated and lives for the whole call.
        unsafe {
            let mut startup: STARTUPINFOW = std::mem::zeroed();
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut information: PROCESS_INFORMATION = std::mem::zeroed();
            let created = CreateProcessW(
                std::ptr::null(),
                buffer.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut information,
            );
            if created == 0 {
                continue;
            }
            CloseHandle(information.hThread);
            CloseHandle(information.hProcess);
        }
        return true;
    }
    false
}

// =============================================================================
// macOS (and the other Unixes, for development)
// =============================================================================

/// The app around this executable (`Clak.app/Contents/MacOS/clak`), or what
/// `CLAK_BROKER` says; in development, the hub next to it in the build folder.
#[cfg(unix)]
pub fn target() -> Option<String> {
    if let Some(custom) = custom() {
        return Some(custom);
    }
    let executable = std::env::current_exe().ok()?;
    let resolved = std::fs::canonicalize(&executable).unwrap_or(executable);
    target_for(&resolved)
}

#[cfg(unix)]
fn target_for(executable: &std::path::Path) -> Option<String> {
    let mac_os = executable.parent()?;
    let contents = mac_os.parent()?;
    let bundle = contents.parent()?;
    let named = |path: &std::path::Path, name: &str| path.file_name().is_some_and(|n| n == name);
    if named(mac_os, "MacOS")
        && named(contents, "Contents")
        && bundle.extension().is_some_and(|e| e == "app")
    {
        return Some(bundle.to_string_lossy().into_owned());
    }
    // Development: the hub next to it, in the build folder.
    Some(mac_os.join("clak-daemon"))
        .filter(|path| is_executable(path))
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Starts the hub, if it is known where it is.
#[cfg(unix)]
pub fn start() -> bool {
    use std::process::{Command, Stdio};
    let Some(target) = target() else { return false };
    if target.ends_with(".app") {
        // Without activating it and without windows: the user is working elsewhere. An app
        // opened by LaunchServices doesn't inherit the environment: the hub folder must be
        // passed, otherwise the app would open another one.
        let mut open = Command::new("/usr/bin/open");
        open.args(["-g", "-j"]);
        let variable = clak_wire::location::HOME_VARIABLE;
        if std::env::var(variable).is_ok_and(|v| !v.trim().is_empty()) {
            open.arg("--env").arg(format!(
                "{variable}={}",
                clak_wire::location::home().display()
            ));
        }
        open.arg("-a").arg(&target);
        open.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        return open.status().is_ok_and(|status| status.success());
    }
    // An executable (the windowless hub): detached, in its own group.
    use std::os::unix::process::CommandExt;
    let child = Command::new("/usr/bin/nohup")
        .arg(&target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match child {
        Ok(mut child) => {
            // A long-lived caller (the shim) must not leave a zombie when the hub exits.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(_) => false,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn l_app_si_trova_intorno_all_eseguibile() {
        assert_eq!(
            target_for(Path::new("/Applications/Clak.app/Contents/MacOS/clak")).as_deref(),
            Some("/Applications/Clak.app")
        );
        assert_eq!(target_for(Path::new("/nessun/posto/clak")), None);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn una_riga_che_non_parte_non_parte() {
        assert!(!start_command("\"C:\\nessun\\posto\\ClakApp.exe\""));
    }
}
