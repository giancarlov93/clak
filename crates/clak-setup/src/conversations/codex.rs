//! Codex keeps a file per session, `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`
//! (`~/.codex` without the variable), one JSON object per line: `{"type", "payload"}`. The
//! first, `session_meta`, has the id and the folder; what the user typed comes as
//! `event_msg` lines of type `user_message`, while the instructions Codex puts in
//! (AGENTS.md, the environment) are `response_item` lines and don't count. A name given to a
//! session is in `session_index.jsonl`, the last line for an id wins. Archived sessions,
//! moved to `archived_sessions`, and compressed ones (`.jsonl.zst`) are left out, and so are
//! those started by another program (`codex exec`, a subagent): `codex resume` lists only
//! the interactive ones. A session resumes with `codex resume <id>` in its folder.

use super::{Conversation, Turn, has_all, modified, shorten, text, typed, valid_id};
use crate::{Places, Result, SetupError};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub(super) fn list(places: &Places) -> Vec<Conversation> {
    let names = names(places);
    files(places)
        .iter()
        .filter_map(|path| session(path, &names))
        .collect()
}

pub(super) fn search(places: &Places, words: &[String]) -> Vec<String> {
    let names = names(places);
    files(places)
        .iter()
        .filter(|path| {
            std::fs::read(path)
                .map(|bytes| has_all(&String::from_utf8_lossy(&bytes).to_lowercase(), words))
                .unwrap_or(false)
        })
        .filter(|path| has_all(&said(&read_turns(path)), words))
        .filter_map(|path| session(path, &names).map(|c| c.id))
        .collect()
}

pub(super) fn turns(places: &Places, id: &str) -> Result<Vec<Turn>> {
    Ok(read_turns(&file(places, id)?))
}

/// `$CODEX_HOME`, or `~/.codex`.
fn home(places: &Places) -> PathBuf {
    match places.var("CODEX_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => places.home.join(".codex"),
    }
}

/// Every session's file, under the year, the month and the day.
fn files(places: &Places) -> Vec<PathBuf> {
    let read = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten();
    let mut all = Vec::new();
    for year in read(&home(places).join("sessions")) {
        for month in read(&year.path()) {
            for day in read(&month.path()) {
                for file in read(&day.path()) {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                        all.push(file.path());
                    }
                }
            }
        }
    }
    all
}

fn file(places: &Places, id: &str) -> Result<PathBuf> {
    if !valid_id(id) {
        return Err(SetupError(format!("“{id}” isn't a conversation.")));
    }
    files(places)
        .into_iter()
        .find(|path| first(path).is_some_and(|meta| text(&meta, "id").as_deref() == Some(id)))
        .ok_or_else(super::gone)
}

/// The names given to the sessions: the last one for each id.
fn names(places: &Places) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Ok(file) = File::open(home(places).join("session_index.jsonl")) else {
        return names;
    };
    for line in BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
    {
        if let Ok(entry) = serde_json::from_str::<Value>(&line)
            && let (Some(id), Some(name)) = (text(&entry, "id"), text(&entry, "thread_name"))
        {
            names.insert(id, name);
        }
    }
    names
}

/// The payload of the first line, `session_meta`.
fn first(path: &Path) -> Option<Value> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut line)
        .ok()?;
    let entry: Value = serde_json::from_str(&line).ok()?;
    (entry.get("type").and_then(Value::as_str) == Some("session_meta"))
        .then(|| entry.get("payload").cloned())
        .flatten()
}

fn session(path: &Path, names: &HashMap<String, String>) -> Option<Conversation> {
    let meta = first(path)?;
    let id = text(&meta, "id")?;
    // Started by a program or by another session: not one to resume by hand.
    let interactive = matches!(
        meta.get("source").and_then(Value::as_str),
        None | Some("cli") | Some("vscode")
    );
    if !interactive || meta.get("parent_thread_id").is_some_and(|p| !p.is_null()) {
        return None;
    }
    let (title, named) = match names.get(&id) {
        Some(name) => (name.clone(), true),
        None => (prompt(path)?, false),
    };
    Some(Conversation {
        id,
        title: shorten(&title),
        named,
        folder: text(&meta, "cwd"),
        updated: modified(path),
        terminal: None,
        size: 0,
    })
}

/// The first thing the user typed. Never a word from the user: nothing to resume.
fn prompt(path: &Path) -> Option<String> {
    read_turns(path).into_iter().find_map(|t| match t {
        Turn::User(text) => Some(text),
        _ => None,
    })
}

fn said(turns: &[Turn]) -> String {
    turns
        .iter()
        .filter_map(|t| match t {
            Turn::User(text) | Turn::Agent(text) => Some(text.to_lowercase()),
            Turn::Tool(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What was said: the user's messages, the agent's answers, the commands it ran.
fn read_turns(path: &Path) -> Vec<Turn> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut turns = Vec::new();
    for line in BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
    {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(payload) = entry.get("payload") else {
            continue;
        };
        let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
        match entry.get("type").and_then(Value::as_str) {
            Some("event_msg") if kind == "user_message" => {
                if let Some(message) = payload.get("message").and_then(Value::as_str) {
                    let message = message
                        .rsplit_once("## My request for Codex:")
                        .map_or(message, |(_, request)| request);
                    if let Some(message) = typed(message) {
                        turns.push(Turn::User(message.to_owned()));
                    }
                }
            }
            Some("response_item") if kind == "message" => {
                if payload.get("role").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                let answer: Vec<&str> = payload
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("output_text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect();
                let answer = answer.join("\n");
                if !answer.trim().is_empty() {
                    turns.push(Turn::Agent(answer));
                }
            }
            Some("response_item") if kind.ends_with("call") => {
                let name = text(payload, "name").unwrap_or_else(|| "shell".to_owned());
                turns.push(Turn::Tool(name));
            }
            _ => {}
        }
    }
    turns
}
