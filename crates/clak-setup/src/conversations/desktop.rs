//! Claude Desktop runs Claude Code in its Code tab, and keeps what it shows of each session
//! (its title, its folder, when it was last used) in a file of its own,
//! `Claude/claude-code-sessions/<account>/<organization>/local_<id>.json`. Those are only
//! read: the app keeps them in memory while it runs, and renames and deletes them itself. A
//! session resumes in the app, through the link the app gives its own recent sessions; what
//! was said is in Claude Code's file, under the session's id in a terminal.

use super::{Conversation, claude, gone, shorten, text};
use crate::{Places, Result, SetupError};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(super) fn list(places: &Places) -> Vec<Conversation> {
    let read = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten();
    let mut all = Vec::new();
    for folder in folders(places) {
        for account in read(&folder.join("claude-code-sessions")) {
            for organization in read(&account.path()) {
                for file in read(&organization.path()) {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if name.starts_with("local_")
                        && name.ends_with(".json")
                        && let Some(session) = conversation(places, &file.path())
                    {
                        all.push(session);
                    }
                }
            }
        }
    }
    all
}

/// The sessions whose conversation, in Claude Code's file, has all the words.
pub(super) fn search(places: &Places, words: &[String]) -> Vec<String> {
    let found = claude::search(places, words);
    list(places)
        .into_iter()
        .filter(|c| c.terminal.as_ref().is_some_and(|t| found.contains(t)))
        .map(|c| c.id)
        .collect()
}

/// One of them, by its id.
pub(super) fn session(places: &Places, id: &str) -> Result<Conversation> {
    let valid = id.starts_with("local_")
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(SetupError(format!("“{id}” isn't a conversation.")));
    }
    list(places)
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(gone)
}

/// Where Claude Desktop keeps its data: on Windows the Store's copy of `%APPDATA%` too.
fn folders(places: &Places) -> Vec<PathBuf> {
    if cfg!(windows) {
        let mut folders: Vec<PathBuf> = std::fs::read_dir(places.local_app_data.join("Packages"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("Claude_"))
            .map(|e| e.path().join("LocalCache").join("Roaming").join("Claude"))
            .collect();
        folders.sort();
        folders.push(places.app_data.join("Claude"));
        folders
    } else {
        vec![places.application_support().join("Claude")]
    }
}

/// A session, from its file: the title the app shows, the folder it was opened on (not the
/// worktree it may work in), when it was last used, and its id in a terminal while Claude
/// Code still has its conversation. Archived ones are left out, as the app does.
fn conversation(places: &Places, path: &Path) -> Option<Conversation> {
    let entry: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if entry.get("isArchived").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let id = text(&entry, "sessionId")?;
    let folder = text(&entry, "originCwd").or_else(|| text(&entry, "cwd"));
    let title = text(&entry, "title")
        .or_else(|| {
            folder
                .as_deref()
                .and_then(|f| Path::new(f).file_name())
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Untitled session".into());
    let updated = ["lastActivityAt", "createdAt"]
        .iter()
        .find_map(|key| entry.get(*key).and_then(Value::as_u64))
        .unwrap_or(0);
    Some(Conversation {
        id,
        title: shorten(&title),
        named: entry.get("titleSource").and_then(Value::as_str) == Some("user"),
        folder,
        updated,
        terminal: text(&entry, "cliSessionId").filter(|id| claude::file(places, id).is_ok()),
        size: 0,
    })
}
