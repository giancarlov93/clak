//! A fake machine: profile, AppData and environment in a temporary folder. The user's
//! real files are never touched.

#![allow(dead_code)]

use clak_setup::Places;
use clak_setup::clients::{self, McpClient};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub const SHIM: &str = r"C:\Program Files\Clak\clak-shim.exe";
#[cfg(not(windows))]
pub const SHIM: &str = "/Applications/Clak.app/Contents/MacOS/clak-shim";

pub struct Sandbox {
    pub root: PathBuf,
    pub places: Places,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        Sandbox::with_environment(BTreeMap::new())
    }

    pub fn with_environment(environment: BTreeMap<String, String>) -> Sandbox {
        let root = std::env::temp_dir().join(format!("clak-setup-{}", clak_wire::json::random_hex(8)));
        std::fs::create_dir_all(&root).unwrap();
        // On the Mac /var/folders is a link to /private/var/folders: the real path avoids surprises.
        let root = std::fs::canonicalize(&root).map(|p| strip_verbatim(&p)).unwrap_or(root);
        let places = Places::new(root.join("home"), root.join("appdata"), root.join("localappdata"), environment);
        let sandbox = Sandbox { root, places };
        sandbox.dir(&["home"]);
        sandbox.dir(&["appdata"]);
        sandbox.dir(&["localappdata"]);
        sandbox
    }

    pub fn dir(&self, parts: &[&str]) -> PathBuf {
        let path = parts.iter().fold(self.root.clone(), |p, part| p.join(part));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        relative.split('/').fold(self.root.clone(), |p, part| p.join(part))
    }

    pub fn write(&self, relative: &str, text: &str) -> PathBuf {
        self.write_bytes(relative, text.as_bytes())
    }

    pub fn write_bom(&self, relative: &str, text: &str) -> PathBuf {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(text.as_bytes());
        self.write_bytes(relative, &bytes)
    }

    fn write_bytes(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    pub fn client(&self, name: &str) -> Box<dyn McpClient> {
        clients::all(&self.places, &[SHIM.to_owned()]).into_iter().find(|c| c.name() == name).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `\\?\C:\…` → `C:\…`: paths are compared as the user writes them.
fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned())
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

pub fn json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&read(path)).unwrap()
}

pub fn keys(value: &serde_json::Value) -> Vec<String> {
    value.as_object().unwrap().keys().cloned().collect()
}
