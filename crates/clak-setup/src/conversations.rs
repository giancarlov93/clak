//! The past conversations of an agent, from the Agents tab: listed, resumed in the folder
//! where they took place, renamed, deleted; their folder opened.
//!
//! They are the agent's files, not Clak's. Claude Code keeps one per conversation,
//! `<config>/projects/<folder>/<id>.jsonl`, one JSON object per line, and only ever adds
//! lines to it: renaming adds one more line, as its own `/rename` does. Deleting removes
//! that file and the folders named after the conversation, as nothing in Claude Code does:
//! nothing else is written.
//!
//! Claude Desktop runs Claude Code in its Code tab, and keeps what it shows of each session
//! (its title, its folder, when it was last used) in a file of its own,
//! `Claude/claude-code-sessions/<account>/<organization>/local_<id>.json`. Those are only
//! read: the app keeps them in memory while it runs, and renames and deletes them itself. A
//! session resumes in the app, through the link the app gives its own recent sessions.

use crate::{Places, Result, SetupError};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// How long a title can be, in characters: the first prompt is cut there.
const TITLE_LENGTH: usize = 120;

/// A past conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub id: String,
    /// Given with a rename, or else the first thing the user asked.
    pub title: String,
    /// Renamed: the title is the user's own.
    pub named: bool,
    /// Where it took place, and where it resumes.
    pub folder: Option<String>,
    /// Last written, in milliseconds since 1970.
    pub updated: u64,
    /// The id `claude --resume` takes, to resume it in a terminal: Claude Desktop's sessions
    /// have one of their own.
    pub terminal: Option<String>,
}

impl Conversation {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "named": self.named,
            "folder": self.folder,
            "updated": self.updated,
            "terminal": self.terminal,
        })
    }
}

const CLAUDE_CODE: &str = "claude-code";
const CLAUDE_DESKTOP: &str = "claude-desktop";

/// Whether Clak reads this agent's conversations (by its catalog id, see `launcher`).
pub fn supported(agent: &str) -> bool {
    agent == CLAUDE_CODE || agent == CLAUDE_DESKTOP
}

/// Whether Clak renames and deletes them too: not those Claude Desktop keeps itself.
pub fn editable(agent: &str) -> bool {
    agent == CLAUDE_CODE
}

/// The agent's conversations, the latest first. None yet: an empty list.
pub fn list(places: &Places, agent: &str) -> Result<Vec<Conversation>> {
    if !supported(agent) {
        return Err(unsupported(agent));
    }
    if agent == CLAUDE_DESKTOP {
        let mut all = desktop_sessions(places);
        all.sort_by_key(|c| std::cmp::Reverse(c.updated));
        return Ok(all);
    }
    let mut all = Vec::new();
    let Ok(projects) = std::fs::read_dir(claude_projects(places)) else {
        return Ok(all);
    };
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_some_and(|e| e == "jsonl")
                && let Some(conversation) = claude_conversation(&path)
            {
                all.push(conversation);
            }
        }
    }
    all.sort_by_key(|c| std::cmp::Reverse(c.updated));
    Ok(all)
}

/// Gives the conversation a title, as the agent itself does.
pub fn rename(places: &Places, agent: &str, id: &str, title: &str) -> Result<()> {
    if !editable(agent) {
        return Err(not_editable(agent));
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err(SetupError("The name is empty.".into()));
    }
    let title: String = title.chars().take(TITLE_LENGTH).collect();
    let path = claude_file(places, id)?;
    let line = json!({ "type": "custom-title", "customTitle": title, "sessionId": id });
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(&path)
        .map_err(|e| SetupError(format!("Can't write {}: {e}", path.display())))?;
    // A last line without its newline would swallow ours.
    let mut last = [0u8; 1];
    let ends_open = file.seek(SeekFrom::End(-1)).is_ok()
        && file.read_exact(&mut last).is_ok()
        && last[0] != b'\n';
    let text = format!("{}{line}\n", if ends_open { "\n" } else { "" });
    file.write_all(text.as_bytes())
        .map_err(|e| SetupError(format!("Can't write {}: {e}", path.display())))
}

/// Deletes the conversation: its file, and what Claude Code keeps beside it under its id
/// (the subagents and the long tool results, the files' earlier versions, the environment).
/// The prompts in the shared `history.jsonl` stay: that file is every conversation's.
pub fn delete(places: &Places, agent: &str, id: &str) -> Result<()> {
    if !editable(agent) {
        return Err(not_editable(agent));
    }
    let path = claude_file(places, id)?;
    std::fs::remove_file(&path)
        .map_err(|e| SetupError(format!("Can't delete {}: {e}", path.display())))?;
    let config = claude_config(places);
    for dir in [
        path.with_extension(""),
        config.join("file-history").join(id),
        config.join("session-env").join(id),
    ] {
        if dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    Ok(())
}

/// What to add to the agent's command line to resume the conversation, and the folder to
/// start it in.
pub fn resume(places: &Places, agent: &str, id: &str) -> Result<(Vec<String>, Option<PathBuf>)> {
    if agent == CLAUDE_DESKTOP {
        return Err(SetupError(
            "Claude Desktop's sessions resume in the app.".into(),
        ));
    }
    let folder = folder(places, agent, id)?;
    Ok((vec!["--resume".into(), id.into()], folder))
}

/// The link that opens one of Claude Desktop's sessions in the app: the one the app gives
/// its own recent sessions, in the Dock and in Spotlight.
pub fn link(places: &Places, agent: &str, id: &str) -> Result<String> {
    if agent != CLAUDE_DESKTOP {
        return Err(SetupError(format!(
            "The conversations of “{agent}” resume in a terminal."
        )));
    }
    let session = desktop_session(places, id)?;
    Ok(format!("claude://code/continue?session={}", session.id))
}

/// The folder where the conversation took place, if it still is there. None: it never said.
pub fn folder(places: &Places, agent: &str, id: &str) -> Result<Option<PathBuf>> {
    if !supported(agent) {
        return Err(unsupported(agent));
    }
    let folder = if agent == CLAUDE_DESKTOP {
        desktop_session(places, id)?.folder
    } else {
        claude_conversation(&claude_file(places, id)?).and_then(|c| c.folder)
    }
    .map(PathBuf::from);
    if let Some(folder) = folder.as_ref().filter(|f| !f.is_dir()) {
        return Err(SetupError(format!(
            "The folder of this conversation is gone: {}",
            folder.display()
        )));
    }
    Ok(folder)
}

fn unsupported(agent: &str) -> SetupError {
    SetupError(format!(
        "Clak doesn't read the conversations of “{agent}” yet."
    ))
}

fn not_editable(agent: &str) -> SetupError {
    if agent == CLAUDE_DESKTOP {
        return SetupError("Claude Desktop renames and deletes its sessions itself.".into());
    }
    unsupported(agent)
}

/// Where Claude Desktop keeps its data: on Windows the Store's copy of `%APPDATA%` too.
fn desktop_folders(places: &Places) -> Vec<PathBuf> {
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

/// Claude Desktop's Code sessions that aren't archived: one file each, under the account
/// and the organization.
fn desktop_sessions(places: &Places) -> Vec<Conversation> {
    let read = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten();
    let mut all = Vec::new();
    for folder in desktop_folders(places) {
        for account in read(&folder.join("claude-code-sessions")) {
            for organization in read(&account.path()) {
                for file in read(&organization.path()) {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if name.starts_with("local_")
                        && name.ends_with(".json")
                        && let Some(session) = desktop_conversation(places, &file.path())
                    {
                        all.push(session);
                    }
                }
            }
        }
    }
    all
}

/// One of them, by its id.
fn desktop_session(places: &Places, id: &str) -> Result<Conversation> {
    let valid = id.starts_with("local_")
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(SetupError(format!("“{id}” isn't a conversation.")));
    }
    desktop_sessions(places)
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| SetupError("This conversation is no longer there.".into()))
}

/// A session, from its file: the title the app shows, the folder it was opened on (not the
/// worktree it may work in), when it was last used, and its id in a terminal while Claude
/// Code still has its conversation.
fn desktop_conversation(places: &Places, path: &Path) -> Option<Conversation> {
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
        terminal: text(&entry, "cliSessionId").filter(|id| claude_file(places, id).is_ok()),
    })
}

/// `$CLAUDE_CONFIG_DIR`, or `~/.claude`.
fn claude_config(places: &Places) -> PathBuf {
    match places.var("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => places.home.join(".claude"),
    }
}

fn claude_projects(places: &Places) -> PathBuf {
    claude_config(places).join("projects")
}

/// The file of a conversation, by its id: found in whichever folder it took place.
fn claude_file(places: &Places, id: &str) -> Result<PathBuf> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        return Err(SetupError(format!("“{id}” isn't a conversation.")));
    }
    let name = format!("{id}.jsonl");
    std::fs::read_dir(claude_projects(places))
        .into_iter()
        .flatten()
        .flatten()
        .map(|project| project.path().join(&name))
        .find(|path| path.is_file())
        .ok_or_else(|| SetupError("This conversation is no longer there.".into()))
}

/// A conversation, from its file: its last title, the folder of its first message, its first
/// prompt. Only the lines that can say so are parsed: a file can be tens of megabytes.
fn claude_conversation(path: &Path) -> Option<Conversation> {
    let id = path.file_stem()?.to_str()?.to_owned();
    let updated = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64);
    // The titles given to this conversation, and those of the one it continues, whose lines
    // come along in its file: its own win.
    let mut named = None;
    let mut inherited = None;
    let mut summary = None;
    let mut folder = None;
    let mut prompt = None;
    for line in BufReader::new(File::open(path).ok()?)
        .split(b'\n')
        .map_while(std::result::Result::ok)
    {
        let has = |needle: &[u8]| line.windows(needle.len()).any(|w| w == needle);
        if has(b"\"custom-title\"") {
            if let Ok(entry) = serde_json::from_slice::<Value>(&line)
                && let Some(title) = text(&entry, "customTitle")
            {
                match entry.get("sessionId").and_then(Value::as_str) {
                    Some(owner) if owner != id => inherited = Some(title),
                    _ => named = Some(title),
                }
            }
        } else if has(b"\"summary\"") && summary.is_none() && has(b"\"type\":\"summary\"") {
            summary = serde_json::from_slice::<Value>(&line)
                .ok()
                .and_then(|entry| text(&entry, "summary"));
        } else if (folder.is_none() || prompt.is_none()) && has(b"\"type\":\"user\"") {
            let Ok(entry) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            if folder.is_none() {
                folder = entry.get("cwd").and_then(Value::as_str).map(str::to_owned);
            }
            if prompt.is_none() && entry.get("isMeta").and_then(Value::as_bool) != Some(true) {
                prompt = entry.get("message").and_then(prompt_text);
            }
        }
    }
    // Never a word from the user: nothing to resume.
    let prompt = prompt?;
    let (title, named) = match named.or(inherited) {
        Some(title) => (title, true),
        None => (summary.unwrap_or(prompt), false),
    };
    Some(Conversation {
        terminal: Some(id.clone()),
        id,
        title: shorten(&title),
        named,
        folder,
        updated,
    })
}

fn text(entry: &Value, name: &str) -> Option<String> {
    let text = entry.get(name)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// What the user typed: the text of a message, not a command's output or a reminder.
fn prompt_text(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    let texts: Vec<&str> = match content {
        Value::String(text) => vec![text.as_str()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => return None,
    };
    texts
        .into_iter()
        .map(str::trim)
        .find(|t| !t.is_empty() && !t.starts_with('<') && !t.starts_with("Caveat:"))
        .map(str::to_owned)
}

/// On one line, cut at the length of a title.
fn shorten(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TITLE_LENGTH {
        return line;
    }
    let cut: String = line.chars().take(TITLE_LENGTH - 1).collect();
    format!("{}…", cut.trim_end())
}
