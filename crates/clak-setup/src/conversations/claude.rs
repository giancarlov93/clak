//! Claude Code: one file per conversation, `<config>/projects/<folder>/<id>.jsonl`, one JSON
//! object per line, to which it only ever adds lines. Renaming adds one more line, as its
//! own `/rename` does. Deleting removes that file and the folders named after the
//! conversation, as nothing in Claude Code does.

use super::{
    Conversation, Turn, disk_size, gone, has_all, modified, shorten, text, typed, valid_id,
};
use crate::{Places, Result, SetupError};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(super) fn list(places: &Places) -> Vec<Conversation> {
    files(places)
        .iter()
        .filter_map(|path| conversation(path))
        .map(|mut c| {
            c.size = size(places, &c.id);
            c
        })
        .collect()
}

pub(super) fn rename(places: &Places, id: &str, title: &str) -> Result<()> {
    let path = file(places, id)?;
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

/// The file, and what Claude Code keeps beside it under its id (the subagents and the long
/// tool results, the files' earlier versions, the environment). The prompts in the shared
/// `history.jsonl` stay: that file is every conversation's.
pub(super) fn delete(places: &Places, id: &str) -> Result<()> {
    let path = file(places, id)?;
    std::fs::remove_file(&path)
        .map_err(|e| SetupError(format!("Can't delete {}: {e}", path.display())))?;
    for dir in beside(places, &path, id) {
        if dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    Ok(())
}

pub(super) fn folder(places: &Places, id: &str) -> Result<Option<String>> {
    Ok(conversation(&file(places, id)?).and_then(|c| c.folder))
}

/// The conversations in which all the words were said. The whole file is looked at first,
/// cheaply: only one that has them all is read message by message.
pub(super) fn search(places: &Places, words: &[String]) -> Vec<String> {
    files(places)
        .iter()
        .filter(|path| {
            std::fs::read(path)
                .map(|bytes| has_all(&String::from_utf8_lossy(&bytes).to_lowercase(), words))
                .unwrap_or(false)
        })
        .filter(|path| {
            let said = read_turns(path)
                .iter()
                .filter_map(|t| match t {
                    Turn::User(text) | Turn::Agent(text) => Some(text.to_lowercase()),
                    Turn::Tool(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            has_all(&said, words)
        })
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
        .collect()
}

/// What was said, in order: the user's prompts, the agent's answers, the commands it used.
pub(super) fn turns(places: &Places, id: &str) -> Result<Vec<Turn>> {
    Ok(read_turns(&file(places, id)?))
}

fn read_turns(path: &Path) -> Vec<Turn> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut turns = Vec::new();
    for line in BufReader::new(file)
        .split(b'\n')
        .map_while(std::result::Result::ok)
    {
        let has = |needle: &[u8]| line.windows(needle.len()).any(|w| w == needle);
        let user = has(b"\"type\":\"user\"");
        if !user && !has(b"\"type\":\"assistant\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let flag = |name: &str| entry.get(name).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain") || flag("isMeta") {
            continue;
        }
        let Some(content) = entry.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let blocks = match content {
            Value::String(text) => vec![json!({ "type": "text", "text": text })],
            Value::Array(blocks) => blocks.clone(),
            _ => continue,
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let Some(said) = block.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    if user {
                        if let Some(said) = typed(said) {
                            turns.push(Turn::User(said.to_owned()));
                        }
                    } else if !said.trim().is_empty() {
                        turns.push(Turn::Agent(said.to_owned()));
                    }
                }
                Some("tool_use") if !user => {
                    if let Some(name) = block.get("name").and_then(Value::as_str) {
                        turns.push(Turn::Tool(name.to_owned()));
                    }
                }
                _ => {}
            }
        }
    }
    turns
}

/// `$CLAUDE_CONFIG_DIR`, or `~/.claude`.
fn config(places: &Places) -> PathBuf {
    match places.var("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => places.home.join(".claude"),
    }
}

fn projects(places: &Places) -> PathBuf {
    config(places).join("projects")
}

/// Every conversation's file, in every folder.
fn files(places: &Places) -> Vec<PathBuf> {
    let read = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten();
    read(&projects(places))
        .flat_map(|project| read(&project.path()))
        .map(|file| file.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

/// The file of a conversation, by its id: found in whichever folder it took place.
pub(super) fn file(places: &Places, id: &str) -> Result<PathBuf> {
    if !valid_id(id) || id.len() > 64 {
        return Err(SetupError(format!("“{id}” isn't a conversation.")));
    }
    let name = format!("{id}.jsonl");
    std::fs::read_dir(projects(places))
        .into_iter()
        .flatten()
        .flatten()
        .map(|project| project.path().join(&name))
        .find(|path| path.is_file())
        .ok_or_else(gone)
}

/// The folders Claude Code keeps under a conversation's id.
fn beside(places: &Places, path: &Path, id: &str) -> [PathBuf; 3] {
    let config = config(places);
    [
        path.with_extension(""),
        config.join("file-history").join(id),
        config.join("session-env").join(id),
    ]
}

/// What deleting it would free.
fn size(places: &Places, id: &str) -> u64 {
    let Ok(path) = file(places, id) else {
        return 0;
    };
    disk_size(&path)
        + beside(places, &path, id)
            .iter()
            .map(|dir| disk_size(dir))
            .sum::<u64>()
}

/// A conversation, from its file: its last title, the folder of its first message, its first
/// prompt. Only the lines that can say so are parsed: a file can be tens of megabytes.
pub(super) fn conversation(path: &Path) -> Option<Conversation> {
    let id = path.file_stem()?.to_str()?.to_owned();
    let updated = modified(path);
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
        size: 0,
    })
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
    texts.into_iter().find_map(typed).map(str::to_owned)
}
