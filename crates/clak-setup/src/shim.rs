//! Where the shim lives: the program every client launches, which bridges to the hub.
//!
//! When shipped with the app it sits next to its executable. Otherwise the user points to
//! it once and the path stays in the preferences file (`win.json`, `mac.json`), in the
//! hub's folder.

use crate::Result;
use crate::preferences;
use serde_json::Value;
use std::path::Path;

pub const FILE_NAME: &str = if cfg!(windows) {
    "clak-shim.exe"
} else {
    "clak-shim"
};

/// The shim command (executable and arguments), or `None` if we don't know where it is.
pub fn locate(app_directory: &Path, home: &Path) -> Option<Vec<String>> {
    let bundled = app_directory.join(FILE_NAME);
    if bundled.is_file() {
        return Some(vec![bundled.to_string_lossy().into_owned()]);
    }
    let settings = preferences::load(&preferences::file(home));
    let saved = settings.get("shim")?.as_array()?;
    let command: Vec<String> = saved
        .iter()
        .map(|part| part.as_str().unwrap_or("").to_owned())
        .collect();
    (!command.is_empty() && Path::new(&command[0]).is_file()).then_some(command)
}

/// Remembers the shim chosen by hand, leaving the rest of the file as it was.
pub fn remember(home: &Path, command: &[String]) -> Result<()> {
    let mut settings = preferences::load(&preferences::file(home));
    settings.insert(
        "shim".into(),
        Value::Array(command.iter().cloned().map(Value::String).collect()),
    );
    preferences::save(home, &settings)
}
