//! OpenCode keeps its sessions in a SQLite database, `<data>/opencode/opencode.db` (the
//! `session_v2` table), and before that in a JSON file per session,
//! `<data>/opencode/storage/session/<project>/<id>.json`. `<data>` is `$XDG_DATA_HOME`, or
//! `~/.local/share` on every system. Both are only read, the database without a lock on it:
//! OpenCode may be writing. A session resumes with `opencode --session <id>` in its folder;
//! the sessions an agent of OpenCode started on its own (with a parent) are left out, as
//! `opencode session list` does.

use super::{Conversation, shorten, text};
use crate::{Places, Result, SetupError};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) fn list(places: &Places) -> Result<Vec<Conversation>> {
    let data = data(places);
    let database = data.join("opencode.db");
    if database.is_file() {
        return from_database(&database);
    }
    Ok(from_files(&data.join("storage").join("session")))
}

fn data(places: &Places) -> PathBuf {
    match places.var("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => places.home.join(".local").join("share"),
    }
    .join("opencode")
}

fn from_database(path: &Path) -> Result<Vec<Conversation>> {
    let unreadable =
        |e: rusqlite::Error| SetupError(format!("Can't read OpenCode's sessions: {e}"));
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(unreadable)?;
    db.busy_timeout(Duration::from_secs(2))
        .map_err(unreadable)?;
    let mut query = db
        .prepare(
            "SELECT id, directory, title, time_updated FROM session_v2 \
             WHERE parent_id IS NULL AND time_archived IS NULL",
        )
        .map_err(|_| {
            SetupError(
                "This version of OpenCode keeps its sessions in a way Clak doesn't read.".into(),
            )
        })?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .map_err(unreadable)?;
    Ok(rows
        .flatten()
        .map(|(id, folder, title, updated)| session(id, folder, title, updated))
        .collect())
}

fn from_files(dir: &Path) -> Vec<Conversation> {
    let read = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten();
    read(dir)
        .flat_map(|project| read(&project.path()))
        .filter_map(|file| {
            let entry: Value = serde_json::from_slice(&std::fs::read(file.path()).ok()?).ok()?;
            if entry.get("parentID").is_some_and(|p| !p.is_null()) {
                return None;
            }
            let updated = entry
                .get("time")
                .and_then(|t| t.get("updated"))
                .and_then(Value::as_i64);
            Some(session(
                text(&entry, "id")?,
                text(&entry, "directory"),
                text(&entry, "title"),
                updated,
            ))
        })
        .collect()
}

fn session(
    id: String,
    folder: Option<String>,
    title: Option<String>,
    updated: Option<i64>,
) -> Conversation {
    let folder = folder.filter(|f| !f.trim().is_empty());
    let title = title
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            folder
                .as_deref()
                .and_then(|f| Path::new(f).file_name())
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Untitled session".into());
    Conversation {
        id,
        title: shorten(&title),
        named: false,
        folder,
        updated: updated.unwrap_or(0).max(0) as u64,
        terminal: None,
        size: 0,
    }
}
