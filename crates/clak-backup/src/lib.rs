//! Clak's backups: everything the user set up, tokens included, in a file locked with a
//! password of theirs, written by itself into a folder they choose, and restored after a
//! check on the computer it is restored on.
//!
//! - [`seal`]: the password opens a key, the key opens the backups; the automatic ones need
//!   only its public half, so the password is never kept.
//! - [`contents`]: what goes in (servers, APIs, presets, allowed applications, rules, with
//!   their secrets) and its fingerprint, to write one only when something changed.
//! - [`file`]: the file, its name, the folder and how many are kept.
//! - [`settings`]: `backup.json`, off until the user turns it on.
//! - [`plan`]: what a backup would restore here, checked item by item: a path of another
//!   computer, another system, another program at the same path.
//! - [`restore`]: the items the user ticks, on the running hub.
//!
//! The app schedules it and asks the questions; the hub without an interface may use it
//! too.

pub mod contents;
pub mod file;
pub mod plan;
pub mod restore;
pub mod seal;
pub mod settings;

pub use contents::Contents;
pub use file::{Backup, FileError, Header};
pub use plan::{Check, Here, Item, Kind, Plan};
pub use restore::{Outcome, Targets, restore};
pub use seal::{Cost, Lock, MIN_PASSWORD, SealError};
pub use settings::Settings;

use clak_apis::ApiStore;
use clak_connectors::ConnectorStore;
use std::path::{Path, PathBuf};

/// How a backup went.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    /// Written there.
    Written(PathBuf),
    /// Nothing changed since the last one.
    Unchanged,
}

/// A backup now, if something changed since the last one (`always`: in any case), into the
/// folder of `backup.json`; and how it went, there too. Slow (it reads the secrets, on the
/// Mac from the Keychain): never under a lock, nor on an async thread. Whoever calls it
/// makes sure two don't run at once.
pub fn back_up(
    home: &Path,
    connectors: &ConnectorStore,
    apis: &ApiStore,
    always: bool,
) -> Result<Done, String> {
    let mut settings = Settings::load(home);
    let (Some(lock), Some(mark), Some(folder)) = (
        settings.lock.clone(),
        settings.mark_key(),
        settings.folder.clone(),
    ) else {
        return Err("Choose a folder and a password first.".into());
    };
    let contents = Contents::collect(home, connectors, apis);
    let fingerprint = contents.fingerprint(&mark);
    if !always && settings.fingerprint.as_deref() == Some(fingerprint.as_str()) {
        return Ok(Done::Unchanged);
    }
    let backup = Backup::seal(&contents, &lock, Header::here());
    let result = file::write(&folder, &backup, settings.keep);
    settings.last = Some(settings::Last {
        at: backup.header.created,
        file: result
            .as_ref()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        error: result
            .as_ref()
            .err()
            .map(|e| format!("Couldn't write into {}: {e}", folder.display())),
    });
    if result.is_ok() {
        settings.fingerprint = Some(fingerprint);
    }
    let saved = settings.save(home);
    let path = result.map_err(|e| format!("Couldn't write into {}: {e}", folder.display()))?;
    saved.map_err(|e| format!("Couldn't save the backup's settings: {e}"))?;
    Ok(Done::Written(path))
}
