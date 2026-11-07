//! A backup file: JSON, with in clear only what lets the user recognize it (when, from which
//! computer and system, which Clak) and the locked key; the content sealed.
//!
//! ```json
//! { "kind": "clak-backup", "version": 1, "created": 1792…, "computer": "STUDIO",
//!   "system": "windows", "clak": "0.3.0",
//!   "lock": { "kdf": "argon2id", … }, "sealed": { "ephemeral": "…", "nonce": "…", "data": "…" } }
//! ```
//!
//! In the chosen folder they are named `Clak backup 2026-11-07 16-05-03.clakbackup`, and
//! only the newest are kept: the others with that name are removed, and nothing else in the
//! folder is touched.

use crate::contents::Contents;
use crate::seal::{Lock, SealError, Sealed};
use clak_wire::json as wire;
use serde_json::{Value, json};
use std::io;
use std::path::{Path, PathBuf};

const KIND: &str = "clak-backup";
const VERSION: u64 = 1;

pub const EXTENSION: &str = "clakbackup";
const PREFIX: &str = "Clak backup ";

/// What a backup says of itself, readable without the password.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub created: f64,
    pub computer: String,
    /// `windows`, `macos`, `linux`.
    pub system: String,
    pub clak: String,
}

impl Header {
    /// This computer, now.
    pub fn here() -> Header {
        Header {
            created: wire::now(),
            computer: computer_name(),
            system: std::env::consts::OS.to_owned(),
            clak: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "created": wire::float(self.created),
            "computer": self.computer,
            "system": self.system,
            "clak": self.clak,
        })
    }
}

/// Why a file can't be read as a backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    NotABackup,
    /// Made by a newer Clak, in a format this one doesn't know.
    Newer,
    Sealed(SealError),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::NotABackup => f.write_str("This isn't a backup of Clak."),
            FileError::Newer => {
                f.write_str("This backup comes from a newer Clak: update Clak to restore it.")
            }
            FileError::Sealed(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for FileError {}

impl From<SealError> for FileError {
    fn from(e: SealError) -> Self {
        FileError::Sealed(e)
    }
}

/// A backup read from its file, still locked.
#[derive(Debug, Clone)]
pub struct Backup {
    pub header: Header,
    lock: Lock,
    sealed: Sealed,
}

impl Backup {
    /// `contents` sealed for `lock`: no password needed.
    pub fn seal(contents: &Contents, lock: &Lock, header: Header) -> Backup {
        Backup {
            header,
            lock: lock.clone(),
            sealed: lock.seal(&wire::encode_pretty(&contents.to_json())),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut file = json!({ "kind": KIND, "version": VERSION });
        if let (Some(file), Value::Object(header)) = (file.as_object_mut(), self.header.to_json()) {
            file.extend(header);
            file.insert("lock".into(), self.lock.to_json());
            file.insert("sealed".into(), self.sealed.to_json());
        }
        wire::encode_pretty(&file)
    }

    pub fn decode(bytes: &[u8]) -> Result<Backup, FileError> {
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let file = wire::parse(bytes).map_err(|_| FileError::NotABackup)?;
        if file.get("kind").and_then(Value::as_str) != Some(KIND) {
            return Err(FileError::NotABackup);
        }
        match file.get("version").and_then(Value::as_u64) {
            Some(VERSION) => {}
            Some(v) if v > VERSION => return Err(FileError::Newer),
            _ => return Err(FileError::NotABackup),
        }
        let text = |key: &str| {
            file.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        Ok(Backup {
            header: Header {
                created: file.get("created").and_then(Value::as_f64).unwrap_or(0.0),
                computer: text("computer"),
                system: text("system"),
                clak: text("clak"),
            },
            lock: file
                .get("lock")
                .and_then(Lock::from_json)
                .ok_or(SealError::Damaged)?,
            sealed: file
                .get("sealed")
                .and_then(Sealed::from_json)
                .ok_or(SealError::Damaged)?,
        })
    }

    /// The content, with the password. Slow (Argon2): never under a lock, nor on an async
    /// thread.
    pub fn open(&self, password: &str) -> Result<Contents, FileError> {
        let plain = self.lock.open(password)?.unseal(&self.sealed)?;
        let plain = zeroize::Zeroizing::new(plain);
        let value = wire::parse(&plain).map_err(|_| SealError::Damaged)?;
        Ok(Contents::from_json(&value))
    }
}

/// The name of a backup made at `created` (seconds since the epoch), in local time.
pub fn file_name(created: f64) -> String {
    let at = chrono::DateTime::from_timestamp(created as i64, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Local);
    format!("{PREFIX}{}.{EXTENSION}", at.format("%Y-%m-%d %H-%M-%S"))
}

/// Writes `backup` into `folder` (made if missing), through a draft, then keeps only the
/// newest `keep` backups there. Where it went.
pub fn write(folder: &Path, backup: &Backup, keep: usize) -> io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let name = file_name(backup.header.created);
    let mut path = folder.join(&name);
    let mut n = 2;
    while path.exists() {
        path = folder.join(name.replace(&format!(".{EXTENSION}"), &format!(" ({n}).{EXTENSION}")));
        n += 1;
    }
    let draft = path.with_extension(format!("{EXTENSION}.tmp"));
    private_write(&draft, &backup.encode())?;
    std::fs::rename(&draft, &path)?;
    prune(folder, keep.max(1));
    Ok(path)
}

/// The backups in `folder`, newest first.
pub fn list(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(PREFIX) && name.ends_with(&format!(".{EXTENSION}"))
        })
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some((meta.modified().ok()?, e.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    found.into_iter().map(|(_, path)| path).collect()
}

fn prune(folder: &Path, keep: usize) {
    for old in list(folder).into_iter().skip(keep) {
        let _ = std::fs::remove_file(old);
    }
}

/// The user's alone on Unix (0600): it holds the tokens, even if sealed.
#[cfg(unix)]
fn private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}

/// The computer's name, as the user knows it: in the restore it says where a backup comes from.
fn computer_name() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
    #[cfg(unix)]
    {
        let mut buffer = [0u8; 256];
        // SAFETY: the buffer is valid for its length, and gethostname writes at most that.
        let done = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        if done != 0 {
            return String::new();
        }
        let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        let name = String::from_utf8_lossy(&buffer[..end]).into_owned();
        name.strip_suffix(".local")
            .map(str::to_owned)
            .unwrap_or(name)
    }
    #[cfg(not(any(windows, unix)))]
    {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::Cost;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clak-backup-{name}-{}",
            clak_wire::json::random_hex(4)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_backup_reads_back_its_header_without_the_password() {
        let lock = Lock::new("a long password", Cost::CHEAP).unwrap();
        let backup = Backup::seal(&Contents::default(), &lock, Header::here());
        let bytes = backup.encode();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("\"kind\": \"clak-backup\""), "{text}");
        let back = Backup::decode(&bytes).unwrap();
        assert_eq!(back.header, backup.header);
        assert_eq!(back.header.system, std::env::consts::OS);
        assert_eq!(back.open("a long password").unwrap(), Contents::default());
        assert_eq!(
            back.open("wrong password").err(),
            Some(FileError::Sealed(SealError::WrongPassword))
        );
    }

    #[test]
    fn other_files_and_newer_versions_are_told_apart() {
        assert_eq!(Backup::decode(b"hello").err(), Some(FileError::NotABackup));
        assert_eq!(
            Backup::decode(br#"{"kind":"clak-config","version":1}"#).err(),
            Some(FileError::NotABackup)
        );
        assert_eq!(
            Backup::decode(br#"{"kind":"clak-backup","version":2}"#).err(),
            Some(FileError::Newer)
        );
        assert_eq!(
            Backup::decode(br#"{"kind":"clak-backup","version":1}"#).err(),
            Some(FileError::Sealed(SealError::Damaged))
        );
        assert!(FileError::Newer.to_string().contains("newer Clak"));
    }

    #[test]
    fn the_folder_keeps_the_newest_and_nothing_else_is_touched() {
        let dir = temp("folder");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "mine").unwrap();
        std::fs::write(dir.join("Clak backup old.json"), "mine too").unwrap();
        let lock = Lock::new("a long password", Cost::CHEAP).unwrap();
        let mut written = Vec::new();
        for _ in 0..4 {
            let backup = Backup::seal(&Contents::default(), &lock, Header::here());
            written.push(write(&dir, &backup, 2).unwrap());
            // Their times must differ for the order.
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The same second: a second name, not an overwrite.
        assert_ne!(written[0], written[1]);
        let kept = list(&dir);
        assert_eq!(kept, vec![written[3].clone(), written[2].clone()]);
        assert!(dir.join("notes.txt").exists());
        assert!(dir.join("Clak backup old.json").exists());
        assert!(
            std::fs::read_dir(&dir).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&kept[0]).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_name_says_when() {
        let name = file_name(1_792_000_000.0);
        assert!(name.starts_with("Clak backup 20"), "{name}");
        assert!(name.ends_with(".clakbackup"), "{name}");
        assert!(
            !name.contains(':'),
            "a colon isn't allowed on Windows: {name}"
        );
    }
}
