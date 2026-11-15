//! The programs' commands as Clak knows them, and what the user decided about each: kept in
//! `commands.json` in the hub's folder, across connections and restarts.
//!
//! A command is approved as it was: its name, description and schema (the digest covers all
//! three, see `wire::ToolDef`). One that changes afterwards, even by a comma, is no longer
//! the approved one: it asks the user again, who sees what changed. The approval is the
//! command's, for every agent: which agent may use which program is another question, asked
//! on its own (`requests`).
//!
//! A program is kept by what identifies it across connections: a known application by its
//! key in `apps.json` (all its instances), an MCP server or a REST API by its id. A program
//! nobody could identify is kept only while it is connected; Clak's own settings aren't kept
//! at all, they are Clak's own code.

use crate::consent::ConsentLine;
use crate::texts::clean;
use crate::wire::ToolDef;
use clak_wire::json::{self as wire, float, now};
use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use std::path::PathBuf;

/// What the user decided about a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// It runs without asking, as long as it stays the approved version.
    Approved,
    /// It asks at every call: new, never approved, or the user's choice.
    Ask,
    /// Agents can't run it, nor see it.
    Off,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Approved => "approved",
            State::Ask => "ask",
            State::Off => "off",
        }
    }

    pub fn parse(text: &str) -> Option<State> {
        match text {
            "approved" => Some(State::Approved),
            "ask" => Some(State::Ask),
            "off" => Some(State::Off),
            _ => None,
        }
    }
}

/// A command as the user sees it on a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Approved, and still as approved: it runs.
    Approved,
    /// Approved, but not as it is now: it asks, showing what changed.
    Changed,
    /// Never approved: it asks.
    New,
    /// Approved once, but the user wants it to ask every time.
    Ask,
    /// Turned off: refused, and hidden from agents.
    Off,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Approved => "approved",
            Status::Changed => "changed",
            Status::New => "new",
            Status::Ask => "ask",
            Status::Off => "off",
        }
    }
}

/// One version of a command: its definition as declared, and when (and how) it was taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub digest: String,
    /// `{name, description, input_schema}`: what changed is shown from it.
    pub definition: Value,
    pub at: f64,
    /// Approved how: `question`, `panel`, `connect`, `restored`; for a version seen: `seen`.
    pub how: String,
}

impl Version {
    fn of(tool: &ToolDef, how: &str) -> Version {
        Version {
            digest: tool.digest.clone(),
            definition: tool.to_json(),
            at: now(),
            how: how.to_owned(),
        }
    }

    fn to_json(&self) -> Value {
        json!({ "digest": self.digest, "definition": self.definition, "at": float(self.at), "how": self.how })
    }

    fn from_json(value: &Value) -> Option<Version> {
        let definition = value.get("definition")?.clone();
        // The digest is the definition's: recomputed, never trusted from the file.
        let tool = ToolDef::from_json(&definition).ok()?;
        Some(Version {
            digest: tool.digest,
            definition,
            at: value.get("at").and_then(Value::as_f64).unwrap_or(0.0),
            how: value
                .get("how")
                .and_then(Value::as_str)
                .unwrap_or("question")
                .to_owned(),
        })
    }
}

/// A command Clak has seen, and what the user decided about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub state: State,
    /// The version the user approved, if any.
    pub approved: Option<Version>,
    /// The latest version declared, when it isn't the approved one.
    pub seen: Option<Version>,
    pub first_seen: f64,
    pub last_seen: f64,
    /// No longer offered since then; it comes back as it was if it returns the same.
    pub missing_since: Option<f64>,
}

impl Command {
    /// The latest version known: the one seen, or else the approved one.
    pub fn latest(&self) -> Option<&Version> {
        self.seen.as_ref().or(self.approved.as_ref())
    }

    fn status_of(&self, digest: &str) -> Status {
        match (self.state, &self.approved) {
            (State::Off, _) => Status::Off,
            (State::Approved, Some(v)) if v.digest == digest => Status::Approved,
            (State::Approved, Some(_)) => Status::Changed,
            (_, None) => Status::New,
            (State::Ask, Some(_)) => Status::Ask,
        }
    }

    /// What the user sees in the panel: as for a call, with the latest version known.
    fn shown_status(&self) -> Status {
        match self.latest() {
            Some(v) => self.status_of(&v.digest.clone()),
            None => Status::New,
        }
    }
}

/// A program's commands.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub key: String,
    /// `app`, `mcp`, `api`, or `conn` (identified by nothing: kept only while connected).
    pub kind: String,
    /// What the user calls it.
    pub label: String,
    /// For a server or an API, where it is: if that changes, it is another one, and its
    /// approvals go.
    pub origin: Option<String>,
    pub first_seen: f64,
    pub last_seen: f64,
    pub commands: IndexMap<String, Command>,
}

impl Program {
    /// How many commands need the user: new or changed ones.
    pub fn to_review(&self) -> usize {
        self.commands
            .values()
            .filter(|c| {
                c.missing_since.is_none()
                    && matches!(c.shown_status(), Status::New | Status::Changed)
            })
            .count()
    }

    fn approved(&self) -> usize {
        self.commands
            .values()
            .filter(|c| c.missing_since.is_none() && c.shown_status() == Status::Approved)
            .count()
    }
}

/// What a program brought that the user hasn't decided about.
#[derive(Debug, Default, PartialEq)]
pub struct Observed {
    pub new: Vec<String>,
    pub changed: Vec<String>,
}

/// How long `last_seen` may lag: a server's list is read again every ten minutes, and the
/// file isn't written for that.
const SEEN_EVERY: f64 = 86_400.0;

/// A command never approved and no longer offered for this long is forgotten: a program
/// that names its commands after what is open doesn't fill the file.
const FORGET_AFTER: f64 = 30.0 * 86_400.0;

/// The most commands kept per program; past it the new ones aren't kept (they ask, as new).
const MAX_COMMANDS: usize = 1_000;

#[derive(Debug, Default)]
pub struct CommandCache {
    /// Where they are kept; `None`: only in memory.
    path: Option<PathBuf>,
    programs: IndexMap<String, Program>,
    /// Why the file couldn't be written the last time, if it couldn't: what the user
    /// decided would be lost at the next start.
    error: Option<String>,
}

impl CommandCache {
    /// What `commands.json` has; nothing, or a damaged file, is a cache to fill again: the
    /// commands ask once more.
    pub fn load(path: PathBuf) -> CommandCache {
        let programs = std::fs::read(&path)
            .ok()
            .and_then(|b| wire::parse(&b).ok())
            .and_then(|v| v.get("programs").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(program_from_json)
            .map(|p| (p.key.clone(), p))
            .collect();
        CommandCache {
            path: Some(path),
            programs,
            error: None,
        }
    }

    /// Why `commands.json` couldn't be written the last time, if it couldn't.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn get(&self, key: &str) -> Option<&Program> {
        self.programs.get(key)
    }

    pub fn all(&self) -> impl Iterator<Item = &Program> {
        self.programs.values()
    }

    /// The command as the user decided, for its version now.
    pub fn status(&self, key: &str, tool: &ToolDef) -> Status {
        self.programs
            .get(key)
            .and_then(|p| p.commands.get(&tool.name))
            .map_or(Status::New, |c| c.status_of(&tool.digest))
    }

    /// The version the user approved, to show what changed.
    pub fn approved(&self, key: &str, name: &str) -> Option<&Version> {
        self.programs
            .get(key)?
            .commands
            .get(name)?
            .approved
            .as_ref()
    }

    /// A program connected or offered its commands again: they are taken in. A new one is
    /// to be decided; one whose version isn't the approved one keeps the approval, and is
    /// shown as changed; one no longer offered is marked missing. Returns what needs the
    /// user, if it is news.
    pub fn observe(
        &mut self,
        key: &str,
        kind: &str,
        label: &str,
        origin: Option<&str>,
        tools: &[ToolDef],
        elsewhere: &[String],
    ) -> Observed {
        let at = now();
        let mut changed_file = false;
        let program = self.programs.entry(key.to_owned()).or_insert_with(|| {
            changed_file = true;
            Program {
                key: key.to_owned(),
                kind: kind.to_owned(),
                label: label.to_owned(),
                origin: origin.map(str::to_owned),
                first_seen: at,
                last_seen: at,
                commands: IndexMap::new(),
            }
        });
        if program.label != label {
            program.label = label.to_owned();
            changed_file = true;
        }
        // Another server or API behind the same id: what was approved was the other one's.
        if origin.is_some() && program.origin.as_deref() != origin {
            if program.origin.is_some() {
                for command in program.commands.values_mut() {
                    if command.approved.is_some() {
                        command.seen = command.seen.take().or(command.approved.take());
                    }
                    if command.state == State::Approved {
                        command.state = State::Ask;
                    }
                }
            }
            program.origin = origin.map(str::to_owned);
            changed_file = true;
        }
        if at - program.last_seen > SEEN_EVERY {
            program.last_seen = at;
            changed_file = true;
        }
        // Never approved, and gone for long: forgotten.
        let before = program.commands.len();
        program.commands.retain(|_, c| {
            c.approved.is_some()
                || c.state == State::Off
                || c.missing_since
                    .is_none_or(|since| at - since < FORGET_AFTER)
        });
        changed_file |= program.commands.len() != before;
        let mut observed = Observed::default();
        for tool in tools {
            let full = program.commands.len() >= MAX_COMMANDS;
            match program.commands.get_mut(&tool.name) {
                None if full => {}
                None => {
                    program.commands.insert(
                        tool.name.clone(),
                        Command {
                            state: State::Ask,
                            approved: None,
                            seen: Some(Version::of(tool, "seen")),
                            first_seen: at,
                            last_seen: at,
                            missing_since: None,
                        },
                    );
                    observed.new.push(tool.name.clone());
                    changed_file = true;
                }
                Some(command) => {
                    if command.missing_since.take().is_some() {
                        changed_file = true;
                    }
                    let approved = command.approved.as_ref().map(|v| v.digest.as_str());
                    let seen = command.seen.as_ref().map(|v| v.digest.as_str());
                    if approved == Some(tool.digest.as_str()) {
                        if command.seen.take().is_some() {
                            changed_file = true;
                        }
                    } else if seen != Some(tool.digest.as_str()) {
                        command.seen = Some(Version::of(tool, "seen"));
                        if command.approved.is_some() {
                            observed.changed.push(tool.name.clone());
                        } else {
                            observed.new.push(tool.name.clone());
                        }
                        changed_file = true;
                    }
                    if at - command.last_seen > SEEN_EVERY {
                        command.last_seen = at;
                        changed_file = true;
                    }
                }
            }
        }
        // Missing only if no instance of the program offers it: another one may.
        for (name, command) in program.commands.iter_mut() {
            if command.missing_since.is_none()
                && !tools.iter().any(|t| &t.name == name)
                && !elsewhere.contains(name)
            {
                command.missing_since = Some(at);
                changed_file = true;
            }
        }
        if changed_file {
            self.save();
        }
        observed
    }

    /// The user approves the command as it is (`tool`): it runs from now on, for every agent
    /// that may use the program.
    pub fn approve(&mut self, key: &str, tool: &ToolDef, how: &str) {
        let Some(command) = self
            .programs
            .get_mut(key)
            .and_then(|p| p.commands.get_mut(&tool.name))
        else {
            return;
        };
        command.state = State::Approved;
        command.approved = Some(Version::of(tool, how));
        command.seen = None;
        self.save();
    }

    /// The user sets a command's state from the panel. Approving takes the version the
    /// user saw (`digest`): if the program changed it since, nothing is approved. False if
    /// nothing changed.
    pub fn set_state(&mut self, key: &str, name: &str, state: State, digest: Option<&str>) -> bool {
        let Some(command) = self
            .programs
            .get_mut(key)
            .and_then(|p| p.commands.get_mut(name))
        else {
            return false;
        };
        if state == State::Approved {
            let shown = command.latest().map(|v| v.digest.clone());
            if digest.is_none() || shown.as_deref() != digest {
                return false;
            }
            if let Some(mut latest) = command.seen.take() {
                latest.how = "panel".into();
                latest.at = now();
                command.approved = Some(latest);
            }
            if command.approved.is_none() {
                return false;
            }
        }
        command.state = state;
        self.save();
        true
    }

    /// The commands the user saw (name and digest; all those offered now, if `shown` is
    /// `None`), approved as they were: what the panel's "Approve all" does. One that changed
    /// since stays as it is; the turned off ones stay off.
    pub fn approve_all(
        &mut self,
        key: &str,
        how: &str,
        shown: Option<&[(String, String)]>,
    ) -> usize {
        let Some(program) = self.programs.get_mut(key) else {
            return 0;
        };
        let mut count = 0;
        for (name, command) in program.commands.iter_mut() {
            if command.missing_since.is_some() || command.state == State::Off {
                continue;
            }
            // Only what needs the user: one set to ask every time stays so.
            if !matches!(command.shown_status(), Status::New | Status::Changed) {
                continue;
            }
            if let Some(shown) = shown {
                let latest = command.latest().map(|v| v.digest.as_str());
                if !shown
                    .iter()
                    .any(|(n, d)| n == name && Some(d.as_str()) == latest)
                {
                    continue;
                }
            }
            if let Some(mut latest) = command.seen.take() {
                latest.how = how.to_owned();
                latest.at = now();
                command.approved = Some(latest);
            }
            if command.approved.is_some() && command.state != State::Approved {
                command.state = State::Approved;
                count += 1;
            } else if command.approved.is_some() {
                count += 1;
            }
        }
        self.save();
        count
    }

    /// All the program's approvals go: every command asks at each call, as the user chose,
    /// not as new (the versions approved stay, to show what changes next). The ones turned
    /// off stay off.
    pub fn reset(&mut self, key: &str) {
        let Some(program) = self.programs.get_mut(key) else {
            return;
        };
        for command in program.commands.values_mut() {
            if command.state != State::Off {
                command.state = State::Ask;
            }
        }
        self.save();
    }

    /// The commands the program no longer offers are forgotten; with `keep_off`, not the
    /// ones turned off (forgotten, one would come back asking).
    pub fn forget_missing(&mut self, key: &str, keep_off: bool) {
        if let Some(program) = self.programs.get_mut(key) {
            program
                .commands
                .retain(|_, c| c.missing_since.is_none() || (keep_off && c.state == State::Off));
            self.save();
        }
    }

    /// The program is forgotten, with what the user decided.
    pub fn forget(&mut self, key: &str) {
        if self.programs.shift_remove(key).is_some() {
            self.save();
        }
    }

    /// The program is known by another key now (an application moved, or updated without a
    /// signature): its commands follow.
    pub fn follow(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        if let Some(mut program) = self.programs.shift_remove(from) {
            program.key = to.to_owned();
            self.programs.insert(to.to_owned(), program);
            self.save();
        }
    }

    /// A program seen for the first time under `to` takes the approvals of `from` (the same
    /// application, updated without a signature): each command approved there is approved
    /// here, for the same version only. A command that changed with the update asks.
    pub fn inherit(&mut self, from: &str, to: &str) {
        let Some(old) = self.programs.get(from).cloned() else {
            return;
        };
        if self.programs.contains_key(to) {
            return;
        }
        let mut program = old;
        program.key = to.to_owned();
        for command in program.commands.values_mut() {
            command.missing_since = None;
            if let Some(approved) = &mut command.approved {
                approved.how = "inherited".into();
            }
        }
        self.programs.insert(to.to_owned(), program);
        self.save();
    }

    /// A program from a backup, under its key here: the commands not known here yet, and
    /// the approvals of those known here but not approved (the program may have connected
    /// before the restore). What the user decided here stays. False if nothing was added.
    pub fn restore(&mut self, mut program: Program) -> bool {
        let here = self
            .programs
            .entry(program.key.clone())
            .or_insert_with(|| Program {
                commands: IndexMap::new(),
                ..program.clone()
            });
        let mut added = false;
        for (name, mut command) in program.commands.drain(..) {
            if let Some(approved) = &mut command.approved {
                approved.how = "restored".into();
            }
            match here.commands.get_mut(&name) {
                None => {
                    here.commands.insert(name, command);
                    added = true;
                }
                Some(known) if known.approved.is_none() && known.state == State::Ask => {
                    let Some(approved) = command.approved else {
                        // Turned off there: off here too.
                        if command.state == State::Off {
                            known.state = State::Off;
                            added = true;
                        }
                        continue;
                    };
                    if known
                        .seen
                        .as_ref()
                        .is_some_and(|s| s.digest == approved.digest)
                    {
                        known.seen = None;
                    }
                    known.approved = Some(approved);
                    known.state = command.state;
                    added = true;
                }
                Some(_) => {}
            }
        }
        if added {
            self.save();
        }
        added
    }

    /// For the panel: a program's commands, with their state and what changed.
    pub fn panel(&self, key: &str) -> Value {
        let Some(program) = self.programs.get(key) else {
            return json!({ "key": key, "commands": [] });
        };
        json!({
            "key": program.key,
            "kind": program.kind,
            "label": program.label,
            "first_seen": float(program.first_seen),
            "commands": program.commands.iter().map(|(name, c)| {
                let latest = c.latest();
                json!({
                    "name": name,
                    "digest": latest.map(|v| v.digest.clone()),
                    "state": c.state.as_str(),
                    "status": c.shown_status().as_str(),
                    "description": latest.and_then(|v| v.definition.get("description")).cloned(),
                    "input_schema": latest.and_then(|v| v.definition.get("input_schema")).cloned(),
                    "approved": c.approved.as_ref().map(|v| json!({ "at": float(v.at), "how": v.how })),
                    "changes": match (&c.approved, &c.seen) {
                        (Some(before), Some(after)) => diff(&before.definition, &after.definition)
                            .iter()
                            .map(|l| json!({ "label": l.label, "value": l.value }))
                            .collect::<Vec<_>>(),
                        _ => vec![],
                    },
                    "first_seen": float(c.first_seen),
                    "missing_since": c.missing_since.map(float),
                })
            }).collect::<Vec<_>>(),
        })
    }

    /// For the snapshot, which goes out on every change: per program only the counts.
    pub fn summary(&self) -> Value {
        Value::Array(
            self.programs
                .values()
                .filter(|p| p.kind != "conn")
                .map(|p| {
                    let offered = || p.commands.values().filter(|c| c.missing_since.is_none());
                    let off = offered().filter(|c| c.state == State::Off).count();
                    json!({
                        "key": p.key,
                        "kind": p.kind,
                        "label": p.label,
                        "commands": offered().count(),
                        "approved": p.approved(),
                        "off": off,
                        "asking": offered().count() - p.approved() - off,
                        "to_review": p.to_review(),
                    })
                })
                .collect(),
        )
    }

    /// As `commands.json` keeps it: also what a backup carries. Programs nobody could
    /// identify aren't kept.
    pub fn to_json(&self) -> Value {
        json!({
            "programs": self.programs.values()
                .filter(|p| p.kind != "conn")
                .map(program_to_json)
                .collect::<Vec<_>>()
        })
    }

    /// The programs a backup carries, as they are kept.
    pub fn programs_from_json(value: &Value) -> Vec<Program> {
        value
            .get("programs")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(program_from_json).collect())
            .unwrap_or_default()
    }

    fn save(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        let draft = path.with_extension("json.tmp");
        let written = std::fs::write(&draft, wire::encode_pretty(&self.to_json()))
            .and_then(|()| std::fs::rename(&draft, path));
        self.error = written.err().map(|e| format!("{}: {e}", path.display()));
    }
}

pub fn program_to_json(p: &Program) -> Value {
    json!({
        "key": p.key,
        "kind": p.kind,
        "label": p.label,
        "origin": p.origin,
        "first_seen": float(p.first_seen),
        "last_seen": float(p.last_seen),
        "commands": p.commands.iter().map(|(name, c)| json!({
            "name": name,
            "state": c.state.as_str(),
            "approved": c.approved.as_ref().map(Version::to_json),
            "seen": c.seen.as_ref().map(Version::to_json),
            "first_seen": float(c.first_seen),
            "last_seen": float(c.last_seen),
            "missing_since": c.missing_since.map(float),
        })).collect::<Vec<_>>(),
    })
}

pub fn program_from_json(value: &Value) -> Option<Program> {
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let number = |v: &Value, key: &str| v.get(key).and_then(Value::as_f64);
    let kind = text(value, "kind")?;
    if kind == "conn" {
        return None;
    }
    let commands = value
        .get("commands")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|c| {
            let name = text(c, "name")?;
            let approved = c.get("approved").and_then(Version::from_json);
            let mut state = State::parse(c.get("state")?.as_str()?)?;
            // Approved without a version isn't approved.
            if state == State::Approved && approved.is_none() {
                state = State::Ask;
            }
            Some((
                name,
                Command {
                    state,
                    approved,
                    seen: c.get("seen").and_then(Version::from_json),
                    first_seen: number(c, "first_seen").unwrap_or(0.0),
                    last_seen: number(c, "last_seen").unwrap_or(0.0),
                    missing_since: number(c, "missing_since"),
                },
            ))
        })
        .collect();
    Some(Program {
        key: text(value, "key")?,
        kind,
        label: text(value, "label").unwrap_or_default(),
        origin: text(value, "origin"),
        first_seen: number(value, "first_seen").unwrap_or(0.0),
        last_seen: number(value, "last_seen").unwrap_or(0.0),
        commands,
    })
}

/// What changed between two versions of a command, in words: the description, each
/// parameter added, removed or changed (type, required, values, default, description), and
/// the rest of the schema as a whole. Values are cleaned and cut, but a change anywhere in
/// a long text is shown: the line says where it starts.
pub fn diff(before: &Value, after: &Value) -> Vec<ConsentLine> {
    let mut lines = Vec::new();
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let (old, new) = (text(before, "description"), text(after, "description"));
    if old != new {
        lines.push(ConsentLine::new(
            "Description, before",
            changed_part(&old, &new),
        ));
        lines.push(ConsentLine::new(
            "Description, now",
            changed_part(&new, &old),
        ));
    }
    let schema = |v: &Value| v.get("input_schema").cloned().unwrap_or(Value::Null);
    let (old_schema, new_schema) = (schema(before), schema(after));
    let properties = |s: &Value| -> Map<String, Value> {
        s.get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let required = |s: &Value| -> Vec<String> {
        s.get("required")
            .and_then(Value::as_array)
            .map(|r| {
                r.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let (old_props, new_props) = (properties(&old_schema), properties(&new_schema));
    let (old_required, new_required) = (required(&old_schema), required(&new_schema));
    for (name, value) in &new_props {
        let name_shown = clean(Some(name), 64);
        match old_props.get(name) {
            None => lines.push(ConsentLine::new(
                format!("Parameter {name_shown}"),
                format!("added: {}", clean(Some(&compact(value)), 300)),
            )),
            Some(old) => {
                let mut what = Vec::new();
                for field in ["type", "enum", "default", "description"] {
                    if old.get(field) != value.get(field) {
                        what.push(format!(
                            "{field} {} → {}",
                            clean(Some(&compact(old.get(field).unwrap_or(&Value::Null))), 120),
                            clean(
                                Some(&compact(value.get(field).unwrap_or(&Value::Null))),
                                120
                            )
                        ));
                    }
                }
                let rest = |v: &Value| {
                    let mut v = v.clone();
                    if let Some(map) = v.as_object_mut() {
                        for field in ["type", "enum", "default", "description"] {
                            map.remove(field);
                        }
                    }
                    v
                };
                if rest(old) != rest(value) {
                    what.push(format!("schema → {}", clean(Some(&compact(value)), 200)));
                }
                if !what.is_empty() {
                    lines.push(ConsentLine::new(
                        format!("Parameter {name_shown}"),
                        what.join("; "),
                    ));
                }
            }
        }
        let (was, is) = (old_required.contains(name), new_required.contains(name));
        if was != is && old_props.contains_key(name) {
            lines.push(ConsentLine::new(
                format!("Parameter {name_shown}"),
                if is {
                    "now required"
                } else {
                    "no longer required"
                },
            ));
        }
    }
    for name in old_props.keys().filter(|n| !new_props.contains_key(*n)) {
        lines.push(ConsentLine::new(
            format!("Parameter {}", clean(Some(name), 64)),
            "removed",
        ));
    }
    let outside = |s: &Value| {
        let mut s = s.clone();
        if let Some(map) = s.as_object_mut() {
            map.remove("properties");
            map.remove("required");
        }
        s
    };
    if outside(&old_schema) != outside(&new_schema) {
        lines.push(ConsentLine::new(
            "Schema",
            format!("changed: {}", clean(Some(&compact(&new_schema)), 300)),
        ));
    }
    lines
}

fn compact(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// A long text around where it differs from `other`: the window can't show it all, but it
/// must show the change.
fn changed_part(text: &str, other: &str) -> String {
    const SHOWN: usize = 4000;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= SHOWN {
        return clean(Some(text), SHOWN + 1);
    }
    let start = text
        .chars()
        .zip(other.chars())
        .take_while(|(a, b)| a == b)
        .count()
        .saturating_sub(40);
    let piece: String = chars.iter().skip(start).take(SHOWN - 40).collect();
    let more = chars.len() - start - piece.chars().count();
    format!(
        "{}{}{}",
        if start > 0 {
            format!("[from character {start}] …")
        } else {
            String::new()
        },
        clean(Some(&piece), SHOWN),
        if more > 0 {
            format!("… [{more} more]")
        } else {
            String::new()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, description: &str) -> ToolDef {
        ToolDef::new(
            name,
            description,
            json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        )
    }

    #[test]
    fn a_new_command_asks_an_approved_one_runs_a_changed_one_asks_again() {
        let mut cache = CommandCache::default();
        let read = tool("read", "Reads.");
        let seen = cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&read),
            &[],
        );
        assert_eq!(seen.new, ["read"]);
        assert_eq!(cache.status("app:k", &read), Status::New);
        cache.approve("app:k", &read, "question");
        assert_eq!(cache.status("app:k", &read), Status::Approved);

        // Offered again as it was: nothing new.
        let again = cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&read),
            &[],
        );
        assert_eq!(again, Observed::default());

        // Changed by a comma: asks, and shows what changed.
        let changed = tool("read", "Reads, then sends.");
        let seen = cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&changed),
            &[],
        );
        assert_eq!(seen.changed, ["read"]);
        assert_eq!(cache.status("app:k", &changed), Status::Changed);
        let panel = cache.panel("app:k");
        assert_eq!(panel["commands"][0]["status"], "changed");
        assert!(
            panel["commands"][0]["changes"][1]["value"]
                .as_str()
                .unwrap()
                .contains("then sends")
        );
        // The approved version still is what it was.
        assert_eq!(cache.status("app:k", &read), Status::Approved);
    }

    #[test]
    fn a_command_never_approved_and_long_gone_is_forgotten() {
        let mut cache = CommandCache::default();
        let (read, temp) = (tool("read", "Reads."), tool("temp_123", "Something open."));
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            &[read.clone(), temp.clone()],
            &[],
        );
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&read),
            &[],
        );
        // A month later.
        cache.programs.get_mut("app:k").unwrap().commands["temp_123"].missing_since =
            Some(now() - FORGET_AFTER - 1.0);
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&read),
            &[],
        );
        assert!(
            !cache
                .get("app:k")
                .unwrap()
                .commands
                .contains_key("temp_123")
        );
    }

    #[test]
    fn a_command_another_instance_offers_isn_t_missing() {
        let mut cache = CommandCache::default();
        let (read, write) = (tool("read", "Reads."), tool("write", "Writes."));
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            &[read.clone(), write.clone()],
            &[],
        );
        cache.set_state("app:k", "write", State::Off, None);
        // Another instance offers only `read`, while the first still offers `write`.
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&read),
            &["write".to_owned()],
        );
        assert!(
            cache.get("app:k").unwrap().commands["write"]
                .missing_since
                .is_none()
        );
        // Gone everywhere: missing; forgotten from the channel, the turned off one stays.
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&read),
            &[],
        );
        cache.forget_missing("app:k", true);
        assert_eq!(cache.status("app:k", &write), Status::Off);
    }

    #[test]
    fn gone_and_back_as_it_was_keeps_its_approval() {
        let mut cache = CommandCache::default();
        let (read, write) = (tool("read", "Reads."), tool("write", "Writes."));
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            &[read.clone(), write.clone()],
            &[],
        );
        cache.approve("app:k", &write, "question");
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&read),
            &[],
        );
        assert!(
            cache.get("app:k").unwrap().commands["write"]
                .missing_since
                .is_some()
        );
        let back = cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            &[read.clone(), write.clone()],
            &[],
        );
        assert_eq!(back, Observed::default());
        assert_eq!(cache.status("app:k", &write), Status::Approved);
        cache.forget_missing("app:k", false);
        assert_eq!(cache.get("app:k").unwrap().commands.len(), 2);
    }

    #[test]
    fn another_server_behind_the_same_id_loses_the_approvals() {
        let mut cache = CommandCache::default();
        let read = tool("read", "Reads.");
        cache.observe(
            "mcp:1",
            "mcp",
            "Notes",
            Some("https://a.example/mcp"),
            std::slice::from_ref(&read),
            &[],
        );
        cache.approve("mcp:1", &read, "question");
        cache.observe(
            "mcp:1",
            "mcp",
            "Notes",
            Some("https://b.example/mcp"),
            std::slice::from_ref(&read),
            &[],
        );
        assert_eq!(cache.status("mcp:1", &read), Status::New);
    }

    #[test]
    fn states_set_from_the_panel() {
        let mut cache = CommandCache::default();
        let read = tool("read", "Reads.");
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&read),
            &[],
        );
        // Approved only as the user saw it.
        assert!(!cache.set_state("app:k", "read", State::Approved, Some("another")));
        assert!(!cache.set_state("app:k", "read", State::Approved, None));
        assert!(cache.set_state("app:k", "read", State::Approved, Some(&read.digest)));
        assert_eq!(cache.status("app:k", &read), Status::Approved);
        assert!(cache.set_state("app:k", "read", State::Ask, None));
        assert_eq!(cache.status("app:k", &read), Status::Ask);
        assert!(cache.set_state("app:k", "read", State::Off, None));
        assert_eq!(cache.status("app:k", &read), Status::Off);
        cache.reset("app:k");
        assert_eq!(cache.status("app:k", &read), Status::Off);
        assert!(cache.set_state("app:k", "read", State::Ask, None));
        // Approved once, asking every time now: the user's choice, which "approve all"
        // leaves as it is; and a reset makes it ask, not look new.
        assert_eq!(cache.status("app:k", &read), Status::Ask);
        assert_eq!(cache.approve_all("app:k", "panel", None), 0);
        assert!(cache.set_state("app:k", "read", State::Approved, Some(&read.digest)));
        cache.reset("app:k");
        assert_eq!(cache.status("app:k", &read), Status::Ask);
        // A new version needs the user: "approve all" takes it, if it is the one shown.
        let newer = tool("read", "Reads more.");
        cache.set_state("app:k", "read", State::Approved, Some(&read.digest));
        cache.observe(
            "app:k",
            "app",
            "Notes",
            None,
            std::slice::from_ref(&newer),
            &[],
        );
        let stale = [("read".to_owned(), "another".to_owned())];
        assert_eq!(cache.approve_all("app:k", "panel", Some(&stale)), 0);
        assert_eq!(cache.approve_all("app:k", "panel", None), 1);
        assert_eq!(cache.status("app:k", &newer), Status::Approved);
        assert!(!cache.set_state("app:k", "nothing", State::Off, None));
    }

    #[test]
    fn the_file_keeps_what_the_user_decided_and_a_damaged_one_starts_over() {
        let dir =
            std::env::temp_dir().join(format!("clak-commands-{}", clak_wire::json::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("commands.json");
        let read = tool("read", "Reads.");
        {
            let mut cache = CommandCache::load(path.clone());
            cache.observe(
                "app:k",
                "app",
                "Notes",
                None,
                std::slice::from_ref(&read),
                &[],
            );
            cache.approve("app:k", &read, "question");
            cache.observe(
                "conn:x",
                "conn",
                "Who",
                None,
                std::slice::from_ref(&read),
                &[],
            );
        }
        let cache = CommandCache::load(path.clone());
        assert_eq!(cache.status("app:k", &read), Status::Approved);
        assert!(cache.get("conn:x").is_none());
        // A definition edited in the file is not approved as the one the user saw.
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("Reads.", "Reads all.");
        std::fs::write(&path, text).unwrap();
        let cache = CommandCache::load(path.clone());
        assert_eq!(cache.status("app:k", &read), Status::Changed);
        std::fs::write(&path, "{ not json").unwrap();
        assert!(CommandCache::load(path).get("app:k").is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_backup_brings_back_approvals_also_for_a_program_that_connected_first() {
        let mut cache = CommandCache::default();
        let (read, write) = (tool("read", "Reads."), tool("write", "Writes."));
        cache.observe(
            "mcp:old",
            "mcp",
            "Notes",
            None,
            &[read.clone(), write.clone()],
            &[],
        );
        cache.approve("mcp:old", &read, "question");
        let mut saved = cache.get("mcp:old").unwrap().clone();

        // Here the server connected before the restore: its commands are new.
        let mut here = CommandCache::default();
        here.observe(
            "mcp:new",
            "mcp",
            "Notes",
            None,
            &[read.clone(), write.clone()],
            &[],
        );
        here.set_state("mcp:new", "write", State::Off, None);
        saved.key = "mcp:new".into();
        assert!(here.restore(saved));
        assert_eq!(here.status("mcp:new", &read), Status::Approved);
        // What the user decided here stays.
        assert_eq!(here.status("mcp:new", &write), Status::Off);
        assert_eq!(
            here.get("mcp:new").unwrap().commands["read"]
                .approved
                .as_ref()
                .unwrap()
                .how,
            "restored"
        );
    }

    #[test]
    fn a_change_far_into_a_long_description_is_shown() {
        let long = "a".repeat(6000);
        let before = tool("read", &format!("{long} keep it"));
        let after = tool("read", &format!("{long} send it all away"));
        let lines = diff(&before.to_json(), &after.to_json());
        assert!(
            lines[1].value.contains("send it all away"),
            "{}",
            lines[1].value
        );
        assert!(lines[1].value.starts_with("[from character"));

        let schema = |props: Value, required: Value| {
            ToolDef::new(
                "w",
                "W.",
                json!({"type": "object", "properties": props, "required": required}),
            )
        };
        let lines = diff(
            &schema(
                json!({"a": {"type": "string"}, "b": {"type": "string"}}),
                json!(["a"]),
            )
            .to_json(),
            &schema(
                json!({"a": {"type": "integer"}, "c": {"type": "string"}}),
                json!([]),
            )
            .to_json(),
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| format!("{}: {}", l.label, l.value))
            .collect();
        assert!(
            text.iter().any(|l| l.starts_with("Parameter a: type")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|l| l == "Parameter a: no longer required"),
            "{text:?}"
        );
        assert!(
            text.iter().any(|l| l.starts_with("Parameter c: added")),
            "{text:?}"
        );
        assert!(text.iter().any(|l| l == "Parameter b: removed"), "{text:?}");
    }
}
