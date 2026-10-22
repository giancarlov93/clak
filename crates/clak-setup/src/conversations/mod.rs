//! The past conversations of an agent, from the Agents tab: listed, searched in their text,
//! resumed in the folder where they took place, renamed, deleted, exported as Markdown.
//!
//! They are the agent's files, not Clak's, and each agent keeps them its own way (one
//! module each): Claude Code a JSONL file per conversation (`claude`), Claude Desktop a
//! file per Code session over Claude Code's (`desktop`), OpenCode a SQLite database
//! (`opencode`), Codex a JSONL file per session by date (`codex`), Gemini CLI a JSON file
//! per session by project (`gemini`). Clak writes only where the agent itself would, and
//! only for Claude Code: a rename is one more line, as its `/rename` adds. The others are
//! read, and their sessions resume through the agent.

mod claude;
mod codex;
mod desktop;
mod gemini;
mod opencode;

use crate::{Places, Result, SetupError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// How long a title can be, in characters: the first prompt is cut there.
const TITLE_LENGTH: usize = 120;

const CLAUDE_CODE: &str = "claude-code";
const CLAUDE_DESKTOP: &str = "claude-desktop";
const OPENCODE: &str = "opencode";
const CODEX: &str = "codex";
const GEMINI: &str = "gemini";

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
    /// What it takes on disk, in bytes, when Clak can delete it: 0 otherwise.
    pub size: u64,
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
            "size": self.size,
        })
    }
}

/// What Clak does with an agent's conversations, beyond listing and resuming them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Can {
    pub rename: bool,
    pub delete: bool,
    /// Find them by words in what was said, not only in the title.
    pub search: bool,
    pub export: bool,
    /// They open again in the app, not in a terminal.
    pub in_app: bool,
}

impl Can {
    pub fn to_json(&self) -> Value {
        json!({
            "rename": self.rename,
            "delete": self.delete,
            "search": self.search,
            "export": self.export,
            "in_app": self.in_app,
        })
    }
}

/// What Clak does with this agent's conversations (by its catalog id, see `launcher`), or
/// `None` if it doesn't read them.
pub fn can(agent: &str) -> Option<Can> {
    let reads = Can {
        search: true,
        export: true,
        ..Can::default()
    };
    match agent {
        CLAUDE_CODE => Some(Can {
            rename: true,
            delete: true,
            ..reads
        }),
        CLAUDE_DESKTOP => Some(Can {
            in_app: true,
            ..reads
        }),
        CODEX | GEMINI => Some(reads),
        OPENCODE => Some(Can::default()),
        _ => None,
    }
}

/// Whether Clak reads this agent's conversations.
pub fn supported(agent: &str) -> bool {
    can(agent).is_some()
}

/// The agent's conversations, the latest first. None yet: an empty list.
pub fn list(places: &Places, agent: &str) -> Result<Vec<Conversation>> {
    let mut all = match agent {
        CLAUDE_CODE => claude::list(places),
        CLAUDE_DESKTOP => desktop::list(places),
        OPENCODE => opencode::list(places)?,
        CODEX => codex::list(places),
        GEMINI => gemini::list(places),
        _ => return Err(unsupported(agent)),
    };
    all.sort_by_key(|c| std::cmp::Reverse(c.updated));
    Ok(all)
}

/// The ids of the conversations in which all these words were said, by the user or by the
/// agent: case doesn't count.
pub fn search(places: &Places, agent: &str, words: &[String]) -> Result<Vec<String>> {
    let words: Vec<String> = words
        .iter()
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    match agent {
        CLAUDE_CODE => Ok(claude::search(places, &words)),
        CLAUDE_DESKTOP => Ok(desktop::search(places, &words)),
        CODEX => Ok(codex::search(places, &words)),
        GEMINI => Ok(gemini::search(places, &words)),
        _ => Err(cannot(agent, "search")),
    }
}

/// Gives the conversation a title, as the agent itself does.
pub fn rename(places: &Places, agent: &str, id: &str, title: &str) -> Result<()> {
    if agent != CLAUDE_CODE {
        return Err(cannot(agent, "rename"));
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err(SetupError("The name is empty.".into()));
    }
    let title: String = title.chars().take(TITLE_LENGTH).collect();
    claude::rename(places, id, &title)
}

/// Deletes the conversation, with what the agent keeps beside it.
pub fn delete(places: &Places, agent: &str, id: &str) -> Result<()> {
    if agent != CLAUDE_CODE {
        return Err(cannot(agent, "delete"));
    }
    claude::delete(places, id)
}

/// Deletes several, one by one: those that can't be deleted, with the reason.
pub fn delete_many(places: &Places, agent: &str, ids: &[String]) -> Vec<(String, SetupError)> {
    ids.iter()
        .filter_map(|id| delete(places, agent, id).err().map(|e| (id.clone(), e)))
        .collect()
}

/// The conversation as Markdown, to keep or share: what the user asked and what the agent
/// answered, with the commands it used named. Its title comes along, for the file's name.
pub fn export(places: &Places, agent: &str, id: &str) -> Result<(String, String)> {
    let conversation = list(places, agent)?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(gone)?;
    let turns = match agent {
        CLAUDE_CODE => claude::turns(places, id)?,
        CLAUDE_DESKTOP => claude::turns(
            places,
            conversation
                .terminal
                .as_deref()
                .ok_or_else(|| SetupError("Claude Code no longer has this conversation.".into()))?,
        )?,
        CODEX => codex::turns(places, id)?,
        GEMINI => gemini::turns(places, id)?,
        _ => return Err(cannot(agent, "export")),
    };
    Ok((conversation.title.clone(), markdown(&conversation, &turns)))
}

/// What to add to the agent's command line to resume the conversation, and the folder to
/// start it in.
pub fn resume(places: &Places, agent: &str, id: &str) -> Result<(Vec<String>, Option<PathBuf>)> {
    // It goes on a command line: an id, nothing else.
    if !valid_id(id) {
        return Err(SetupError(format!("“{id}” isn't a conversation.")));
    }
    let args = match agent {
        CLAUDE_CODE => vec!["--resume".to_owned(), id.to_owned()],
        OPENCODE => vec!["--session".to_owned(), id.to_owned()],
        CODEX => vec!["resume".to_owned(), id.to_owned()],
        GEMINI => vec!["--resume".to_owned(), id.to_owned()],
        CLAUDE_DESKTOP => {
            return Err(SetupError(
                "Claude Desktop's sessions resume in the app.".into(),
            ));
        }
        _ => return Err(unsupported(agent)),
    };
    let folder = folder(places, agent, id)?;
    Ok((args, folder))
}

/// The link that opens one of Claude Desktop's sessions in the app: the one the app gives
/// its own recent sessions, in the Dock and in Spotlight.
pub fn link(places: &Places, agent: &str, id: &str) -> Result<String> {
    if agent != CLAUDE_DESKTOP {
        return Err(SetupError(format!(
            "The conversations of “{agent}” resume in a terminal."
        )));
    }
    let session = desktop::session(places, id)?;
    Ok(format!("claude://code/continue?session={}", session.id))
}

/// The folder where the conversation took place, if it still is there. None: it never said.
pub fn folder(places: &Places, agent: &str, id: &str) -> Result<Option<PathBuf>> {
    let folder = match agent {
        CLAUDE_CODE => claude::folder(places, id)?,
        CLAUDE_DESKTOP => desktop::session(places, id)?.folder,
        _ => {
            list(places, agent)?
                .into_iter()
                .find(|c| c.id == id)
                .ok_or_else(gone)?
                .folder
        }
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

/// One turn of a conversation, for the export.
#[derive(Debug, Clone, PartialEq)]
enum Turn {
    User(String),
    Agent(String),
    /// The agent used a command: its name.
    Tool(String),
}

fn markdown(conversation: &Conversation, turns: &[Turn]) -> String {
    let mut out = format!("# {}\n\n", conversation.title);
    if let Some(folder) = &conversation.folder {
        out.push_str(&format!("`{folder}`\n\n"));
    }
    let mut tools: Vec<String> = Vec::new();
    let flush = |out: &mut String, tools: &mut Vec<String>| {
        if !tools.is_empty() {
            out.push_str(&format!("*Used {}*\n\n", tools.join(", ")));
            tools.clear();
        }
    };
    for turn in turns {
        match turn {
            Turn::Tool(name) => {
                if tools.last() != Some(name) {
                    tools.push(name.clone());
                }
            }
            Turn::User(text) => {
                flush(&mut out, &mut tools);
                out.push_str(&format!("## You\n\n{}\n\n", text.trim()));
            }
            Turn::Agent(text) => {
                flush(&mut out, &mut tools);
                out.push_str(&format!("## Agent\n\n{}\n\n", text.trim()));
            }
        }
    }
    flush(&mut out, &mut tools);
    out.trim_end().to_owned() + "\n"
}

/// All the words are in the text: the text already in lower case.
fn has_all(text: &str, words: &[String]) -> bool {
    words.iter().all(|w| text.contains(w.as_str()))
}

fn unsupported(agent: &str) -> SetupError {
    SetupError(format!(
        "Clak doesn't read the conversations of “{agent}” yet."
    ))
}

fn cannot(agent: &str, what: &str) -> SetupError {
    match agent {
        CLAUDE_DESKTOP if what == "rename" || what == "delete" => {
            SetupError("Claude Desktop renames and deletes its sessions itself.".into())
        }
        _ if supported(agent) => {
            SetupError(format!("Clak can't {what} the conversations of “{agent}”."))
        }
        _ => unsupported(agent),
    }
}

fn gone() -> SetupError {
    SetupError("This conversation is no longer there.".into())
}

/// An id as the agents make them: letters, digits, `-` and `_`, not a path.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn text(entry: &Value, name: &str) -> Option<String> {
    let text = entry.get(name)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// What the user typed, not a reminder or instructions put in by the agent: those start
/// with a tag (`<environment_context>`, `<command-name>`) or say so.
fn typed(text: &str) -> Option<&str> {
    let text = text.trim();
    let injected = text.is_empty()
        || text.starts_with('<')
        || text.starts_with("Caveat:")
        || text.starts_with("# AGENTS.md instructions");
    (!injected).then_some(text)
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

/// When a file was last written, in milliseconds since 1970.
fn modified(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

/// What a file or a folder takes, all inside it.
fn disk_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| disk_size(&e.path()))
        .sum()
}
