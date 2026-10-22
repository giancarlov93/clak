//! The past conversations of a command-line agent, from the Agents tab: listed, resumed in
//! the folder where they took place, renamed, deleted; their folder opened.
//!
//! They are the agent's files, not Clak's. Claude Code keeps one per conversation,
//! `<config>/projects/<folder>/<id>.jsonl`, one JSON object per line, and only ever adds
//! lines to it: renaming adds one more line, as its own `/rename` does. Deleting removes
//! that file and the folders named after the conversation, as nothing in Claude Code does:
//! nothing else is written.

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
}

impl Conversation {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "named": self.named,
            "folder": self.folder,
            "updated": self.updated,
        })
    }
}

/// Whether Clak reads this agent's conversations (by its catalog id, see `launcher`).
pub fn supported(agent: &str) -> bool {
    agent == "claude-code"
}

/// The agent's conversations, the latest first. None yet: an empty list.
pub fn list(places: &Places, agent: &str) -> Result<Vec<Conversation>> {
    if !supported(agent) {
        return Err(unsupported(agent));
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
    if !supported(agent) {
        return Err(unsupported(agent));
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
    if !supported(agent) {
        return Err(unsupported(agent));
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
    let folder = folder(places, agent, id)?;
    Ok((vec!["--resume".into(), id.into()], folder))
}

/// The folder where the conversation took place, if it still is there. None: it never said.
pub fn folder(places: &Places, agent: &str, id: &str) -> Result<Option<PathBuf>> {
    if !supported(agent) {
        return Err(unsupported(agent));
    }
    let path = claude_file(places, id)?;
    let folder = claude_conversation(&path)
        .and_then(|c| c.folder)
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
