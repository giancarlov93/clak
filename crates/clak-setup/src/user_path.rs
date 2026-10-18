//! `clak` reachable by typing just the name.
//!
//! On Windows it is the folder of `clak.exe` in the user's PATH ([`UserPath`]); on the Mac
//! a symlink in `~/.local/bin` ([`PathLink`]), because there the PATH is decided by the
//! shell files, which belong to the user and are not rewritten.

#[cfg(windows)]
pub use windows::UserPath;

#[cfg(unix)]
pub use unix::{PathLink, PathLinkState, PathLinkStatus};

#[cfg(windows)]
mod windows {
    use crate::Result;
    use crate::registry::{self, EXPAND_STRING};
    use std::path::Path;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    const NAME: &str = "Path";

    /// The folder of `clak.exe` in the user's PATH, so an agent finds it by typing just
    /// `clak`. It is the user's PATH (HKCU\Environment), not the system one: no
    /// privileges, and it is removed the way it was added.
    ///
    /// Variables inside the value (`%USERPROFILE%\…`) stay as they were: it is read and
    /// written without expanding them. Programs already open see the change only when
    /// they restart.
    #[derive(Debug, Clone)]
    pub struct UserPath {
        folder: String,
        key: String,
        announce: bool,
    }

    impl UserPath {
        /// The user's real PATH.
        pub fn new(folder: impl AsRef<Path>) -> UserPath {
            UserPath::with_key(folder, "Environment", true)
        }

        /// On another key under HKCU, and without notifying other programs: for the tests.
        pub fn with_key(
            folder: impl AsRef<Path>,
            key: impl Into<String>,
            announce: bool,
        ) -> UserPath {
            UserPath {
                folder: folder.as_ref().to_string_lossy().into_owned(),
                key: key.into(),
                announce,
            }
        }

        pub fn folder(&self) -> &str {
            &self.folder
        }

        pub fn contains(&self) -> bool {
            let environment = registry::open(&self.key, false);
            entries(&read(environment.as_ref()))
                .iter()
                .any(|e| self.same(e))
        }

        pub fn add(&self) -> Result<()> {
            let environment = registry::create(&self.key)?;
            let current = read(Some(&environment));
            if entries(&current).iter().any(|e| self.same(e)) {
                return Ok(());
            }
            let value = if current.is_empty() {
                self.folder.clone()
            } else {
                format!("{};{}", current.trim_end_matches(';'), self.folder)
            };
            environment.set_string(Some(NAME), &value, kind_of(&environment))?;
            self.announce();
            Ok(())
        }

        pub fn remove(&self) -> Result<()> {
            let Some(environment) = registry::open(&self.key, true) else {
                return Ok(());
            };
            let current = read(Some(&environment));
            let all = entries(&current);
            let kept: Vec<&String> = all.iter().filter(|e| !self.same(e)).collect();
            if kept.len() == all.len() {
                return Ok(());
            }
            let value = kept
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(";");
            environment.set_string(Some(NAME), &value, kind_of(&environment))?;
            self.announce();
            Ok(())
        }

        /// Removes the other folders for which `drop` says yes (it receives the expanded path),
        /// leaving this one and everything else as it was. Returns the removed ones, as written.
        pub fn remove_where(&self, drop: impl Fn(&Path) -> bool) -> Result<Vec<String>> {
            let Some(environment) = registry::open(&self.key, true) else {
                return Ok(Vec::new());
            };
            let current = read(Some(&environment));
            let all = entries(&current);
            let dropped: Vec<String> = all
                .iter()
                .filter(|e| {
                    !self.same(e) && drop(Path::new(&registry::expand(e.trim_matches('"'))))
                })
                .cloned()
                .collect();
            if dropped.is_empty() {
                return Ok(Vec::new());
            }
            let value = all
                .iter()
                .filter(|e| !dropped.contains(e))
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(";");
            environment.set_string(Some(NAME), &value, kind_of(&environment))?;
            self.announce();
            Ok(dropped)
        }

        fn same(&self, entry: &str) -> bool {
            let expanded = registry::expand(entry.trim_matches('"'));
            match (full(&expanded), full(&self.folder)) {
                (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
                _ => false,
            }
        }

        /// File Explorer and the listening programs reread the environment.
        fn announce(&self) {
            if !self.announce {
                return;
            }
            let text: Vec<u16> = "Environment"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut result = 0usize;
            // SAFETY: nul-terminated text, valid for the whole call (synchronous, with timeout).
            unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    WM_SETTINGCHANGE,
                    0,
                    text.as_ptr() as isize,
                    SMTO_ABORTIFHUNG,
                    2000,
                    &mut result,
                )
            };
        }
    }

    fn read(environment: Option<&registry::Key>) -> String {
        environment
            .and_then(|k| k.string(Some(NAME)))
            .unwrap_or_default()
    }

    fn entries(value: &str) -> Vec<String> {
        value
            .split(';')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn kind_of(environment: &registry::Key) -> u32 {
        environment.kind(Some(NAME)).unwrap_or(EXPAND_STRING)
    }

    /// The full path, without the trailing slash.
    fn full(path: &str) -> Option<String> {
        if path.trim().is_empty() {
            return None;
        }
        let absolute = std::path::absolute(path).ok()?;
        let text = absolute.to_string_lossy().into_owned();
        let trimmed = text.trim_end_matches(['\\', '/']);
        Some(if trimmed.len() <= 2 {
            text
        } else {
            trimmed.to_owned()
        })
    }
}

#[cfg(unix)]
mod unix {
    use crate::clients::same_path_text;
    use crate::places::is_dir;
    use crate::{Result, SetupError};
    use std::path::{Component, Path};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PathLinkState {
        /// The user's commands folder is not there: we don't create it from here (see `PathLink`).
        NoFolder,
        /// The folder is there, `clak` is not.
        Absent,
        /// Our link is there, and points to this `clak`.
        Ok,
        /// A link of ours is there, but to another copy of the app (moved or old).
        Stale,
        /// Something else is in its place: a file, a folder, a link that isn't ours.
        Foreign,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PathLinkStatus {
        pub state: PathLinkState,
        /// Where the link is (or would be).
        pub path: String,
        /// Where it points today, if it is a link.
        pub target: Option<String>,
    }

    pub const NAME: &str = "clak";

    /// `clak` reachable by typing just the name: a symlink in `~/.local/bin`, the folder
    /// the Claude Code installer puts in the PATH.
    ///
    /// On Windows it is the app's folder in the user's PATH; here the PATH is decided by
    /// the shell files (`~/.zprofile`, `~/.zshrc`…), which belong to the user and are not
    /// rewritten: a link in a folder already in the PATH touches none of their files, and
    /// removing it puts everything back as it was. If the folder is not there we don't
    /// create it: on its own it would be useless (it wouldn't be in the PATH), and the panel
    /// explains what to do.
    ///
    /// Only a symlink pointing to a `clak` inside an app (`….app/Contents/MacOS/clak`) is
    /// touched: any other `clak`, even with the same name, stays where it is.
    #[derive(Debug, Clone)]
    pub struct PathLink {
        folder: String,
        cli: String,
    }

    impl PathLink {
        pub fn new(folder: impl Into<String>, cli: impl Into<String>) -> PathLink {
            PathLink {
                folder: folder.into(),
                cli: cli.into(),
            }
        }

        /// `~/.local/bin`.
        pub fn user_bin(places: &crate::Places, cli: impl Into<String>) -> PathLink {
            PathLink::new(
                places
                    .home
                    .join(".local")
                    .join("bin")
                    .to_string_lossy()
                    .into_owned(),
                cli,
            )
        }

        pub fn folder(&self) -> &str {
            &self.folder
        }

        pub fn path(&self) -> String {
            format!("{}/{NAME}", self.folder)
        }

        fn status_of(&self, state: PathLinkState, target: Option<String>) -> PathLinkStatus {
            PathLinkStatus {
                state,
                path: self.path(),
                target,
            }
        }

        pub fn status(&self) -> PathLinkStatus {
            if !is_dir(Path::new(&self.folder)) {
                return self.status_of(PathLinkState::NoFolder, None);
            }
            let path = self.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return self.status_of(PathLinkState::Absent, None);
                }
                Err(_) => return self.status_of(PathLinkState::Foreign, None),
            };
            if !metadata.file_type().is_symlink() {
                return self.status_of(PathLinkState::Foreign, None);
            }
            let Ok(target) = std::fs::read_link(&path) else {
                return self.status_of(PathLinkState::Foreign, None);
            };
            let target = target.to_string_lossy().into_owned();
            // A relative link is relative to its folder.
            let absolute = if target.starts_with('/') {
                target.clone()
            } else {
                format!("{}/{target}", self.folder)
            };
            if same_path_text(&absolute, &self.cli) {
                return self.status_of(PathLinkState::Ok, Some(target));
            }
            let state = if ours(&absolute) {
                PathLinkState::Stale
            } else {
                PathLinkState::Foreign
            };
            self.status_of(state, Some(target))
        }

        pub fn add(&self) -> Result<()> {
            match self.status().state {
                PathLinkState::Ok => return Ok(()),
                PathLinkState::NoFolder => {
                    return Err(SetupError(format!(
                        "The folder {} doesn't exist. Create it and add it to the PATH in your shell's file (for zsh, in ~/.zprofile: export PATH=\"$HOME/.local/bin:$PATH\"), then try again; or use the full path, as the skill already does.",
                        self.folder
                    )));
                }
                PathLinkState::Foreign => {
                    return Err(SetupError(format!(
                        "There's already a “{NAME}” in {} that isn't from Clak: leaving it alone.",
                        self.folder
                    )));
                }
                PathLinkState::Absent | PathLinkState::Stale => {
                    // Created alongside and renamed over it: whoever launches `clak` at that
                    // moment finds the old one or the new one, never neither.
                    let draft = format!("{}/.{NAME}.clak-tmp", self.folder);
                    let _ = std::fs::remove_file(&draft);
                    std::os::unix::fs::symlink(&self.cli, &draft)
                        .map_err(|e| self.failure("create", e))?;
                    if let Err(e) = std::fs::rename(&draft, self.path()) {
                        let _ = std::fs::remove_file(&draft);
                        return Err(self.failure("create", e));
                    }
                }
            }
            Ok(())
        }

        pub fn remove(&self) -> Result<()> {
            if !matches!(
                self.status().state,
                PathLinkState::Ok | PathLinkState::Stale
            ) {
                return Ok(());
            }
            match std::fs::remove_file(self.path()) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(self.failure("remove", e)),
            }
        }

        fn failure(&self, verb: &str, error: std::io::Error) -> SetupError {
            SetupError(format!("Can't {verb} the link {}: {error}.", self.path()))
        }
    }

    /// A `clak` inside an app bundle: ours, even if the app was moved, renamed or is
    /// gone.
    pub(crate) fn ours(target: &str) -> bool {
        let mut parts: Vec<String> = Vec::new();
        for component in Path::new(target).components() {
            match component {
                Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
                Component::ParentDir => {
                    parts.pop();
                }
                _ => {}
            }
        }
        if parts.len() < 4 {
            return false;
        }
        let tail = &parts[parts.len() - 4..];
        tail[0].ends_with(".app") && tail[1] == "Contents" && tail[2] == "MacOS" && tail[3] == NAME
    }
}
