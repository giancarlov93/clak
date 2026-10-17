//! Where the hub lives: its folder, the lockfile, the control channel.
//!
//! The folder is `~/.clak` (or `CLAK_HOME`) on both systems. The control channel is a
//! named pipe on Windows (`\\.\pipe\clak-<hash>`) and a Unix socket on the Mac
//! (`<home>/control.sock`): names derived from the folder, so programs, command line and
//! hubs of different versions find each other.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The variable that moves the hub.
pub const HOME_VARIABLE: &str = "CLAK_HOME";

#[cfg(windows)]
pub const PIPE_PREFIX: &str = r"\\.\pipe\";

/// `~/.clak`, or `CLAK_HOME`. The same folder written two ways (forward slashes,
/// trailing slash, `..`) must stay the same folder: the control channel and the hub's
/// single instance depend on it.
pub fn home() -> PathBuf {
    match std::env::var(HOME_VARIABLE) {
        Ok(custom) if !custom.trim().is_empty() => normalized(Path::new(&custom)),
        _ => standard_home(),
    }
}

/// The hub's folder when no variable moves it.
pub fn standard_home() -> PathBuf {
    user_home().join(".clak")
}

pub fn user_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// The file that says where the live hub listens, with the programs' token.
pub fn lockfile(home: &Path) -> PathBuf {
    home.join("daemon.json")
}

/// The file with which the user says "I closed the hub myself, don't reopen it": the app
/// writes it on quit and removes it when reopened; command line and shim respect it.
pub fn pause_flag(home: &Path) -> PathBuf {
    home.join("paused")
}

/// On the Mac, held with `flock` for the hub's whole life: it is its mutex.
pub fn hub_lock(home: &Path) -> PathBuf {
    home.join("hub.lock")
}

/// What the lockfile says: programs' port, token, control channel, pid, protocol
/// version.
#[derive(Debug, Clone)]
pub struct Lockfile {
    pub ws_port: u16,
    pub token: String,
    pub control: String,
    pub pid: u32,
    pub protocol: u64,
}

impl Lockfile {
    pub fn read(home: &Path) -> Option<Lockfile> {
        let bytes = std::fs::read(lockfile(home)).ok()?;
        let value = crate::json::parse(&bytes).ok()?;
        Some(Lockfile {
            ws_port: value.get("ws_port")?.as_u64()? as u16,
            token: value.get("token")?.as_str()?.to_owned(),
            control: value.get("control")?.as_str()?.to_owned(),
            pid: value.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
            protocol: value.get("protocol").and_then(Value::as_u64).unwrap_or(1),
        })
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "ws_port": self.ws_port,
            "token": self.token,
            "control": self.control,
            "pid": self.pid,
            "protocol": self.protocol,
        })
    }
}

/// `clak-<12 digits>`: the Windows pipe name, the same computation on the Mac for the
/// fallback socket. Over the folder as written on each system.
pub fn hash_name(home: &Path) -> String {
    let text = home.to_string_lossy();
    #[cfg(windows)]
    let text = {
        // Backslashes, no trailing slash.
        let replaced = text.replace('/', "\\");
        replaced.trim_end_matches('\\').to_owned()
    };
    let digest = hex::encode(Sha256::digest(text.as_bytes()));
    format!("clak-{}", &digest[..12])
}

/// Where this folder's hub listens, without looking at the lockfile: a lockfile left over
/// by a dead hub must not decide where the next one listens.
pub fn computed_control(home: &Path) -> String {
    #[cfg(windows)]
    {
        format!("{PIPE_PREFIX}{}", hash_name(home))
    }
    #[cfg(unix)]
    {
        // `sun_path` holds 103 bytes: with a long folder the socket goes in the user's
        // temporary folder, with a name derived from the folder's hash.
        let inside = home.join("control.sock").to_string_lossy().into_owned();
        if fits_unix_path(&inside) {
            return inside;
        }
        user_temporary_directory()
            .join(format!("{}.sock", hash_name(home)))
            .to_string_lossy()
            .into_owned()
    }
}

/// Where a caller connects: the live hub's lockfile says so, otherwise it is recomputed.
pub fn control(home: &Path) -> String {
    if let Some(lock) = Lockfile::read(home) {
        #[cfg(windows)]
        let valid = lock.control.starts_with(PIPE_PREFIX);
        #[cfg(unix)]
        let valid = lock.control.starts_with('/');
        if valid {
            return lock.control;
        }
    }
    computed_control(home)
}

#[cfg(unix)]
pub fn fits_unix_path(path: &str) -> bool {
    path.len() < 104
}

/// The user's temporary folder according to the system (`/var/folders/…/T`), not
/// according to `TMPDIR`: it must be the same for the hub and for whoever looks for it.
#[cfg(unix)]
pub fn user_temporary_directory() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        const CS_DARWIN_USER_TEMP_DIR: libc::c_int = 65537;
        let mut buffer = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: the buffer is as long as declared.
        let n = unsafe { libc::confstr(CS_DARWIN_USER_TEMP_DIR, buffer.as_mut_ptr().cast(), buffer.len()) };
        if n > 0 {
            let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
            return normalized(Path::new(&String::from_utf8_lossy(&buffer[..end]).into_owned()));
        }
    }
    normalized(&std::env::temp_dir())
}

/// An absolute, normalized path.
///
/// On Windows absolute, with `.` and `..` resolved, without the trailing slash. On the Mac
/// without `.` and `..` and with symlinks resolved for the part that exists: `/tmp/hub` and
/// `/private/tmp/hub` are the same folder, and must give the same hub.
pub fn normalized(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let text = full.to_string_lossy().into_owned();
        let trimmed = text.trim_end_matches(['\\', '/']);
        // "C:\" stays "C:\": without the slash it would be the drive's current folder.
        if trimmed.len() <= 2 { full } else { PathBuf::from(trimmed) }
    }
    #[cfg(unix)]
    {
        let text = path.to_string_lossy();
        let expanded = if let Some(rest) = text.strip_prefix("~/") {
            user_home().join(rest).to_string_lossy().into_owned()
        } else if text == "~" {
            user_home().to_string_lossy().into_owned()
        } else {
            text.into_owned()
        };
        let full = if expanded.starts_with('/') {
            expanded
        } else {
            format!("{}/{}", std::env::current_dir().unwrap_or_default().to_string_lossy(), expanded)
        };
        let mut parts: Vec<&str> = Vec::new();
        for part in full.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => parts.push(other),
            }
        }
        for existing in (1..=parts.len()).rev() {
            let prefix = format!("/{}", parts[..existing].join("/"));
            if let Ok(real) = std::fs::canonicalize(&prefix) {
                let rest = parts[existing..].join("/");
                let base = real.to_string_lossy().into_owned();
                return PathBuf::from(if rest.is_empty() {
                    base
                } else if base == "/" {
                    format!("/{rest}")
                } else {
                    format!("{base}/{rest}")
                });
            }
        }
        PathBuf::from(format!("/{}", parts.join("/")))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn il_nome_della_pipe_non_dipende_da_come_si_scrive_la_cartella() {
        // sha256 of the folder with backslashes and no trailing slash.
        let a = hash_name(Path::new(r"C:\Users\x\.clak"));
        let b = hash_name(Path::new("C:/Users/x/.clak/"));
        assert_eq!(a, b);
        assert!(a.starts_with("clak-") && a.len() == 17);
        assert!(computed_control(Path::new(r"C:\Users\x\.clak")).starts_with(r"\\.\pipe\clak-"));
    }

    #[test]
    fn la_cartella_si_normalizza() {
        assert_eq!(normalized(Path::new(r"C:\a\b\..\c\")), PathBuf::from(r"C:\a\c"));
        assert_eq!(normalized(Path::new("C:/a/c")), PathBuf::from(r"C:\a\c"));
    }
}
