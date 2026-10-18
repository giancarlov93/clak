//! The `clak://` scheme, so that a page can offer a connector with one click. The link
//! connects nothing: it opens the connector window already filled in, and the user
//! confirms.
//!
//! On Windows an entry in `HKCU\Software\Classes\clak`, for the current user only, without
//! privileges, like start with Windows. On the Mac the bundle declares the scheme
//! (`CFBundleURLTypes` in the `Info.plist`) and LaunchServices registers it by itself when
//! it sees the app: here there is nothing to write or remove, and [`UriScheme`] always
//! answers "On".

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriSchemeState {
    /// clak:// links open this copy.
    On,
    /// Nobody opens them.
    Off,
    /// Another copy of Clak opens them.
    Elsewhere,
    /// The scheme belongs to another program: hands off.
    Foreign,
}

pub const SCHEME: &str = "clak";

#[cfg(windows)]
pub use windows::UriScheme;

#[cfg(not(windows))]
pub use unix::UriScheme;

#[cfg(windows)]
mod windows {
    use super::{SCHEME, UriSchemeState};
    use crate::Result;
    use crate::autostart::{first_of, same_exe};
    use crate::installation::TRAY_APP;
    use crate::registry::{self, STRING};
    use std::path::{Path, PathBuf};

    const DESCRIPTION: &str = "URL:Clak";

    /// The `clak://` scheme in `HKCU\Software\Classes\clak`.
    #[derive(Debug, Clone)]
    pub struct UriScheme {
        exe: PathBuf,
        root: String,
    }

    impl UriScheme {
        /// The real entry, in `Software\Classes`.
        pub fn new(exe: impl Into<PathBuf>) -> UriScheme {
            UriScheme::with_root(exe, r"Software\Classes")
        }

        /// Under another HKCU key: for the tests.
        pub fn with_root(exe: impl Into<PathBuf>, root: impl Into<String>) -> UriScheme {
            UriScheme {
                exe: exe.into(),
                root: root.into(),
            }
        }

        fn key(&self) -> String {
            format!(r"{}\{SCHEME}", self.root)
        }

        pub fn exe(&self) -> &Path {
            &self.exe
        }

        /// The executable that opens the links today, or `None`.
        pub fn registered(&self) -> Option<String> {
            let command = registry::open(&format!(r"{}\shell\open\command", self.key()), false)?;
            first_of(&command.string_expanded(None)?)
        }

        pub fn state(&self) -> UriSchemeState {
            let Some(key) = registry::open(&self.key(), false) else {
                return UriSchemeState::Off;
            };
            let registered = self.registered();
            if registered
                .as_deref()
                .is_some_and(|r| same_exe(r, &self.exe.to_string_lossy()))
            {
                return UriSchemeState::On;
            }
            // Ours if the description says so or if another copy of the app opens it.
            let file = registered
                .as_deref()
                .and_then(|r| Path::new(r).file_name())
                .map(|f| f.to_string_lossy().into_owned());
            let ours = key.string_expanded(None).as_deref() == Some(DESCRIPTION)
                || file
                    .as_deref()
                    .is_some_and(|f| f.eq_ignore_ascii_case(TRAY_APP));
            if ours {
                UriSchemeState::Elsewhere
            } else {
                UriSchemeState::Foreign
            }
        }

        pub fn register(&self) -> Result<()> {
            let exe = self.exe.display();
            let key = registry::create(&self.key())?;
            key.set_string(None, DESCRIPTION, STRING)?;
            key.set_string(Some("URL Protocol"), "", STRING)?;
            registry::create(&format!(r"{}\DefaultIcon", self.key()))?.set_string(
                None,
                &format!("\"{exe}\",0"),
                STRING,
            )?;
            registry::create(&format!(r"{}\shell\open\command", self.key()))?.set_string(
                None,
                &format!("\"{exe}\" \"%1\""),
                STRING,
            )?;
            Ok(())
        }

        /// Removes the scheme, unless it belongs to another program.
        pub fn unregister(&self) -> Result<()> {
            if self.state() != UriSchemeState::Foreign {
                registry::delete_tree(&self.key())?;
            }
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod unix {
    use super::UriSchemeState;
    use crate::Result;
    use std::path::{Path, PathBuf};

    /// On the Mac the scheme is in the bundle: nothing to register, nothing to remove.
    #[derive(Debug, Clone)]
    pub struct UriScheme {
        exe: PathBuf,
    }

    impl UriScheme {
        pub fn new(exe: impl Into<PathBuf>) -> UriScheme {
            UriScheme { exe: exe.into() }
        }

        pub fn exe(&self) -> &Path {
            &self.exe
        }

        /// Only LaunchServices knows who opens the links: we don't ask here.
        pub fn registered(&self) -> Option<String> {
            None
        }

        /// The app's `Info.plist` declares it.
        pub fn state(&self) -> UriSchemeState {
            UriSchemeState::On
        }

        pub fn register(&self) -> Result<()> {
            Ok(())
        }

        pub fn unregister(&self) -> Result<()> {
            Ok(())
        }
    }
}
