//! Where each agent writes its tokens, and how.
//!
//! - Claude Code: `<config>/projects/**/*.jsonl` (`$CLAUDE_CONFIG_DIR` or `~/.claude`, the
//!   subagents' files included), on each `assistant` line: `message.usage` with the cache
//!   written for five minutes and for an hour, `message.model`, `message.id` and `requestId`
//!   (the same answer is written once per part, and again in a resumed conversation).
//! - Codex: `$CODEX_HOME/sessions/**/rollout-*.jsonl` and `archived_sessions` (`~/.codex`):
//!   `token_count` events with the session's running total, `turn_context` with the model.
//!   The input it counts includes the cached input.
//! - Gemini CLI: `~/.gemini/tmp/<project>/chats/**/session-*.jsonl` (or `.json`), on each
//!   `gemini` message: `tokens` (`input` with the cached part, `thoughts` and `tool` apart).
//! - OpenCode: `<data>/opencode/opencode.db`, the `assistant` rows of `session_message`, or
//!   before it `<data>/opencode/storage/message/<session>/<id>.json`: `tokens` without the
//!   cache, the reasoning apart, and the `cost` OpenCode worked out.

use super::{CLAUDE_CODE, CODEX, GEMINI, OPENCODE, Request, Stamp, Tokens};
use crate::Places;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A file to read the tokens from.
pub(super) struct Source {
    pub path: PathBuf,
    kind: Kind,
}

enum Kind {
    Claude,
    Codex,
    /// With its project's folder.
    Gemini(Option<String>),
    OpenCodeDatabase,
    OpenCodeFile,
}

impl Source {
    /// OpenCode's database changes in its `-wal` file first.
    pub fn stamp(&self) -> Stamp {
        let of = |path: &Path| {
            std::fs::metadata(path)
                .map(|m| (m.len(), m.modified().ok()))
                .unwrap_or((0, None))
        };
        let (size, written) = of(&self.path);
        if matches!(self.kind, Kind::OpenCodeDatabase) {
            let (wal, wal_written) = of(&crate::places::suffixed(&self.path, "-wal"));
            return (size + wal, written.max(wal_written));
        }
        (size, written)
    }

    pub fn read(&self) -> Vec<Request> {
        match &self.kind {
            Kind::Claude => claude(&self.path),
            Kind::Codex => codex(&self.path),
            Kind::Gemini(folder) => gemini(&self.path, folder.clone()),
            Kind::OpenCodeDatabase => opencode_database(&self.path),
            Kind::OpenCodeFile => opencode_file(&self.path).into_iter().collect(),
        }
    }
}

pub(super) fn sources(places: &Places) -> Vec<Source> {
    let mut all = Vec::new();
    let mut add = |kind: fn() -> Kind, files: Vec<PathBuf>| {
        all.extend(files.into_iter().map(|path| Source { path, kind: kind() }));
    };
    add(
        || Kind::Claude,
        under(&claude_config(places).join("projects"), &|name| {
            name.ends_with(".jsonl")
        }),
    );
    let codex = env_or(places, "CODEX_HOME", ".codex");
    let rollout = |name: &str| name.starts_with("rollout-") && name.ends_with(".jsonl");
    add(|| Kind::Codex, under(&codex.join("sessions"), &rollout));
    add(
        || Kind::Codex,
        under(&codex.join("archived_sessions"), &rollout),
    );
    let opencode = opencode_data(places);
    let database = opencode.join("opencode.db");
    if database.is_file() {
        add(|| Kind::OpenCodeDatabase, vec![database]);
    } else {
        let message = |name: &str| name.ends_with(".json");
        add(
            || Kind::OpenCodeFile,
            under(&opencode.join("storage").join("message"), &message),
        );
    }
    let gemini = env_or(places, "GEMINI_CLI_HOME", "")
        .join(".gemini")
        .join("tmp");
    for project in std::fs::read_dir(gemini).into_iter().flatten().flatten() {
        let folder = std::fs::read_to_string(project.path().join(".project_root"))
            .ok()
            .map(|f| f.trim().to_owned())
            .filter(|f| !f.is_empty());
        let session = |name: &str| {
            name.starts_with("session-") && (name.ends_with(".jsonl") || name.ends_with(".json"))
        };
        for path in under(&project.path().join("chats"), &session) {
            all.push(Source {
                path,
                kind: Kind::Gemini(folder.clone()),
            });
        }
    }
    all
}

/// `$CLAUDE_CONFIG_DIR`, or `~/.claude`.
fn claude_config(places: &Places) -> PathBuf {
    match places.var("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => places.home.join(".claude"),
    }
}

/// The folder in a variable, or this one in the home folder.
fn env_or(places: &Places, variable: &str, home: &str) -> PathBuf {
    match places.var(variable) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ if home.is_empty() => places.home.clone(),
        _ => places.home.join(home),
    }
}

fn opencode_data(places: &Places) -> PathBuf {
    match places.var("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => places.home.join(".local").join("share"),
    }
    .join("opencode")
}

/// The files under a folder, at any depth, whose name is wanted.
fn under(dir: &Path, wanted: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file() && wanted(&entry.file_name().to_string_lossy()) {
                found.push(entry.path());
            }
        }
    }
    found
}

/// The lines of a JSONL file that may have what is looked for, read.
fn lines(path: &Path, having: &str) -> Vec<Value> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| line.contains(having))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn number(value: &Value, name: &str) -> u64 {
    value.get(name).and_then(Value::as_u64).unwrap_or(0)
}

fn string(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The folder is where the conversation started: Claude Code's `cwd` follows the agent's
/// `cd` into the project's subfolders.
fn claude(path: &Path) -> Vec<Request> {
    let mut requests: Vec<Request> = lines(path, "\"usage\"")
        .iter()
        .filter(|line| line.get("type").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|line| {
            let message = line.get("message")?;
            let usage = message.get("usage")?;
            let model = string(message, "model").filter(|m| m != "<synthetic>")?;
            let (five_minutes, hour) = match usage.get("cache_creation") {
                Some(split) if split.is_object() => (
                    number(split, "ephemeral_5m_input_tokens"),
                    number(split, "ephemeral_1h_input_tokens"),
                ),
                _ => (number(usage, "cache_creation_input_tokens"), 0),
            };
            let tokens = Tokens {
                input: number(usage, "input_tokens"),
                output: number(usage, "output_tokens"),
                cache_write: five_minutes,
                cache_write_hour: hour,
                cache_read: number(usage, "cache_read_input_tokens"),
            };
            let key = match (string(message, "id"), string(line, "requestId")) {
                (Some(message), Some(request)) => Some(format!("claude:{message}:{request}")),
                _ => None,
            };
            Some(Request {
                agent: CLAUDE_CODE,
                at: string(line, "timestamp").as_deref().and_then(millis)?,
                model,
                provider: Some("anthropic".into()),
                folder: string(line, "cwd"),
                session: string(line, "sessionId").unwrap_or_else(|| stem(path)),
                tokens,
                recorded: line.get("costUSD").and_then(Value::as_f64),
                key,
            })
        })
        .collect();
    let started = requests.iter().find_map(|r| r.folder.clone());
    for request in &mut requests {
        request.folder.clone_from(&started);
    }
    requests
}

/// Codex writes the session's total after each answer, sometimes twice: what a request
/// used is what the total grew by.
fn codex(path: &Path) -> Vec<Request> {
    let mut session = stem(path);
    let mut folder = None;
    // Sessions written before Codex said which model: its default then.
    let mut model = "gpt-5".to_owned();
    let mut total = Tokens::default();
    let mut requests = Vec::new();
    for line in lines(path, "\"payload\"") {
        let Some(payload) = line.get("payload") else {
            continue;
        };
        match line.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                session = string(payload, "id").unwrap_or(session);
                folder = string(payload, "cwd").or(folder);
            }
            Some("turn_context") => {
                model = string(payload, "model").unwrap_or(model);
                folder = string(payload, "cwd").or(folder);
            }
            Some("event_msg")
                if payload.get("type").and_then(Value::as_str) == Some("token_count") =>
            {
                let Some(info) = payload.get("info").filter(|i| i.is_object()) else {
                    continue;
                };
                let read = |usage: &Value| {
                    let cached = number(usage, "cached_input_tokens");
                    Tokens {
                        input: number(usage, "input_tokens").saturating_sub(cached),
                        output: number(usage, "output_tokens"),
                        cache_read: cached,
                        ..Tokens::default()
                    }
                };
                let used = match info.get("total_token_usage") {
                    Some(now) if now.is_object() => {
                        let now = read(now);
                        let grown = Tokens {
                            input: now.input.saturating_sub(total.input),
                            output: now.output.saturating_sub(total.output),
                            cache_read: now.cache_read.saturating_sub(total.cache_read),
                            ..Tokens::default()
                        };
                        // A total that went down: a new count started.
                        let restarted = now.total() < total.total();
                        total = now;
                        if restarted { now } else { grown }
                    }
                    _ => match info.get("last_token_usage") {
                        Some(last) if last.is_object() => read(last),
                        _ => continue,
                    },
                };
                if used.total() == 0 {
                    continue;
                }
                let Some(at) = string(&line, "timestamp").as_deref().and_then(millis) else {
                    continue;
                };
                requests.push(Request {
                    agent: CODEX,
                    at,
                    model: model.clone(),
                    provider: Some("openai".into()),
                    folder: folder.clone(),
                    session: session.clone(),
                    tokens: used,
                    recorded: None,
                    key: None,
                });
            }
            _ => {}
        }
    }
    // The session's id and folder come first, but a file cut short may lack them.
    for request in &mut requests {
        request.session.clone_from(&session);
        request.folder.clone_from(&folder);
    }
    requests
}

fn gemini(path: &Path, folder: Option<String>) -> Vec<Request> {
    let (session, messages) = if path.extension().is_some_and(|e| e == "json") {
        let Some(whole) = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            return Vec::new();
        };
        let messages = match whole.get("messages") {
            Some(Value::Array(messages)) => messages.clone(),
            _ => Vec::new(),
        };
        (string(&whole, "sessionId"), messages)
    } else {
        let Ok(bytes) = std::fs::read(path) else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut lines = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok());
        let session = lines.next().and_then(|first| string(&first, "sessionId"));
        // A message with an id already seen takes its place.
        let mut messages: Vec<Value> = Vec::new();
        let mut at: HashMap<String, usize> = HashMap::new();
        for line in lines.filter(|l| l.get("tokens").is_some()) {
            let id = string(&line, "id");
            match id.as_ref().and_then(|id| at.get(id)) {
                Some(&index) => messages[index] = line,
                None => {
                    if let Some(id) = id {
                        at.insert(id, messages.len());
                    }
                    messages.push(line);
                }
            }
        }
        (session, messages)
    };
    let session = session.unwrap_or_else(|| stem(path));
    messages
        .iter()
        .filter(|m| m.get("type").and_then(Value::as_str) == Some("gemini"))
        .filter_map(|message| {
            let tokens = message.get("tokens")?;
            let cached = number(tokens, "cached");
            let tokens = Tokens {
                input: number(tokens, "input").saturating_sub(cached) + number(tokens, "tool"),
                output: number(tokens, "output") + number(tokens, "thoughts"),
                cache_read: cached,
                ..Tokens::default()
            };
            (tokens.total() > 0).then_some(())?;
            Some(Request {
                agent: GEMINI,
                at: string(message, "timestamp").as_deref().and_then(millis)?,
                model: string(message, "model").unwrap_or_else(|| "gemini-2.5-pro".into()),
                provider: Some("gemini".into()),
                folder: folder.clone(),
                session: session.clone(),
                tokens,
                recorded: None,
                key: None,
            })
        })
        .collect()
}

/// An OpenCode answer: `tokens`, `cost`, the model and when.
fn opencode_answer(
    data: &Value,
    model: Option<String>,
    provider: Option<String>,
    session: String,
    folder: Option<String>,
) -> Option<Request> {
    let tokens = data.get("tokens")?;
    let cache = tokens.get("cache");
    let tokens = Tokens {
        input: number(tokens, "input"),
        output: number(tokens, "output") + number(tokens, "reasoning"),
        cache_write: cache.map_or(0, |c| number(c, "write")),
        cache_read: cache.map_or(0, |c| number(c, "read")),
        ..Tokens::default()
    };
    (tokens.total() > 0).then_some(())?;
    Some(Request {
        agent: OPENCODE,
        at: data.get("time")?.get("created")?.as_i64()?,
        model: model?,
        provider,
        folder,
        session,
        tokens,
        recorded: data.get("cost").and_then(Value::as_f64),
        key: None,
    })
}

fn opencode_database(path: &Path) -> Vec<Request> {
    let read = || -> rusqlite::Result<Vec<Request>> {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::from_secs(2))?;
        let mut query = db.prepare(
            "SELECT m.session_id, s.directory, m.data FROM session_message m \
             LEFT JOIN session_v2 s ON s.id = m.session_id WHERE m.type = 'assistant'",
        )?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        Ok(rows
            .flatten()
            .filter_map(|(session, folder, data)| {
                let data: Value = serde_json::from_str(&data).ok()?;
                let model = data.get("model");
                opencode_answer(
                    &data,
                    model.and_then(|m| string(m, "id")),
                    model.and_then(|m| string(m, "providerID")),
                    session,
                    folder.filter(|f| !f.is_empty()),
                )
            })
            .collect())
    };
    read().unwrap_or_default()
}

fn opencode_file(path: &Path) -> Option<Request> {
    let data: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (data.get("role").and_then(Value::as_str) == Some("assistant")).then_some(())?;
    opencode_answer(
        &data,
        string(&data, "modelID"),
        string(&data, "providerID"),
        string(&data, "sessionID")?,
        data.get("path").and_then(|p| string(p, "cwd")),
    )
}

/// An ISO 8601 time in milliseconds since 1970: `2026-10-21T13:59:13.777Z`, with `+02:00`
/// in place of `Z`.
pub(crate) fn millis(text: &str) -> Option<i64> {
    let digits = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
    };
    let (date, rest) = text.split_once(['T', ' '])?;
    let mut parts = date.splitn(3, '-');
    let (year, month, day) = (
        digits(parts.next()?)?,
        digits(parts.next()?)?,
        digits(parts.next()?)?,
    );
    let zone_at = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (clock, zone) = rest.split_at(zone_at);
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut parts = clock.splitn(3, ':');
    let (hour, minute) = (digits(parts.next()?)?, digits(parts.next()?)?);
    let second = parts.next().map_or(Some(0), digits)?;
    let fraction = format!("{fraction:0<3}");
    let milli = if fraction.len() >= 3 {
        digits(&fraction[..3])?
    } else {
        0
    };
    let offset = match zone.as_bytes().first() {
        Some(b'+' | b'-') => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let (h, m) = zone[1..]
                .split_once(':')
                .unwrap_or((&zone[1..zone.len().min(3)], &zone[zone.len().min(3)..]));
            sign * (digits(h)? * 60 + if m.is_empty() { 0 } else { digits(m)? })
        }
        _ => 0,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from 1970-01-01, by Howard Hinnant's algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hour) * 60 + minute - offset) * 60_000 + second * 1000 + milli)
}

#[cfg(test)]
mod tests {
    use super::millis;

    #[test]
    fn times_are_read_in_utc() {
        assert_eq!(millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(millis("2026-10-21T13:59:13.777Z"), Some(1_792_591_153_777));
        assert_eq!(
            millis("2026-10-21T15:59:13.777+02:00"),
            Some(1_792_591_153_777)
        );
        assert_eq!(millis("2026-10-21T13:59:13.7Z"), Some(1_792_591_153_700));
        assert_eq!(millis("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        assert_eq!(millis("not a time"), None);
    }
}
