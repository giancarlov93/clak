use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// An error to show the user as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupError(pub String);

impl SetupError {
    pub fn new(message: impl Into<String>) -> SetupError {
        SetupError(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SetupError {}

impl From<std::io::Error> for SetupError {
    fn from(error: std::io::Error) -> SetupError {
        SetupError(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, SetupError>;

/// Where the configurations live. In the tests it is a temporary folder.
///
/// `app_data` and `local_app_data` are `%APPDATA%` and `%LOCALAPPDATA%` on Windows; on the
/// Mac both are `~/Library/Application Support`.
#[derive(Debug, Clone, Default)]
pub struct Places {
    pub home: PathBuf,
    pub app_data: PathBuf,
    pub local_app_data: PathBuf,
    pub environment: BTreeMap<String, String>,
}

impl Places {
    pub fn new(
        home: impl Into<PathBuf>,
        app_data: impl Into<PathBuf>,
        local_app_data: impl Into<PathBuf>,
        environment: BTreeMap<String, String>,
    ) -> Places {
        Places { home: home.into(), app_data: app_data.into(), local_app_data: local_app_data.into(), environment }
    }

    /// The same folders with another environment.
    pub fn with_environment(&self, environment: BTreeMap<String, String>) -> Places {
        Places { environment, ..self.clone() }
    }

    /// An environment variable. On Windows names are case-insensitive (`Path` and `PATH`
    /// are the same).
    pub fn var(&self, name: &str) -> Option<&str> {
        if let Some(value) = self.environment.get(name) {
            return Some(value);
        }
        if cfg!(windows) {
            return self.environment.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str());
        }
        None
    }

    /// `~/Library/Application Support`.
    pub fn application_support(&self) -> PathBuf {
        self.home.join("Library").join("Application Support")
    }

    /// The real user.
    #[cfg(windows)]
    pub fn real() -> Places {
        let environment = real_environment();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let app_data = dirs::config_dir().unwrap_or_else(|| home.join("AppData").join("Roaming"));
        let local_app_data = dirs::data_local_dir().unwrap_or_else(|| home.join("AppData").join("Local"));
        Places { home, app_data, local_app_data, environment }
    }

    /// The real user. `HOME` wins over what the system says: so the panel can be tried on
    /// a fake folder.
    ///
    /// An app opened from the Finder has a minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`):
    /// we add the folders where commands installed by the user usually live, otherwise
    /// `claude` would never be found.
    #[cfg(not(windows))]
    pub fn real() -> Places {
        let mut environment = real_environment();
        let home = environment
            .get("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut path: Vec<String> =
            environment.get("PATH").map(|p| p.split(':').filter(|s| !s.is_empty()).map(str::to_owned).collect()).unwrap_or_default();
        let local_bin = home.join(".local").join("bin").to_string_lossy().into_owned();
        for extra in [local_bin.as_str(), "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            if !path.iter().any(|p| p == extra) {
                path.push(extra.to_owned());
            }
        }
        environment.insert("PATH".into(), path.join(":"));
        let support = home.join("Library").join("Application Support");
        Places { home, app_data: support.clone(), local_app_data: support, environment }
    }
}

fn real_environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .map(|(key, value)| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
        .collect()
}

/// A path with a suffix attached to the name: `config.json` → `config.json.clak-bak`.
pub(crate) fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut text = path.as_os_str().to_owned();
    text.push(suffix);
    PathBuf::from(text)
}

pub(crate) fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

/// The folder of a path; for a path without one, a folder that doesn't exist.
pub(crate) fn parent(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new(""))
}
