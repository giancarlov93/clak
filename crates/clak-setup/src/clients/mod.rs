//! The clients that can host the shim, and the configuration entry they need.
//!
//! Each client keeps MCP servers in a place of its own and in a shape of its own. Rules,
//! the same for all:
//!
//!  - a file that can't be read in full (JSON with comments, broken TOML) is NOT
//!    written: the error reaches the user together with the snippet to paste by
//!    hand;
//!  - before the first change the file is copied alongside, to `<name>.clak-bak`;
//!  - the write is atomic (temporary file and rename) and preserves keys, order and
//!    indentation of what was there (on the Mac the permissions too);
//!  - the configuration always gets the full path of the shim: an app opened from the
//!    Finder doesn't have the shell's PATH, and a bare name wouldn't find it;
//!  - Claude Code is not edited by hand: it rewrites its .claude.json all the time,
//!    so we go through its `claude mcp` command.

mod catalog;
mod claude_code;
mod codex;
pub mod import;
mod json;
mod json_file;
mod toml;

pub use catalog::all;
pub use claude_code::{ClaudeCodeClient, CommandRunner};
pub use codex::CodexClient;
pub use json_file::JsonFileClient;

use crate::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    /// The client doesn't seem to be on this machine.
    NotInstalled,
    /// It is there, and doesn't have the entry.
    Absent,
    /// It has the entry, and it points to this shim.
    Ok,
    /// It has the entry, but it points elsewhere (another copy, moved or removed).
    Stale,
    /// Its file can't be read: it won't be touched.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStatus {
    pub state: ClientState,
    /// The file that would be touched.
    pub path: PathBuf,
    /// The command configured today, or the reason it can't be read.
    pub detail: String,
    /// The entry's executable, if any: tells whether it points to a shim that no longer exists.
    pub command: Option<String>,
}

impl ClientStatus {
    pub fn new(state: ClientState, path: impl Into<PathBuf>) -> ClientStatus {
        ClientStatus {
            state,
            path: path.into(),
            detail: String::new(),
            command: None,
        }
    }

    pub fn with(
        state: ClientState,
        path: impl Into<PathBuf>,
        detail: impl Into<String>,
        command: Option<String>,
    ) -> ClientStatus {
        ClientStatus {
            state,
            path: path.into(),
            detail: detail.into(),
            command,
        }
    }
}

/// The name under which the shim appears in every client.
pub const SERVER: &str = "clak";

pub trait McpClient: Send + Sync {
    fn name(&self) -> &str;

    /// What the user must do for the change to show; empty when there is nothing to do.
    fn note(&self) -> &str;

    /// The shim command: executable and arguments.
    fn command(&self) -> &[String];

    fn status(&self) -> ClientStatus;
    fn add(&self) -> Result<()>;
    fn remove(&self) -> Result<()>;

    /// What the user would paste by hand, if they prefer or if it can't be written.
    fn snippet(&self) -> String;
}

/// Is the entry found ours? Same executable and same arguments.
///
/// On Windows forward or back slashes, trailing slash and case don't count; on the Mac
/// `//`, `.`, `..` and the trailing slash.
pub(crate) fn same(command: &[String], executable: Option<&str>, args: &[String]) -> bool {
    let Some(executable) = executable else {
        return false;
    };
    let Some(ours) = command.first() else {
        return false;
    };
    same_exe_text(executable, ours) && args == &command[1..]
}

fn same_exe_text(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        let normalize = |p: &str| p.replace('/', "\\").trim_end_matches('\\').to_lowercase();
        normalize(a) == normalize(b)
    } else {
        same_path_text(a, b)
    }
}

/// The same path written two ways: `//`, `.`, `..` and the trailing slash don't count.
/// On the text only, without touching the disk: the app it points to may be gone.
pub(crate) fn same_path_text(a: &str, b: &str) -> bool {
    fn normal(path: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
            if part == ".." {
                parts.pop();
            } else {
                parts.push(part);
            }
        }
        parts
    }
    a.starts_with('/') == b.starts_with('/') && normal(a) == normal(b)
}

pub(crate) fn line(executable: Option<&str>, args: &[String]) -> String {
    let mut parts = vec![executable.unwrap_or("")];
    parts.extend(args.iter().map(String::as_str));
    parts.join(" ").trim().to_owned()
}

pub(crate) mod safe_file {
    use crate::places::{parent, suffixed};
    use crate::{Result, SetupError};
    use std::path::{Path, PathBuf};

    fn name(path: &Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The text, and whether it started with a BOM (so it can be put back). A missing
    /// file is empty.
    pub fn read(path: &Path) -> Result<(String, bool)> {
        if !path.exists() {
            return Ok((String::new(), false));
        }
        let bytes = std::fs::read(path)
            .map_err(|e| SetupError(format!("{} can't be read: {e}", name(path))))?;
        let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let body = if bom { bytes[3..].to_vec() } else { bytes };
        let text = String::from_utf8(body)
            .map_err(|_| SetupError(format!("{} isn't UTF-8: not touching it.", name(path))))?;
        Ok((text, bom))
    }

    pub fn write(path: &Path, text: &str, bom: bool) -> Result<()> {
        // A configuration file can be a link (dotfiles managed with stow or similar):
        // we write where it points, not over the link.
        let target: PathBuf = if cfg!(unix) && path.exists() {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        } else {
            path.to_path_buf()
        };
        let attempt = || -> std::io::Result<()> {
            let backup = suffixed(&target, ".clak-bak");
            if target.exists() && !backup.exists() {
                std::fs::copy(&target, &backup)?;
            }
            std::fs::create_dir_all(parent(&target))?;
            let draft = suffixed(&target, ".clak-tmp");
            let mut bytes = if bom {
                vec![0xEF, 0xBB, 0xBF]
            } else {
                Vec::new()
            };
            bytes.extend_from_slice(text.as_bytes());
            std::fs::write(&draft, bytes)?;
            // A configuration can contain keys and secrets (the servers' environment
            // variables): the new copy has the same permissions as the old one.
            #[cfg(unix)]
            if let Ok(metadata) = std::fs::metadata(&target) {
                let _ = std::fs::set_permissions(&draft, metadata.permissions());
            }
            if let Err(e) = std::fs::rename(&draft, &target) {
                let _ = std::fs::remove_file(&draft);
                return Err(e);
            }
            Ok(())
        };
        attempt().map_err(|e| SetupError(format!("Can't write {}: {e}", target.display())))
    }
}
