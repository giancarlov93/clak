//! Start with the system, for the current user only and without privileges.
//!
//! On Windows an entry in `HKCU\…\Run`. On the Mac a LaunchAgent in
//! `~/Library/LaunchAgents`: it does with a file what `SMAppService.mainApp` would do,
//! which needs the bundle's Objective-C APIs; the user sees it (and can turn it off) in
//! System Settings › General › Login Items, among the background apps.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartState {
    /// Starts with the system, and it is this copy.
    On,
    /// Doesn't start.
    Off,
    /// The entry is there, but the user turned it off from Task Manager › Startup apps (on
    /// the Mac from Login Items).
    DisabledBySystem,
    /// Another copy of Clak starts with the system.
    Elsewhere,
}

#[cfg(windows)]
pub use windows::Autostart;

#[cfg(not(windows))]
pub use unix::Autostart;

/// The first element of a command line: in quotes, or up to the first space.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn first_of(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix('"') {
        return Some(match rest.find('"') {
            Some(end) if end > 0 => rest[..end].to_owned(),
            _ => line.trim_matches('"').to_owned(),
        });
    }
    Some(match line.find(' ') {
        Some(space) if space > 0 => line[..space].to_owned(),
        _ => line.to_owned(),
    })
}

/// The same executable written two ways: variables, slashes, case.
#[cfg(windows)]
pub(crate) fn same_exe(a: &str, b: &str) -> bool {
    let full = |p: &str| -> Option<String> {
        if p.trim().is_empty() {
            return None;
        }
        std::path::absolute(p).ok().map(|p| p.to_string_lossy().to_lowercase())
    };
    match (full(&crate::registry::expand(a)), full(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(windows)]
mod windows {
    use super::{AutostartState, first_of, same_exe};
    use crate::Result;
    use crate::registry::{self, STRING};
    use std::path::{Path, PathBuf};

    pub const NAME: &str = "Clak";

    /// Start with Windows: an entry in `HKCU\…\Run`, for the current user only, without
    /// privileges. Task Manager can turn it off without removing it (a value in
    /// `Explorer\StartupApproved\Run`): we read it, and turning it on from here clears it,
    /// because it is the user asking.
    #[derive(Debug, Clone)]
    pub struct Autostart {
        exe: PathBuf,
        root: String,
    }

    impl Autostart {
        pub const NAME: &'static str = NAME;

        /// The real entry, in `Software\Microsoft\Windows\CurrentVersion`.
        pub fn new(exe: impl Into<PathBuf>) -> Autostart {
            Autostart::with_root(exe, r"Software\Microsoft\Windows\CurrentVersion")
        }

        /// Under another HKCU key: for the tests.
        pub fn with_root(exe: impl Into<PathBuf>, root: impl Into<String>) -> Autostart {
            Autostart { exe: exe.into(), root: root.into() }
        }

        fn run_key(&self) -> String {
            format!(r"{}\Run", self.root)
        }

        fn approved_key(&self) -> String {
            format!(r"{}\Explorer\StartupApproved\Run", self.root)
        }

        pub fn exe(&self) -> &Path {
            &self.exe
        }

        /// The executable that starts with Windows today, or `None`.
        pub fn registered(&self) -> Option<String> {
            let run = registry::open(&self.run_key(), false)?;
            first_of(&run.string_expanded(Some(NAME))?)
        }

        pub fn state(&self) -> AutostartState {
            let Some(registered) = self.registered() else { return AutostartState::Off };
            if !same_exe(&registered, &self.exe.to_string_lossy()) {
                return AutostartState::Elsewhere;
            }
            // An odd first byte means "disabled" (2 and 6 on, 3 and 7 off).
            let disabled = registry::open(&self.approved_key(), false)
                .and_then(|k| k.binary(Some(NAME)))
                .is_some_and(|flags| flags.first().is_some_and(|b| b & 1 == 1));
            if disabled { AutostartState::DisabledBySystem } else { AutostartState::On }
        }

        pub fn enable(&self) -> Result<()> {
            let run = registry::create(&self.run_key())?;
            run.set_string(Some(NAME), &format!("\"{}\"", self.exe.display()), STRING)?;
            self.forget_approval()
        }

        pub fn disable(&self) -> Result<()> {
            if let Some(run) = registry::open(&self.run_key(), true) {
                run.delete_value(Some(NAME))?;
            }
            self.forget_approval()
        }

        fn forget_approval(&self) -> Result<()> {
            if let Some(approved) = registry::open(&self.approved_key(), true) {
                approved.delete_value(Some(NAME))?;
            }
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod unix {
    use super::AutostartState;
    use crate::places::suffixed;
    use crate::{Result, SetupError};
    use std::path::{Path, PathBuf};

    /// The LaunchAgent's label: the bundle identifier.
    pub const LABEL: &str = "dev.clak.Clak";

    /// Start at login with a file: a LaunchAgent on the Mac (`<label>.plist` with
    /// `RunAtLoad`), an XDG `.desktop` entry on other Unixes. For the current user only,
    /// without privileges; turning it off removes the file and doesn't stop the open app.
    ///
    /// On the Mac the user can disable it from Login Items without removing it:
    /// `launchctl print-disabled` says so, and turning it on from here clears it
    /// (`launchctl enable`), because it is the user asking. The tests use a fake folder and
    /// ask launchd nothing.
    #[derive(Debug, Clone)]
    pub struct Autostart {
        exe: PathBuf,
        folder: PathBuf,
        system: bool,
    }

    impl Autostart {
        /// The real LaunchAgent, in `~/Library/LaunchAgents` (`~/.config/autostart` off the Mac).
        pub fn new(exe: impl Into<PathBuf>) -> Autostart {
            let home = clak_wire::location::user_home();
            let folder = if cfg!(target_os = "macos") {
                home.join("Library").join("LaunchAgents")
            } else {
                dirs::config_dir().unwrap_or_else(|| home.join(".config")).join("autostart")
            };
            Autostart { exe: exe.into(), folder, system: true }
        }

        /// In another folder, without asking the system anything: for the tests.
        pub fn with_folder(exe: impl Into<PathBuf>, folder: impl Into<PathBuf>) -> Autostart {
            Autostart { exe: exe.into(), folder: folder.into(), system: false }
        }

        pub fn exe(&self) -> &Path {
            &self.exe
        }

        /// The file that starts the app.
        pub fn file(&self) -> PathBuf {
            if cfg!(target_os = "macos") {
                self.folder.join(format!("{LABEL}.plist"))
            } else {
                self.folder.join("clak.desktop")
            }
        }

        /// The executable that starts at login today, or `None`.
        pub fn registered(&self) -> Option<String> {
            let text = std::fs::read_to_string(self.file()).ok()?;
            if cfg!(target_os = "macos") {
                let after = &text[text.find("<key>ProgramArguments</key>")?..];
                let start = after.find("<string>")? + "<string>".len();
                let end = after[start..].find("</string>")?;
                Some(unescape(&after[start..start + end]))
            } else {
                let line = text.lines().find_map(|l| l.strip_prefix("Exec="))?;
                super::first_of(line)
            }
        }

        pub fn state(&self) -> AutostartState {
            let Some(registered) = self.registered() else { return AutostartState::Off };
            if !crate::clients::same_path_text(&registered, &self.exe.to_string_lossy()) {
                return AutostartState::Elsewhere;
            }
            if self.system && disabled_by_system() { AutostartState::DisabledBySystem } else { AutostartState::On }
        }

        pub fn enable(&self) -> Result<()> {
            let exe = self.exe.to_string_lossy();
            let text = if cfg!(target_os = "macos") {
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                     <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                     <plist version=\"1.0\">\n\
                     <dict>\n\
                     \t<key>Label</key><string>{LABEL}</string>\n\
                     \t<key>ProgramArguments</key><array><string>{}</string></array>\n\
                     \t<key>RunAtLoad</key><true/>\n\
                     \t<key>ProcessType</key><string>Interactive</string>\n\
                     \t<key>LimitLoadToSessionType</key><string>Aqua</string>\n\
                     </dict>\n\
                     </plist>\n",
                    escape(&exe)
                )
            } else {
                format!("[Desktop Entry]\nType=Application\nName=Clak\nExec=\"{exe}\"\nX-GNOME-Autostart-enabled=true\n")
            };
            let file = self.file();
            let write = || -> std::io::Result<()> {
                std::fs::create_dir_all(&self.folder)?;
                let draft = suffixed(&file, ".clak-tmp");
                std::fs::write(&draft, &text)?;
                std::fs::rename(&draft, &file)
            };
            write().map_err(|e| SetupError(format!("Can't write {}: {e}", file.display())))?;
            if self.system && cfg!(target_os = "macos") {
                let _ = launchctl(&["enable", &format!("{}/{LABEL}", domain())]);
            }
            Ok(())
        }

        pub fn disable(&self) -> Result<()> {
            match std::fs::remove_file(self.file()) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(SetupError(format!("Can't remove {}: {e}", self.file().display()))),
            }
        }
    }

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    fn unescape(text: &str) -> String {
        text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
    }

    /// `gui/<uid>`, the launchd domain of the user's session.
    fn domain() -> String {
        // SAFETY: getuid has no preconditions.
        format!("gui/{}", unsafe { libc::getuid() })
    }

    fn launchctl(args: &[&str]) -> Option<String> {
        let output = std::process::Command::new("/bin/launchctl").args(args).output().ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Turned off from Login Items: `"<label>" => disabled` (or `=> true`) in `launchctl print-disabled`.
    fn disabled_by_system() -> bool {
        if !cfg!(target_os = "macos") {
            return false;
        }
        let Some(text) = launchctl(&["print-disabled", &domain()]) else { return false };
        let needle = format!("\"{LABEL}\" =>");
        text.lines().any(|line| {
            let line = line.trim();
            line.starts_with(&needle) && (line.ends_with("disabled") || line.ends_with("true"))
        })
    }
}
