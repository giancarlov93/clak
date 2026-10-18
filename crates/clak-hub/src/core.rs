//! The hub: who is connected, who has permission for what, and the only place where
//! permission is enforced. It never interprets the content of a command.
//!
//! Three registries and one rule. Programs connect (and the user allows it once per
//! executable); agents connect (and the user allows it once per process); an agent uses a
//! program only if the user allows it for *that* pair. A command that is new or changed
//! after that consent needs its own.
//!
//! All state lives in [`Core`], under a single lock taken by [`Broker::run`]: nothing waits
//! in there. The waits (a question to the user, a program's answer) are outside, in
//! [`Broker::consent`], [`Broker::confirm`] and in whoever waits for an answer.

use crate::consent::{AgentIdentity, ConsentRequest, HubOptions, Verdict};
use crate::permissions::AppPermissions;
use crate::wire::{ResourceDef, ToolDef, origin_label, origin_short};
use crate::{cli_names, texts};
use clak_wire::json::{self, float, new_id, now, quoted, round3};
use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

/// The outgoing queue toward a peer. The hub decides everything under the lock and cannot
/// wait in there for a slow process to read: it queues and moves on. The actual writing is
/// done by one task per connection, just one, so messages arrive in the order the hub
/// decided them.
#[derive(Clone, Debug)]
pub struct Outbox(mpsc::UnboundedSender<Value>);

impl Outbox {
    pub fn channel() -> (Outbox, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Outbox(tx), rx)
    }

    /// If nobody is on the other side any more, the message is dropped.
    pub fn send(&self, message: Value) {
        let _ = self.0.send(message);
    }
}

/// The lines that tell the caller what it is waiting for. With nobody to receive them
/// (tests, for example) they do nothing.
#[derive(Clone, Default, Debug)]
pub struct Progress(Option<Outbox>);

impl Progress {
    pub fn new(outbox: Option<Outbox>) -> Self {
        Progress(outbox)
    }

    pub fn waiting(&self, what: &str) {
        self.tell("consent", &format!("waiting for the user: Clak is asking them, in its own window, {what}…"));
    }

    pub fn allowed(&self, text: &str) {
        self.tell("allowed", text);
    }

    pub fn tell(&self, stage: &str, text: &str) {
        if let Some(outbox) = &self.0 {
            outbox.send(json!({ "type": "progress", "stage": stage, "text": text }));
        }
    }
}

/// A connected program: an open document, not an application.
#[derive(Clone)]
pub struct AppConn {
    pub conn_id: String,
    /// The name agents call it by: the hub decides it, not the program. It changes only when
    /// the user renames a connector ([`Core::rename`]).
    pub name: String,
    pub resource: ResourceDef,
    /// Read from the operating system, not declared by the program.
    pub origin: Value,
    /// The commands enabled right now.
    pub tools: Vec<ToolDef>,
    pub since: f64,
    /// For a connector that does not answer: «not responding». Nothing when all is well.
    pub status: Option<String>,
    /// How the program wants to be used, if it says so: `clak <program> -h` shows it.
    pub instructions: Option<String>,
    outbox: Outbox,
}

impl AppConn {
    /// An MCP server or a REST API reached by the hub, not a process that connected.
    pub fn is_connector(&self) -> bool {
        self.origin.get("kind").and_then(Value::as_str) == Some("connector")
    }

    /// The hub's settings: a program that is the hub itself.
    pub fn is_hub(&self) -> bool {
        self.origin.get("kind").and_then(Value::as_str) == Some("hub")
    }

    pub fn send(&self, message: Value) {
        self.outbox.send(message);
    }
}

/// An authorized agent: a process, as long as it lives.
pub struct SessionEntry {
    pub id: String,
    pub agent: AgentIdentity,
    /// The folder it says it works in: declared, it only helps to recognize it.
    pub cwd: Option<String>,
    /// Where it came from the first time: the command line or the MCP shim.
    pub via: String,
    pub since: f64,
}

impl SessionEntry {
    pub fn label(&self) -> String {
        match self.cwd.as_deref().filter(|c| !c.is_empty()) {
            Some(dir) => format!("{} · {}", self.agent.name, folder(dir)),
            None => self.agent.name.clone(),
        }
    }
}

fn folder(dir: &str) -> String {
    let trimmed = dir.trim_end_matches(['\\', '/']);
    match trimmed.rsplit(['\\', '/']).next() {
        Some(leaf) if !leaf.is_empty() => leaf.to_owned(),
        _ => dir.to_owned(),
    }
}

/// A finished call: command, time, duration and outcome. What the agent sent and what came
/// back are kept too, but only in memory, trimmed, and for the latest calls alone
/// ([`DETAIL_LIMIT`]): the panel shows them on request ([`Core::panel_call`]).
#[derive(Debug, Clone)]
pub struct CallRecord {
    pub id: String,
    pub tool: String,
    pub at: f64,
    pub seconds: f64,
    pub outcome: &'static str,
    pub detail: Option<CallDetail>,
}

/// The arguments and the reply of a call, trimmed by [`slim`].
#[derive(Debug, Clone)]
pub struct CallDetail {
    pub arguments: Value,
    pub result: Value,
}

/// How many of a permission's latest calls keep their arguments and reply.
pub const DETAIL_LIMIT: usize = 100;
/// The longest text kept from an argument or a reply, in characters.
const TEXT_LIMIT: usize = 20_000;

/// A value as the panel may keep it: long texts cut, images and audio replaced by their size.
pub fn slim(value: &Value) -> Value {
    match value {
        Value::String(text) => match text.char_indices().nth(TEXT_LIMIT) {
            Some((cut, _)) => {
                let more = text[cut..].chars().count();
                Value::String(format!("{}… [{more} more characters]", &text[..cut]))
            }
            None => value.clone(),
        },
        Value::Array(items) => Value::Array(items.iter().map(slim).collect()),
        Value::Object(map) => {
            let media = matches!(map.get("type").and_then(Value::as_str), Some("image" | "audio"));
            Value::Object(
                map.iter()
                    .map(|(k, v)| match (media, k.as_str(), v) {
                        (true, "data", Value::String(data)) => (k.clone(), Value::String(format!("[{} bytes, base64]", data.len()))),
                        _ => (k.clone(), slim(v)),
                    })
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

pub mod outcome {
    pub const OK: &str = "ok";
    pub const ERROR: &str = "error";
    pub const TIMEOUT: &str = "timeout";
    pub const CLOSED: &str = "closed";
    pub const INTERRUPTED: &str = "interrupted";

    /// A program failure: it answered with an error, did not answer in time, or closed
    /// halfway. An agent leaving before the answer is not the program's fault.
    pub fn failed(outcome: &str) -> bool {
        matches!(outcome, ERROR | TIMEOUT | CLOSED)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolStats {
    pub calls: u64,
    pub errors: u64,
    pub seconds: f64,
}

/// An agent's permission on a program, with the commands approved verbatim.
pub struct GrantEntry {
    pub id: String,
    /// The agent's key (pid and start time).
    pub session_key: String,
    pub app_id: String,
    /// Name → digest. A changed command is no longer the approved one.
    pub approved: HashMap<String, String>,
    pub since: f64,
    /// Includes calls in progress; [`GrantEntry::finished`] only the recorded ones.
    pub calls: u64,
    pub errors: u64,
    pub finished: u64,
    pub last_call: Option<f64>,
    /// The latest calls, oldest first.
    pub history: VecDeque<CallRecord>,
    /// Per command, all calls since the permission began: the history, instead, gets trimmed.
    pub stats: IndexMap<String, ToolStats>,
}

pub const HISTORY_LIMIT: usize = 500;

impl GrantEntry {
    pub fn approves(&self, tool: &ToolDef) -> bool {
        self.approved.get(&tool.name) == Some(&tool.digest)
    }

    fn record(&mut self, call: CallRecord) {
        self.finished += 1;
        let stats = self.stats.entry(call.tool.clone()).or_default();
        stats.calls += 1;
        stats.seconds += call.seconds;
        if outcome::failed(call.outcome) {
            stats.errors += 1;
            self.errors += 1;
        }
        self.history.push_back(call);
        if self.history.len() > HISTORY_LIMIT {
            self.history.pop_front();
        }
        // Only the latest calls keep what was sent and received.
        if let Some(older) = self.history.len().checked_sub(DETAIL_LIMIT + 1) {
            self.history[older].detail = None;
        }
    }
}

type Allow = Box<dyn FnOnce(&mut Core) + Send>;

/// A question to the user in progress. Whoever asks the same thing waits for the same answer.
struct PendingConsent {
    request: ConsentRequest,
    allow: Option<Allow>,
    done: watch::Sender<Option<Verdict>>,
    cancel: CancellationToken,
    waiters: usize,
}

/// A command on its way to a program, and the agent that asked for it.
struct PendingCall {
    answer: oneshot::Sender<Value>,
    conn_id: String,
    session_key: String,
    grant_id: Option<String>,
    tool: String,
    /// What the agent sent, already trimmed, for the call's detail.
    arguments: Value,
    started: f64,
    progress: Progress,
}

const BACKLOG: usize = 200;

/// The hub's state. Touched only inside [`Broker::run`].
pub struct Core {
    control: String,
    pub ws_port: u16,
    started: f64,
    token: String,
    can_ask: bool,
    denial_memory: std::time::Duration,
    permissions: AppPermissions,
    apps: IndexMap<String, AppConn>,
    /// By agent key.
    sessions: IndexMap<String, SessionEntry>,
    grants: IndexMap<String, GrantEntry>,
    calls: HashMap<String, PendingCall>,
    pending: IndexMap<String, PendingConsent>,
    denied: HashMap<String, Instant>,
    confirms: IndexMap<String, ConsentRequest>,
    monitors: IndexMap<String, Outbox>,
    events: VecDeque<Value>,
    dirty: bool,
    sink: Option<mpsc::UnboundedSender<String>>,
    connectors: Value,
    apis: Value,
}

impl Core {
    // =====================================================================
    // Who watches: the panel
    // =====================================================================

    /// Everything the hub knows, in a form that contains nothing to execute.
    pub fn snapshot(&self) -> Value {
        json!({
            "type": "state",
            "broker": {
                "pid": std::process::id(),
                "since": float(self.started),
                "ws_port": self.ws_port,
                "control": self.control,
                "can_ask": self.can_ask,
            },
            "sessions": self.sessions.values().map(|s| json!({
                "id": s.id,
                "label": s.label(),
                "name": s.agent.name,
                "exe": s.agent.exe,
                "pid": s.agent.pid,
                "signed": s.agent.signed,
                "signer": s.agent.signer,
                "cwd": s.cwd,
                "via": s.via,
                "since": float(s.since),
            })).collect::<Vec<_>>(),
            "resources": self.apps.values().map(|a| json!({
                "id": a.conn_id,
                "name": a.name,
                "resource": a.resource.to_json(),
                "origin": a.origin,
                "status": a.status,
                "since": float(a.since),
                "tools": a.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "grants": self.grants.values().filter_map(|g| {
                let app = self.apps.get(&g.app_id)?;
                let session = self.sessions.get(&g.session_key)?;
                Some(json!({
                    "id": g.id,
                    "resource_id": g.app_id,
                    "session_id": session.id,
                    "since": float(g.since),
                    "tools": app.tools.iter().filter(|t| g.approves(t)).map(|t| t.name.clone()).collect::<Vec<_>>(),
                    "calls": g.calls,
                    "errors": g.errors,
                    "finished": g.finished,
                    "last_call": g.last_call.map(float),
                }))
            }).collect::<Vec<_>>(),
            "pending": self.pending.values().map(|p| &p.request).chain(self.confirms.values()).map(|r| json!({
                "id": r.id,
                "kind": r.kind.as_str(),
                "title": r.title,
            })).collect::<Vec<_>>(),
            "apps": self.permissions.all().iter().map(|e| json!({
                "key": e.key, "exe": e.exe, "name": e.name, "signer": e.signer, "since": float(e.since),
            })).collect::<Vec<_>>(),
            "connectors": self.connectors,
            "apis": self.apis,
        })
    }

    pub fn touch(&mut self) {
        self.dirty = true;
    }

    fn publish(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if self.monitors.is_empty() {
            return;
        }
        let state = self.snapshot();
        for monitor in self.monitors.values() {
            monitor.send(state.clone());
        }
    }

    pub fn log(&mut self, text: impl Into<String>) {
        let text = text.into();
        let item = json!({ "ts": float(now()), "text": text });
        self.events.push_back(item.clone());
        if self.events.len() > BACKLOG {
            self.events.pop_front();
        }
        for monitor in self.monitors.values() {
            monitor.send(json!({ "type": "log", "items": [item.clone()] }));
        }
        self.touch();
        if let Some(sink) = &self.sink {
            let _ = sink.send(text);
        }
    }

    /// Whoever opens the panel first receives the log backlog, then the state.
    pub fn watch(&mut self, monitor_id: &str, outbox: Outbox) {
        outbox.send(json!({ "type": "log", "items": self.events.iter().cloned().collect::<Vec<_>>() }));
        outbox.send(self.snapshot());
        self.monitors.insert(monitor_id.to_owned(), outbox);
    }

    pub fn unwatch(&mut self, monitor_id: &str) {
        self.monitors.shift_remove(monitor_id);
    }

    /// The MCP servers as the panel sees them: whoever holds them provides them, on every change.
    pub fn set_connectors(&mut self, view: Value) {
        self.connectors = view;
        self.touch();
    }

    /// The REST APIs as the panel sees them, in the same way.
    pub fn set_apis(&mut self, view: Value) {
        self.apis = view;
        self.touch();
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn can_ask(&self) -> bool {
        self.can_ask
    }

    // =====================================================================
    // Programs
    // =====================================================================

    /// The command-line name: the one chosen by the user for a connector, or the one derived
    /// from the program's name. Two documents of the same program become «word» and
    /// «word-2».
    fn name_for(&self, resource: &ResourceDef, preferred: Option<&str>, own: Option<&str>) -> String {
        let slug = match preferred {
            Some(p) if !p.is_empty() && cli_names::problem(Some(p)).is_none() => p.to_owned(),
            _ => cli_names::from(&resource.app),
        };
        let mut name = slug.clone();
        let mut n = 2;
        while self.apps.values().any(|a| Some(a.conn_id.as_str()) != own && a.name == name) {
            name = format!("{slug}-{n}");
            n += 1;
        }
        name
    }

    /// A program is added to the list; it receives `registered` with its name.
    pub fn register(
        &mut self,
        outbox: Outbox,
        resource: ResourceDef,
        tools: Vec<ToolDef>,
        origin: Value,
        preferred: Option<&str>,
    ) -> String {
        // Only the hub takes the reserved name, and only if nobody else already holds it: name_for never gives it out.
        let name = if origin.get("kind").and_then(Value::as_str) == Some("hub") && self.app(cli_names::SETTINGS).is_none() {
            cli_names::SETTINGS.to_owned()
        } else {
            self.name_for(&resource, preferred, None)
        };
        let conn_id = new_id("app");
        let count = tools.len();
        let app = AppConn {
            conn_id: conn_id.clone(),
            name,
            resource,
            origin,
            tools,
            since: now(),
            status: None,
            instructions: None,
            outbox,
        };
        let line = format!(
            "program connected: {} = {} [{}], {count} {}",
            app.name,
            app.resource.label(),
            origin_label(&app.origin),
            if count == 1 { "command" } else { "commands" }
        );
        app.send(json!({ "type": "registered", "name": app.name, "protocol": clak_wire::PROTOCOL }));
        self.apps.insert(conn_id.clone(), app);
        self.log(line);
        self.touch();
        conn_id
    }

    pub fn app(&self, name: &str) -> Option<&AppConn> {
        self.apps.values().find(|a| a.name == name)
    }

    pub fn app_by_id(&self, conn_id: &str) -> Option<&AppConn> {
        self.apps.get(conn_id)
    }

    pub fn app_by_id_mut(&mut self, conn_id: &str) -> Option<&mut AppConn> {
        self.touch();
        self.apps.get_mut(conn_id)
    }

    pub fn app_names(&self) -> Vec<String> {
        self.apps.values().map(|a| a.name.clone()).collect()
    }

    pub fn app_count(&self) -> usize {
        self.apps.len()
    }

    pub fn registered(&self, conn_id: &str) -> bool {
        self.apps.contains_key(conn_id)
    }

    pub fn tools_changed(&mut self, conn_id: &str, tools: Vec<ToolDef>) {
        let Some(app) = self.apps.get_mut(conn_id) else { return };
        let before: Vec<String> = app.tools.iter().map(|t| t.name.clone()).collect();
        let after: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        app.tools = tools;
        let name = app.name.clone();
        let added: Vec<&String> = after.iter().filter(|n| !before.contains(n)).collect();
        let removed: Vec<&String> = before.iter().filter(|n| !after.contains(n)).collect();
        if !added.is_empty() || !removed.is_empty() {
            let mut line = format!("{name}: commands ");
            if !added.is_empty() {
                line.push_str(&format!("+{} ", quoted(&added)));
            }
            if !removed.is_empty() {
                line.push_str(&format!("-{}", quoted(&removed)));
            }
            self.log(line.trim_end().to_owned());
        }
        self.touch();
    }

    pub fn set_instructions(&mut self, conn_id: &str, instructions: Option<String>) {
        if let Some(app) = self.apps.get_mut(conn_id) {
            app.instructions = instructions;
        }
    }

    pub fn set_status(&mut self, conn_id: &str, status: Option<String>) {
        if let Some(app) = self.apps.get_mut(conn_id) {
            app.status = status;
        }
        self.touch();
    }

    /// The user renamed a connector: name and command change, permissions stay, because the
    /// service is the same. An agent still using the old name is told that program is not
    /// there, and rereads the list.
    pub fn rename(&mut self, conn_id: &str, resource: ResourceDef, preferred: Option<&str>) {
        if !self.apps.contains_key(conn_id) {
            return;
        }
        let name = self.name_for(&resource, preferred, Some(conn_id));
        let app = self.apps.get_mut(conn_id).expect("checked above");
        let before = std::mem::replace(&mut app.name, name);
        app.resource = resource;
        let line = if before == app.name {
            format!("program renamed: {} = {}", app.name, app.resource.label())
        } else {
            format!("program renamed: {before} → {} = {}", app.name, app.resource.label())
        };
        self.log(line);
        self.touch();
    }

    /// The program's connection has ended: its permissions end with it.
    pub fn app_closed(&mut self, conn_id: &str, reason: &str) {
        let Some(app) = self.apps.shift_remove(conn_id) else { return };
        self.grants.retain(|_, g| g.app_id != conn_id);
        let routed: Vec<String> = self.calls.iter().filter(|(_, c)| c.conn_id == conn_id).map(|(k, _)| k.clone()).collect();
        for id in routed {
            self.complete(&id, failure("the program was closed during the command"), outcome::CLOSED);
        }
        self.log(format!("program disconnected: {} = {} ({reason})", app.name, app.resource.label()));
        self.touch();
    }

    /// The panel removes a program from the remembered ones. Whoever is already connected stays.
    pub fn forget_app(&mut self, key: Option<&str>) {
        if let Some(key) = key
            && self.permissions.forget(key)
        {
            self.log("program removed from the allowed ones");
        }
        self.touch();
    }

    pub fn permissions(&self) -> &AppPermissions {
        &self.permissions
    }

    pub fn permissions_mut(&mut self) -> &mut AppPermissions {
        self.touch();
        &mut self.permissions
    }

    // =====================================================================
    // Agents
    // =====================================================================

    pub fn session(&self, agent: &AgentIdentity) -> Option<&SessionEntry> {
        self.sessions.get(&agent.key())
    }

    pub fn session_by_key(&self, key: &str) -> Option<&SessionEntry> {
        self.sessions.get(key)
    }

    /// The authorized agents, by key: a process launched by one of them belongs to it.
    pub fn session_keys(&self) -> HashSet<String> {
        self.sessions.keys().cloned().collect()
    }

    /// The agent becomes authorized (if it is not already); returns its key.
    pub fn authorize(&mut self, agent: &AgentIdentity, cwd: Option<&str>, via: &str) -> String {
        let key = agent.key();
        if self.sessions.contains_key(&key) {
            return key;
        }
        let session = SessionEntry {
            id: new_id("ses"),
            agent: agent.clone(),
            cwd: cwd.map(str::to_owned),
            via: via.to_owned(),
            since: now(),
        };
        let line = format!(
            "agent connected: {} [{}, pid {}, via {via}]",
            session.label(),
            agent.exe.as_deref().unwrap_or("?"),
            agent.pid
        );
        self.sessions.insert(key.clone(), session);
        self.log(line);
        self.touch();
        key
    }

    /// An agent is no longer authorized: it exited, or the user disconnected it.
    pub fn forget_session(&mut self, session_id: Option<&str>, reason: &str) {
        let Some(key) = self.sessions.iter().find(|(_, s)| Some(s.id.as_str()) == session_id).map(|(k, _)| k.clone()) else {
            return;
        };
        let session = self.sessions.shift_remove(&key).expect("found above");
        let ended: Vec<String> = self.grants.values().filter(|g| g.session_key == key).map(|g| g.id.clone()).collect();
        for grant in ended {
            self.end(&grant, reason, &session);
        }
        let why = if reason == "harness_gone" { "process ended" } else { reason };
        self.log(format!("agent disconnected: {} ({why})", session.label()));
        self.touch();
    }

    /// The authorized agents, to check outside the lock whether they are still alive.
    pub fn sessions(&self) -> Vec<(String, AgentIdentity)> {
        self.sessions.values().map(|s| (s.id.clone(), s.agent.clone())).collect()
    }

    // =====================================================================
    // Permissions
    // =====================================================================

    pub fn grant_of(&self, session_key: &str, conn_id: &str) -> Option<&GrantEntry> {
        self.grants.values().find(|g| g.session_key == session_key && g.app_id == conn_id)
    }

    pub fn grant_by_id(&self, grant_id: &str) -> Option<&GrantEntry> {
        self.grants.get(grant_id)
    }

    pub fn grant_by_id_mut(&mut self, grant_id: &str) -> Option<&mut GrantEntry> {
        self.dirty = true;
        self.grants.get_mut(grant_id)
    }

    /// The agent's permission on the program (if not already there); returns its id.
    pub fn grant(&mut self, session_key: &str, conn_id: &str) -> Option<String> {
        if let Some(known) = self.grant_of(session_key, conn_id) {
            return Some(known.id.clone());
        }
        let session = self.sessions.get(session_key)?;
        let app = self.apps.get(conn_id)?;
        let grant = GrantEntry {
            id: new_id("grant"),
            session_key: session_key.to_owned(),
            app_id: conn_id.to_owned(),
            approved: app.tools.iter().map(|t| (t.name.clone(), t.digest.clone())).collect(),
            since: now(),
            calls: 0,
            errors: 0,
            finished: 0,
            last_call: None,
            history: VecDeque::new(),
            stats: IndexMap::new(),
        };
        let label = session.label();
        app.send(json!({ "type": "granted", "grant_id": grant.id, "harness": label }));
        let line = format!("{label} can use {} ({})", app.name, app.resource.label());
        let id = grant.id.clone();
        self.grants.insert(id.clone(), grant);
        self.log(line);
        self.touch();
        Some(id)
    }

    fn end(&mut self, grant_id: &str, reason: &str, session: &SessionEntry) {
        let Some(grant) = self.grants.shift_remove(grant_id) else { return };
        if let Some(app) = self.apps.get(&grant.app_id) {
            app.send(json!({
                "type": "revoked", "grant_id": grant.id, "reason": reason, "harness": session.label(),
            }));
        }
        self.touch();
    }

    /// Revocation from the panel: whoever looks at the panel is the user.
    pub fn revoke(&mut self, grant_id: Option<&str>) {
        let Some(grant) = grant_id.and_then(|id| self.grants.get(id)) else { return };
        let (Some(session), Some(app)) = (self.sessions.get(&grant.session_key), self.apps.get(&grant.app_id)) else {
            return;
        };
        let line = format!("revoked: {} can no longer use {}", session.label(), app.name);
        let id = grant.id.clone();
        let label = session.label();
        self.log(line);
        if let Some(grant) = self.grants.shift_remove(&id)
            && let Some(app) = self.apps.get(&grant.app_id)
        {
            app.send(json!({ "type": "revoked", "grant_id": grant.id, "reason": "user", "harness": label }));
        }
        self.touch();
    }

    // =====================================================================
    // What an agent sees
    // =====================================================================

    pub fn listing(&self, session_key: &str) -> Value {
        Value::Array(
            self.apps
                .values()
                .map(|a| {
                    json!({
                        "name": a.name,
                        "app": a.resource.app,
                        "title": a.resource.title,
                        "publisher": a.resource.publisher,
                        "origin": match &a.status {
                            Some(status) => format!("{} — {status}", origin_short(&a.origin)),
                            None => origin_short(&a.origin),
                        },
                        "tools": a.tools.len(),
                        "allowed": self.grant_of(session_key, &a.conn_id).is_some(),
                    })
                })
                .collect(),
        )
    }

    pub fn describe(&self, session_key: &str, conn_id: &str, query: Option<&str>) -> Option<Map<String, Value>> {
        let app = self.apps.get(conn_id)?;
        let grant = self.grant_of(session_key, conn_id);
        let terms: Vec<String> = query.unwrap_or("").split(' ').map(str::trim).filter(|t| !t.is_empty()).map(str::to_lowercase).collect();
        let tools: Vec<Value> = app
            .tools
            .iter()
            .filter(|t| {
                let name = t.name.to_lowercase();
                let description = t.description.to_lowercase();
                terms.iter().all(|term| name.contains(term) || description.contains(term))
            })
            .map(|t| {
                let mut item = t.to_json();
                item["approved"] = Value::Bool(grant.is_some_and(|g| g.approves(t)));
                item
            })
            .collect();
        let value = json!({
            "name": app.name,
            "app": app.resource.app,
            "title": app.resource.title,
            "allowed": grant.is_some(),
            "instructions": app.instructions,
            "tools": tools,
        });
        value.as_object().cloned()
    }

    /// A program's commands for the panel, with description and parameters: the snapshot
    /// has only the names, because it is sent on every change.
    pub fn panel_tools(&self, resource_id: Option<&str>) -> Value {
        match resource_id.and_then(|id| self.apps.get(id)) {
            Some(app) => json!({
                "type": "tools",
                "resource_id": app.conn_id,
                "found": true,
                "instructions": app.instructions,
                "tools": app.tools.iter().map(ToolDef::to_json).collect::<Vec<_>>(),
            }),
            None => json!({ "type": "tools", "resource_id": resource_id, "found": false, "tools": [] }),
        }
    }

    /// A permission's history for the panel: the calls, most recent first, and the counts per
    /// command. No arguments or results: those only in [`Core::panel_call`], one call at a time.
    pub fn panel_history(&self, grant_id: Option<&str>) -> Value {
        match grant_id.and_then(|id| self.grants.get(id)) {
            Some(grant) => json!({
                "type": "history",
                "grant_id": grant.id,
                "found": true,
                "limit": HISTORY_LIMIT,
                "calls": grant.history.iter().rev().map(|c| json!({
                    "id": c.id, "tool": c.tool, "at": float(c.at), "seconds": round3(c.seconds), "outcome": c.outcome,
                    "detail": c.detail.is_some(),
                })).collect::<Vec<_>>(),
                "stats": grant.stats.iter().map(|(tool, s)| json!({
                    "tool": tool, "calls": s.calls, "errors": s.errors, "seconds": round3(s.seconds),
                })).collect::<Vec<_>>(),
            }),
            None => json!({ "type": "history", "grant_id": grant_id, "found": false, "calls": [], "stats": [] }),
        }
    }

    /// One call in full: what the agent sent and what came back. Only for the app hosting
    /// the hub (control_channel.rs, `panel_message`): never on the control channel, where
    /// any process of the user, an agent included, could read another agent's calls.
    pub fn panel_call(&self, grant_id: Option<&str>, call_id: Option<&str>) -> Value {
        let call = grant_id
            .and_then(|id| self.grants.get(id))
            .and_then(|g| g.history.iter().find(|c| Some(c.id.as_str()) == call_id));
        match call {
            Some(c) => json!({
                "type": "call",
                "grant_id": grant_id,
                "call_id": c.id,
                "found": true,
                "tool": c.tool,
                "at": float(c.at),
                "seconds": round3(c.seconds),
                "outcome": c.outcome,
                "kept": c.detail.is_some(),
                "arguments": c.detail.as_ref().map(|d| d.arguments.clone()),
                "result": c.detail.as_ref().map(|d| d.result.clone()),
            }),
            None => json!({ "type": "call", "grant_id": grant_id, "call_id": call_id, "found": false }),
        }
    }

    // =====================================================================
    // Commands
    // =====================================================================

    /// The command goes out to the program; the answer will arrive on the receiver.
    pub fn begin_call(
        &mut self,
        grant_id: &str,
        tool: &ToolDef,
        arguments: Option<Map<String, Value>>,
        progress: Progress,
    ) -> Option<(String, oneshot::Receiver<Value>)> {
        let grant = self.grants.get_mut(grant_id)?;
        let started = now();
        grant.calls += 1;
        grant.last_call = Some(started);
        let (conn_id, session_key) = (grant.app_id.clone(), grant.session_key.clone());
        let label = self.sessions.get(&session_key).map(SessionEntry::label).unwrap_or_default();
        let app = self.apps.get(&conn_id)?;
        let routed = new_id("call");
        let (answer, receiver) = oneshot::channel();
        let arguments = Value::Object(arguments.unwrap_or_default());
        let kept = slim(&arguments);
        app.send(json!({
            "type": "call",
            "call_id": routed,
            "tool": tool.name,
            "arguments": arguments,
            "harness": label,
        }));
        self.calls.insert(
            routed.clone(),
            PendingCall {
                answer,
                conn_id,
                session_key,
                grant_id: Some(grant_id.to_owned()),
                tool: tool.name.clone(),
                arguments: kept,
                started,
                progress,
            },
        );
        self.touch();
        Some((routed, receiver))
    }

    fn complete(&mut self, routed: &str, result: Value, outcome: &'static str) {
        let Some(call) = self.calls.remove(routed) else { return };
        if let Some(grant) = call.grant_id.as_deref().and_then(|id| self.grants.get_mut(id)) {
            grant.record(CallRecord {
                id: routed.to_owned(),
                tool: call.tool.clone(),
                at: call.started,
                seconds: (now() - call.started).max(0.0),
                outcome,
                detail: Some(CallDetail { arguments: call.arguments.clone(), result: slim(&result) }),
            });
            self.dirty = true;
        }
        let _ = call.answer.send(result);
    }

    /// A command that got no answer in time, or whose requester has gone away. The program
    /// finds out (`cancel`): if the command is still waiting in the queue it does not run
    /// it, if it is halfway it can interrupt it. Otherwise an agent retrying after a timeout
    /// would get the same script run twice.
    pub fn abandon(&mut self, routed: &str, why: &str, outcome: &'static str) {
        if let Some(call) = self.calls.get(routed)
            && let Some(app) = self.apps.get(&call.conn_id)
        {
            app.send(json!({ "type": "cancel", "call_id": routed, "reason": outcome }));
        }
        self.complete(routed, failure(why), outcome);
    }

    /// A command's outcome, from the program that received it and nobody else.
    pub fn result(&mut self, conn_id: &str, message: &Map<String, Value>) -> Result<(), crate::wire::Malformed> {
        let routed = crate::wire::key(Some(crate::wire::required(message, "call_id")?))?;
        let Some(routed) = routed else { return Ok(()) };
        if self.calls.get(routed).is_none_or(|c| c.conn_id != conn_id) {
            return Ok(());
        }
        let failed = json::truthy(message.get("is_error"));
        let routed = routed.to_owned();
        self.complete(
            &routed,
            json!({ "is_error": failed, "content": message.get("content").cloned().unwrap_or(Value::Null) }),
            if failed { outcome::ERROR } else { outcome::OK },
        );
        Ok(())
    }

    /// The agent and folder of whoever launched a command in progress.
    pub fn caller_of(&self, routed: &str) -> Option<(AgentIdentity, Option<String>)> {
        let call = self.calls.get(routed)?;
        let session = self.sessions.get(&call.session_key)?;
        Some((session.agent.clone(), session.cwd.clone()))
    }

    fn progress_of(&self, routed: &str) -> Progress {
        self.calls.get(routed).map(|c| c.progress.clone()).unwrap_or_default()
    }
}

/// An error outcome, as whoever was waiting for the answer receives it.
pub fn failure(content: &str) -> Value {
    json!({ "is_error": true, "content": content })
}

/// The requester has gone away: there is nobody left to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gone;

struct Inner {
    core: Mutex<Core>,
    options: HubOptions,
    stop: CancellationToken,
}

/// The hub, shared among channels, connectors, APIs and the app. Cheap to clone.
#[derive(Clone)]
pub struct Broker(Arc<Inner>);

impl Broker {
    pub fn new(
        control: String,
        options: HubOptions,
        permissions: AppPermissions,
        sink: Option<mpsc::UnboundedSender<String>>,
        stop: CancellationToken,
    ) -> Broker {
        let core = Core {
            control,
            ws_port: 0,
            started: now(),
            token: clak_wire::json::random_hex(16),
            can_ask: options.consent.is_some(),
            denial_memory: options.denial_memory,
            permissions,
            apps: IndexMap::new(),
            sessions: IndexMap::new(),
            grants: IndexMap::new(),
            calls: HashMap::new(),
            pending: IndexMap::new(),
            denied: HashMap::new(),
            confirms: IndexMap::new(),
            monitors: IndexMap::new(),
            events: VecDeque::new(),
            dirty: false,
            sink,
            connectors: json!([]),
            apis: json!([]),
        };
        Broker(Arc::new(Inner { core: Mutex::new(core), options, stop }))
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        // A panic under the lock must not stop the whole hub: the state stays consistent
        // because every step leaves it so before whatever can fail.
        self.0.core.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One whole hub step. At the end, if the state changed, watchers receive a snapshot.
    pub fn run<T>(&self, step: impl FnOnce(&mut Core) -> T) -> T {
        let mut core = self.lock();
        let result = step(&mut core);
        core.publish();
        result
    }

    pub fn options(&self) -> &HubOptions {
        &self.0.options
    }

    /// Cancelled when the hub stops.
    pub fn stopping(&self) -> CancellationToken {
        self.0.stop.clone()
    }

    /// The token, compared in constant time: it is what stops a web page from connecting on
    /// its own.
    pub fn token_matches(&self, offered: Option<&Value>) -> bool {
        let Some(text) = offered.and_then(Value::as_str) else { return false };
        let token = self.run(|c| c.token.clone());
        let (a, b) = (text.as_bytes(), token.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    // =====================================================================
    // Questions to the user
    // =====================================================================

    /// Asks the user, only once per question: whoever arrives while the window is open waits
    /// for the same answer. A «no» holds for a while without asking again. If the user
    /// allows, `allow` is applied at once, inside the lock, even if the asker has gone away
    /// meanwhile. `build` composes the question under the lock: it must not take it again.
    pub async fn consent(
        &self,
        key: &str,
        build: impl FnOnce(String, &Core) -> ConsentRequest,
        allow: impl FnOnce(&mut Core) + Send + 'static,
        gone: &CancellationToken,
    ) -> Result<Verdict, Gone> {
        #[allow(clippy::large_enum_variant)]
        enum Start {
            Now(Verdict),
            Wait { rx: watch::Receiver<Option<Verdict>>, ask: Option<(ConsentRequest, CancellationToken)>, id: String },
        }
        let allow: Allow = Box::new(allow);
        let start = self.run(|c| {
            if c.denied.get(key).is_some_and(|until| *until > Instant::now()) {
                return Start::Now(Verdict::Denied);
            }
            if !c.can_ask {
                c.log("consent is needed, but this hub has no interface to ask for it");
                return Start::Now(Verdict::NoPrompt);
            }
            let mut ask = None;
            if !c.pending.contains_key(key) {
                let request = build(new_id("ask"), c);
                let (done, _) = watch::channel(None);
                let cancel = CancellationToken::new();
                c.log(format!("asking the user: {}", request.title));
                ask = Some((request.clone(), cancel.clone()));
                c.pending.insert(key.to_owned(), PendingConsent { request, allow: Some(allow), done, cancel, waiters: 0 });
                c.touch();
            }
            let pending = c.pending.get_mut(key).expect("inserted above");
            pending.waiters += 1;
            Start::Wait { rx: pending.done.subscribe(), ask, id: pending.request.id.clone() }
        });
        let (mut rx, ask, id) = match start {
            Start::Now(verdict) => return Ok(verdict),
            Start::Wait { rx, ask, id } => (rx, ask, id),
        };
        if let Some((request, cancel)) = ask {
            self.spawn_prompt(key.to_owned(), request, cancel);
        }
        tokio::select! {
            verdict = rx.wait_for(Option::is_some) => Ok(verdict.ok().and_then(|v| *v).unwrap_or(Verdict::NoAnswer)),
            _ = gone.cancelled() => {
                // If nobody is waiting any more, the window is withdrawn.
                let abandon = self.run(|c| match c.pending.get_mut(key) {
                    Some(p) if p.request.id == id => {
                        p.waiters -= 1;
                        (p.waiters == 0).then(|| p.cancel.clone())
                    }
                    _ => None,
                });
                if let Some(cancel) = abandon {
                    cancel.cancel();
                }
                Err(Gone)
            }
        }
    }

    fn spawn_prompt(&self, key: String, request: ConsentRequest, cancel: CancellationToken) {
        let broker = self.clone();
        let prompt = self.0.options.consent.clone().expect("can_ask checked");
        let timeout = self.0.options.consent_timeout;
        tokio::spawn(async move {
            let limit = cancel.clone();
            let timer = tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                limit.cancel();
            });
            let title = request.title.clone();
            let answer = tokio::select! {
                answer = prompt.ask(request, cancel.clone()) => Some(answer),
                _ = cancel.cancelled() => None,
            };
            timer.abort();
            let verdict = verdict_of(&broker, answer, &cancel);
            let done = broker.run(|c| {
                let mut pending = c.pending.shift_remove(&key);
                match verdict {
                    Verdict::Allowed => {
                        c.log(format!("allowed: {title}"));
                        if let Some(allow) = pending.as_mut().and_then(|p| p.allow.take()) {
                            allow(c);
                        }
                    }
                    Verdict::Denied => {
                        c.log(format!("denied: {title}"));
                        let until = Instant::now() + c.denial_memory;
                        c.denied.insert(key.clone(), until);
                    }
                    _ => c.log(format!("no answer: {title}")),
                }
                c.touch();
                pending.map(|p| p.done)
            });
            if let Some(done) = done {
                let _ = done.send(Some(verdict));
            }
        });
    }

    /// A question that holds for a single command: the confirmation an MCP server asks for
    /// halfway (elicitation), or a settings change proposed by an agent. Neither yes nor no
    /// is remembered. `build` receives the id, the agent that launched the command, its
    /// folder and the state: it runs under the lock, and must not take it again.
    pub async fn confirm(
        &self,
        conn_id: &str,
        routed: &str,
        build: impl FnOnce(String, Option<&AgentIdentity>, Option<&str>, &Core) -> ConsentRequest,
        cancel: &CancellationToken,
    ) -> Verdict {
        let started = self.run(|c| {
            if !c.can_ask {
                return None;
            }
            let caller = c.caller_of(routed);
            let asked = build(new_id("ask"), caller.as_ref().map(|(a, _)| a), caller.as_ref().and_then(|(_, cwd)| cwd.as_deref()), c);
            c.confirms.insert(asked.id.clone(), asked.clone());
            c.log(format!("asking the user: {}", asked.title));
            c.touch();
            let (is_hub, name) = c.app_by_id(conn_id).map(|a| (a.is_hub(), a.name.clone())).unwrap_or((false, String::new()));
            Some((asked, c.progress_of(routed), is_hub, name))
        });
        let Some((request, progress, is_hub, name)) = started else {
            return Verdict::NoPrompt;
        };
        progress.tell(
            "consent",
            &if is_hub {
                "waiting for the user: Clak is asking them, in its own window, to confirm this change…".to_owned()
            } else {
                format!("waiting for the user: “{name}” is asking them to confirm, in a Clak window…")
            },
        );

        let limit = CancellationToken::new();
        let (watch_cancel, watch_stop, watch_limit) = (cancel.clone(), self.stopping(), limit.clone());
        let timeout = self.0.options.consent_timeout;
        let guard = tokio::spawn(async move {
            tokio::select! {
                _ = watch_cancel.cancelled() => {}
                _ = watch_stop.cancelled() => {}
                _ = tokio::time::sleep(timeout) => {}
            }
            watch_limit.cancel();
        });
        let prompt = self.0.options.consent.clone().expect("can_ask checked");
        let id = request.id.clone();
        let title = request.title.clone();
        let answer = tokio::select! {
            answer = prompt.ask(request, limit.clone()) => Some(answer),
            _ = limit.cancelled() => None,
        };
        guard.abort();
        let verdict = verdict_of(self, answer, &limit);
        self.run(|c| {
            c.confirms.shift_remove(&id);
            let word = match verdict {
                Verdict::Allowed => "confirmed",
                Verdict::Denied => "canceled",
                _ => "no answer",
            };
            c.log(format!("{word}: {title}"));
            c.touch();
        });
        progress.tell(
            if verdict == Verdict::Allowed { "allowed" } else { "refused" },
            match verdict {
                Verdict::Allowed if is_hub => "confirmed: applying the change.",
                Verdict::Allowed => "confirmed: the command continues.",
                Verdict::Denied => "the user canceled: the command stops.",
                _ => "no answer to the confirmation: the command stops.",
            },
        );
        verdict
    }
}

/// What a window's answer means. A «no» that came because the question was withdrawn (the
/// asker went away, or time ran out) is not a «no» from the user.
fn verdict_of(broker: &Broker, answer: Option<Result<bool, String>>, cancel: &CancellationToken) -> Verdict {
    match answer {
        Some(Ok(true)) => Verdict::Allowed,
        Some(Ok(false)) if !cancel.is_cancelled() => Verdict::Denied,
        Some(Err(problem)) => {
            broker.run(|c| c.log(format!("the question couldn't be shown: {problem}")));
            Verdict::NoAnswer
        }
        _ => Verdict::NoAnswer,
    }
}

/// The questions for the user, already composed: used by whoever builds the requests.
pub use texts::{app as app_request, change as change_request, confirm as confirm_request};
