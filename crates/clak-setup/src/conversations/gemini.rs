//! Gemini CLI keeps a file per session under its project,
//! `~/.gemini/tmp/<project>/chats/session-<time>-<id>.jsonl` (`$GEMINI_CLI_HOME/.gemini`
//! with the variable; `.json` in earlier versions, a single object). The first line has the
//! session's id; then one message per line (`user`, `gemini`), a message with an id already
//! seen taking its place, and `{"$set": {...}}` lines changing the first one: that is how the
//! title (`summary`) arrives. The project's folder is in `<project>/.project_root`. The
//! sessions of Gemini's subagents (`kind: "subagent"`, in folders under `chats`) are left
//! out. A session resumes with `gemini --resume <id>`, from its project's folder: Gemini
//! looks for it only there.

use super::{Conversation, Turn, has_all, modified, shorten, text, typed, valid_id};
use crate::{Places, Result, SetupError};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub(super) fn list(places: &Places) -> Vec<Conversation> {
    files(places)
        .iter()
        .filter_map(|(path, folder)| session(path, folder.clone()))
        .collect()
}

pub(super) fn search(places: &Places, words: &[String]) -> Vec<String> {
    files(places)
        .iter()
        .filter_map(|(path, folder)| {
            let read = read(path)?;
            let said = read
                .turns()
                .iter()
                .filter_map(|t| match t {
                    Turn::User(text) | Turn::Agent(text) => Some(text.to_lowercase()),
                    Turn::Tool(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            has_all(&said, words)
                .then(|| session(path, folder.clone()).map(|c| c.id))
                .flatten()
        })
        .collect()
}

pub(super) fn turns(places: &Places, id: &str) -> Result<Vec<Turn>> {
    if !valid_id(id) {
        return Err(SetupError(format!("“{id}” isn't a conversation.")));
    }
    files(places)
        .iter()
        .filter_map(|(path, _)| read(path))
        .find(|s| s.meta.get("sessionId").and_then(Value::as_str) == Some(id))
        .map(|s| s.turns())
        .ok_or_else(super::gone)
}

/// `$GEMINI_CLI_HOME/.gemini`, or `~/.gemini`.
fn home(places: &Places) -> PathBuf {
    match places.var("GEMINI_CLI_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => places.home.clone(),
    }
    .join(".gemini")
}

/// Every main session's file, with its project's folder.
fn files(places: &Places) -> Vec<(PathBuf, Option<String>)> {
    let read = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten();
    let mut all = Vec::new();
    for project in read(&home(places).join("tmp")) {
        let folder = std::fs::read_to_string(project.path().join(".project_root"))
            .ok()
            .map(|f| f.trim().to_owned())
            .filter(|f| !f.is_empty());
        for file in read(&project.path().join("chats")) {
            let name = file.file_name().to_string_lossy().into_owned();
            if name.starts_with("session-") && (name.ends_with(".jsonl") || name.ends_with(".json"))
            {
                all.push((file.path(), folder.clone()));
            }
        }
    }
    all
}

/// A session as read: its first line with the changes made to it, and its messages.
struct Read {
    meta: Map<String, Value>,
    messages: Vec<Value>,
}

impl Read {
    fn turns(&self) -> Vec<Turn> {
        let mut turns = Vec::new();
        for message in &self.messages {
            let said = content(message.get("content"));
            match message.get("type").and_then(Value::as_str) {
                Some("user") => {
                    if let Some(said) = said.as_deref().and_then(prompt) {
                        turns.push(Turn::User(said.to_owned()));
                    }
                }
                Some("gemini") => {
                    if let Some(said) = said.filter(|s| !s.trim().is_empty()) {
                        turns.push(Turn::Agent(said));
                    }
                    for call in message
                        .get("toolCalls")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(name) = text(call, "name") {
                            turns.push(Turn::Tool(name));
                        }
                    }
                }
                _ => {}
            }
        }
        turns
    }
}

fn read(path: &Path) -> Option<Read> {
    let bytes = std::fs::read(path).ok()?;
    if path.extension().is_some_and(|e| e == "json") {
        let mut whole: Map<String, Value> = serde_json::from_slice(&bytes).ok()?;
        let messages = match whole.remove("messages") {
            Some(Value::Array(messages)) => messages,
            _ => Vec::new(),
        };
        return Some(Read {
            meta: whole,
            messages,
        });
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok());
    let mut meta = match lines.next()? {
        Value::Object(meta) => meta,
        _ => return None,
    };
    let mut messages: Vec<Value> = Vec::new();
    for line in lines {
        if let Some(Value::Object(set)) = line.get("$set") {
            meta.extend(set.clone());
            continue;
        }
        if line.get("type").is_none() {
            continue;
        }
        let id = line.get("id").cloned();
        match messages
            .iter_mut()
            .find(|m| id.is_some() && m.get("id") == id.as_ref())
        {
            Some(earlier) => *earlier = line,
            None => messages.push(line),
        }
    }
    Some(Read { meta, messages })
}

fn session(path: &Path, folder: Option<String>) -> Option<Conversation> {
    let read = read(path)?;
    if read.meta.get("kind").and_then(Value::as_str) == Some("subagent") {
        return None;
    }
    let meta = Value::Object(read.meta.clone());
    let id = text(&meta, "sessionId")?;
    let first = read.turns().into_iter().find_map(|t| match t {
        Turn::User(text) => Some(text),
        _ => None,
    });
    // Never a word from the user: nothing to resume.
    let first = first?;
    let summary = text(&meta, "summary");
    Some(Conversation {
        id,
        named: false,
        title: shorten(&summary.unwrap_or(first)),
        folder,
        updated: modified(path),
        terminal: None,
        size: 0,
    })
}

/// A message's text: a string, or the texts of its parts.
fn content(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Value::Object(part) => part.get("text").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

/// What the user asked, not a command (`/stats`, `?`) or context put in by Gemini.
fn prompt(text: &str) -> Option<&str> {
    typed(text).filter(|t| !t.starts_with('/') && !t.starts_with('?'))
}
