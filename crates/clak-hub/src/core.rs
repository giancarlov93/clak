//! The hub: who is connected, who has permission for what, and the only place where
//! permission is enforced. It never interprets the content of a command.
//!
//! Three registries and two rules. Programs connect (and the user allows it once per
//! executable); agents connect (and the user allows it once per process). An agent uses a
//! program only if the user lets it, for its session: never for good; and it runs a command
//! without asking only if the user approved that command as it is (`commands`), or allowed
//! it for the agent's session. A command that is new, or changed since, asks; a program the
//! user blocked is closed to the agent.
//!
//! All state lives in [`Core`], under a single lock taken by [`Broker::run`]: nothing waits
//! in there. The waits (a question to the user, a program's answer) are outside, in
//! [`Broker::consent`], [`Broker::confirm`] and in whoever waits for an answer.

use crate::commands::{CommandCache, Status};
use crate::consent::{
    AgentIdentity, Asks, ConsentRequest, Elicitation, Elicited, HubOptions, Reply, Verdict,
};
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
        self.tell(
            "consent",
            &format!("waiting for the user: Clak is asking them, in its own window, {what}…"),
        );
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
    /// the user renames it ([`Core::rename`], [`Core::set_suffix`]).
    pub name: String,
    /// What the name starts from: the program's command, before «-2» or the user's suffix.
    pub base: String,
    /// The suffix the user chose for this opening («revit-test»), instead of «-2».
    pub suffix: Option<String>,
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
    /// The key of its application in `apps.json` (executable and signature), for a program
    /// that connected from a process.
    pub key: Option<String>,
    /// All the commands it declared, also those not active now (see [`Core::observe`]).
    manifest: Vec<ToolDef>,
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
            let media = matches!(
                map.get("type").and_then(Value::as_str),
                Some("image" | "audio")
            );
            Value::Object(
                map.iter()
                    .map(|(k, v)| match (media, k.as_str(), v) {
                        (true, "data", Value::String(data)) => (
                            k.clone(),
                            Value::String(format!("[{} bytes, base64]", data.len())),
                        ),
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

/// An agent's permission on an instance of a program: there since the agent may use the
/// program, with the commands allowed for its session verbatim.
pub struct GrantEntry {
    pub id: String,
    /// The agent's key (pid and start time).
    pub session_key: String,
    pub app_id: String,
    /// Name → digest: the commands the user allowed for the agent's session, not approved
    /// in Clak. A changed command is no longer the allowed one.
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

/// What an answer does, under the lock, even if the asker has gone away meanwhile: applied
/// for a yes and for a block, with the user's reply.
pub type Allow = Box<dyn FnOnce(&mut Core, Reply) + Send>;

/// The command a question is about, so that one answer can settle the others it covers
/// (see [`Core::settle_calls`]).
#[derive(Debug, Clone)]
pub struct CallTarget {
    pub session_key: String,
    pub conn_id: String,
    pub tool: String,
    pub digest: String,
    /// Where the program's commands are in the cache, if they are.
    pub cache_key: Option<String>,
}

impl CallTarget {
    /// Where a no to any call of this command is remembered: an agent that changes the
    /// arguments doesn't get a new window at once.
    pub fn command_key(&self) -> String {
        format!(
            "command:{}:{}:{}",
            self.session_key, self.conn_id, self.tool
        )
    }
}

/// An agent may no longer use a program, for the rest of its session: the user blocked it.
struct Block {
    id: String,
    session_key: String,
    /// Who, to tell when it has gone without having been authorized.
    agent: AgentIdentity,
    target: ProgramRef,
    /// The program, as the user knows it.
    label: String,
    since: f64,
}

/// A program as a permission names it (to use it, or a block): all the instances of a known
/// application, a server or an API by its id (which a rename doesn't change), or the
/// instance of a program nobody could identify.
#[derive(Debug, Clone, PartialEq)]
enum ProgramRef {
    App(String),
    Service(String),
    Conn(String),
}

impl ProgramRef {
    fn of(app: &AppConn) -> ProgramRef {
        match &app.key {
            Some(key) => ProgramRef::App(key.clone()),
            None if app.is_connector() || app.is_hub() => {
                ProgramRef::Service(app.resource.instance_id.clone())
            }
            None => ProgramRef::Conn(app.conn_id.clone()),
        }
    }

    fn covers(&self, app: &AppConn) -> bool {
        match self {
            ProgramRef::App(key) => app.key.as_deref() == Some(key.as_str()),
            ProgramRef::Service(id) => {
                app.key.is_none()
                    && (app.is_connector() || app.is_hub())
                    && app.resource.instance_id == *id
            }
            ProgramRef::Conn(id) => app.conn_id == *id,
        }
    }
}

/// How long "only this call" waits for the agent to ask again, when the questions are on a
/// web page and the agent didn't wait for the answer.
const PERMIT_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// What an agent's question came to: the user's answer or, when the questions are on a web
/// page ([`HubOptions::question_link`]), the link where the user answers it. The agent is
/// told at once, and asks again once the user has answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    Verdict(Verdict),
    Link(String),
}

/// A question to the user in progress. Whoever asks the same thing waits for the same answer.
struct PendingConsent {
    request: ConsentRequest,
    allow: Option<Allow>,
    /// A command's question: another answer may settle it.
    call: Option<CallTarget>,
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

/// The longest name the user can give an application.
const MAX_APP_LABEL: usize = 64;
const GONE_APP: &str = "This application is no longer among the allowed ones.";

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
    /// The programs the user blocked, by id.
    blocks: IndexMap<String, Block>,
    /// The programs each agent may use, by its key: the user said yes, for its session.
    uses: Vec<(String, ProgramRef)>,
    /// "Only this call", answered on the web page: the call it was for, until the agent asks
    /// again.
    permits: HashMap<String, Instant>,
    confirms: IndexMap<String, ConsentRequest>,
    monitors: IndexMap<String, Outbox>,
    events: VecDeque<Value>,
    dirty: bool,
    /// The programs' commands, and which ones the user approved.
    pub commands: CommandCache,
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
                "base": a.base,
                "suffix": a.suffix,
                "resource": a.resource.to_json(),
                "origin": a.origin,
                "key": a.key,
                "status": a.status,
                "since": float(a.since),
                "tools": a.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
                "cache": self.cache_key(a).map(|(key, ..)| key),
                "health": self.health(&a.conn_id),
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
            "blocks": self.blocks.values().map(|b| json!({
                "id": b.id,
                "session_id": self.sessions.get(&b.session_key).map(|s| s.id.clone()),
                "agent": b.agent.name,
                "pid": b.agent.pid,
                "program": b.label,
                "since": float(b.since),
            })).collect::<Vec<_>>(),
            "pending": self.pending.values().map(|p| &p.request).chain(self.confirms.values()).map(|r| json!({
                "id": r.id,
                "kind": r.kind.as_str(),
                "title": r.title,
            })).collect::<Vec<_>>(),
            "apps": self.permissions.all().iter().map(|e| json!({
                "key": e.key, "exe": e.exe, "name": e.name, "signer": e.signer, "since": float(e.since),
                "label": e.label, "command": e.command,
            })).collect::<Vec<_>>(),
            "connectors": self.connectors,
            "apis": self.apis,
            "commands": self.commands.summary(),
            "commands_error": self.commands.error(),
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
        outbox.send(
            json!({ "type": "log", "items": self.events.iter().cloned().collect::<Vec<_>>() }),
        );
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

    /// The command a program's names start from: the one chosen by the user, or the one
    /// derived from the program's name.
    fn base_for(resource: &ResourceDef, preferred: Option<&str>) -> String {
        match preferred {
            Some(p) if !p.is_empty() && cli_names::problem(Some(p)).is_none() => p.to_owned(),
            _ => cli_names::from(&resource.app),
        }
    }

    /// Whether another program already has this name.
    fn taken(&self, name: &str, own: Option<&str>) -> bool {
        self.apps
            .values()
            .any(|a| Some(a.conn_id.as_str()) != own && a.name == name)
    }

    /// The command-line name: the base and, for this opening, the suffix the user chose, if
    /// it is still free. Otherwise two documents of the same program become «word» and
    /// «word-2».
    fn name_for(
        &self,
        resource: &ResourceDef,
        preferred: Option<&str>,
        own: Option<&str>,
    ) -> String {
        let slug = Self::base_for(resource, preferred);
        if let Some(suffix) = own
            .and_then(|id| self.apps.get(id))
            .and_then(|a| a.suffix.as_deref())
        {
            let chosen = format!("{slug}-{suffix}");
            if cli_names::problem(Some(&chosen)).is_none() && !self.taken(&chosen, own) {
                return chosen;
            }
        }
        let mut name = slug.clone();
        let mut n = 2;
        while self.taken(&name, own) {
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
        let name = if origin.get("kind").and_then(Value::as_str) == Some("hub")
            && self.app(cli_names::SETTINGS).is_none()
        {
            cli_names::SETTINGS.to_owned()
        } else {
            self.name_for(&resource, preferred, None)
        };
        let base = if name == cli_names::SETTINGS {
            name.clone()
        } else {
            Self::base_for(&resource, preferred)
        };
        let conn_id = new_id("app");
        let count = tools.len();
        let app = AppConn {
            conn_id: conn_id.clone(),
            name,
            base,
            suffix: None,
            resource,
            origin,
            tools,
            since: now(),
            status: None,
            instructions: None,
            key: None,
            manifest: Vec::new(),
            outbox,
        };
        let line = format!(
            "program connected: {} = {} [{}], {count} {}",
            app.name,
            app.resource.label(),
            origin_label(&app.origin),
            if count == 1 { "command" } else { "commands" }
        );
        app.send(
            json!({ "type": "registered", "name": app.name, "protocol": clak_wire::PROTOCOL }),
        );
        let connector = app.is_connector();
        let tools = app.tools.clone();
        self.apps.insert(conn_id.clone(), app);
        self.log(line);
        if connector {
            self.observe(&conn_id, &tools);
        }
        self.touch();
        conn_id
    }

    /// Where a program's commands are kept in the cache: an application by its key (all its
    /// instances), a server or an API by its id, a program nobody identified by its
    /// connection; Clak's settings nowhere. With its kind, its name for the user and, for a
    /// server or an API, where it is.
    pub fn cache_key(
        &self,
        app: &AppConn,
    ) -> Option<(String, &'static str, String, Option<String>)> {
        if app.is_hub() {
            return None;
        }
        if let Some(key) = &app.key {
            let label = self
                .permissions
                .get(key)
                .map(|e| e.label.clone().unwrap_or_else(|| e.name.clone()))
                .unwrap_or_else(|| app.resource.app.clone());
            return Some((format!("app:{key}"), "app", label, None));
        }
        if app.is_connector() {
            let kind = if app.origin.get("protocol").and_then(Value::as_str) == Some("rest") {
                "api"
            } else {
                "mcp"
            };
            let origin = app
                .origin
                .get("url")
                .or_else(|| app.origin.get("command"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            return Some((
                format!("{kind}:{}", app.resource.instance_id),
                kind,
                app.resource.app.clone(),
                origin,
            ));
        }
        Some((
            format!("conn:{}", app.conn_id),
            "conn",
            app.name.clone(),
            None,
        ))
    }

    /// The program offers these commands (all of them, also those not active now): the
    /// cache takes them in. An application updated without a signature (a new key, the same
    /// executable and name) first takes the approvals of the old one, for the same versions.
    pub fn observe(&mut self, conn_id: &str, manifest: &[ToolDef]) {
        let Some(app) = self.apps.get(conn_id) else {
            return;
        };
        let Some((key, kind, label, origin)) = self.cache_key(app) else {
            return;
        };
        // What the other instances of the same program offer: not missing.
        let elsewhere: Vec<String> = self
            .apps
            .values()
            .filter(|a| a.conn_id != conn_id)
            .filter(|a| self.cache_key(a).is_some_and(|(k, ..)| k == key))
            .flat_map(|a| a.manifest.iter().map(|t| t.name.clone()))
            .collect();
        // Only between versions without a signature (known by their content): a new signer
        // is another publisher, and starts over.
        let unsigned = |key: &str| {
            key.rsplit('|')
                .next()
                .is_some_and(|i| i.starts_with("sha256:"))
        };
        if self.commands.get(&key).is_none()
            && let Some(entry) = app.key.as_deref().and_then(|k| self.permissions.get(k))
            && unsigned(&entry.key)
            && let Some(sibling) = self
                .permissions
                .all()
                .into_iter()
                .filter(|e| {
                    e.key != entry.key
                        && unsigned(&e.key)
                        && e.name == entry.name
                        && e.exe.to_lowercase() == entry.exe.to_lowercase()
                        && self.commands.get(&format!("app:{}", e.key)).is_some()
                })
                .max_by(|a, b| a.since.total_cmp(&b.since))
        {
            let from = format!("app:{}", sibling.key);
            self.commands.inherit(&from, &key);
        }
        let observed =
            self.commands
                .observe(&key, kind, &label, origin.as_deref(), manifest, &elsewhere);
        if let Some(app) = self.apps.get_mut(conn_id) {
            app.manifest = manifest.to_vec();
        }
        if !observed.new.is_empty() || !observed.changed.is_empty() {
            let mut line = format!("{label}: to review,");
            if !observed.new.is_empty() {
                line.push_str(&format!(" new {}", quoted(&observed.new)));
            }
            if !observed.changed.is_empty() {
                line.push_str(&format!(" changed {}", quoted(&observed.changed)));
            }
            self.log(line);
        }
        self.touch();
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

    /// The key of the program's application, once its permission is known.
    pub fn set_key(&mut self, conn_id: &str, key: Option<String>) {
        if let Some(app) = self.apps.get_mut(conn_id) {
            app.key = key;
        }
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
        let Some(app) = self.apps.get_mut(conn_id) else {
            return;
        };
        let before: Vec<String> = app.tools.iter().map(|t| t.name.clone()).collect();
        let after: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        let connector = app.is_connector();
        if connector {
            let manifest = tools.clone();
            app.tools = tools;
            self.observe(conn_id, &manifest);
        } else {
            app.tools = tools;
        }
        let Some(app) = self.apps.get(conn_id) else {
            return;
        };
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

    /// A program's live resource has a new title: shown to the user and to the agents from
    /// now on. Its name for the agents and its permissions stay as they are.
    pub fn retitle(&mut self, conn_id: &str, title: String) {
        let Some(app) = self.apps.get_mut(conn_id) else {
            return;
        };
        if app.resource.title == title {
            return;
        }
        let line = format!(
            "{}: now “{}”, was “{}”",
            app.name, title, app.resource.title
        );
        app.resource.title = title;
        self.log(line);
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
        app.base = Self::base_for(&resource, preferred);
        let before = std::mem::replace(&mut app.name, name);
        app.resource = resource;
        let line = if before == app.name {
            format!("program renamed: {} = {}", app.name, app.resource.label())
        } else {
            format!(
                "program renamed: {before} → {} = {}",
                app.name,
                app.resource.label()
            )
        };
        self.log(line);
        self.touch();
    }

    /// The user gave one opening of a program a suffix of their own, to tell it apart when
    /// talking to an agent: «revit-test» instead of «revit-2». `None` goes back to the
    /// automatic one. Its permissions stay; an agent still using the old name is told that
    /// program is not there, and rereads the list. The new name.
    pub fn set_suffix(&mut self, conn_id: &str, suffix: Option<&str>) -> Result<String, String> {
        let Some(app) = self.apps.get(conn_id) else {
            return Err("The program is no longer connected.".into());
        };
        if app.is_hub() {
            return Err("Clak's own settings keep their name.".into());
        }
        let suffix = suffix
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let (resource, base) = (app.resource.clone(), app.base.clone());
        let name = match &suffix {
            Some(suffix) => {
                let name = format!("{base}-{suffix}");
                if let Some(problem) = cli_names::problem(Some(&name)) {
                    return Err(problem);
                }
                if self.taken(&name, Some(conn_id)) {
                    return Err(format!("“{name}” is already another program's name."));
                }
                name
            }
            None => {
                self.apps.get_mut(conn_id).expect("checked above").suffix = None;
                self.name_for(&resource, Some(&base), Some(conn_id))
            }
        };
        let app = self.apps.get_mut(conn_id).expect("checked above");
        app.suffix = suffix;
        let before = std::mem::replace(&mut app.name, name.clone());
        if before != name {
            let line = format!(
                "program renamed: {before} → {name} = {}",
                app.resource.label()
            );
            self.log(line);
        }
        self.touch();
        Ok(name)
    }

    /// The program's connection has ended: its permissions end with it.
    pub fn app_closed(&mut self, conn_id: &str, reason: &str) {
        let Some(app) = self.apps.shift_remove(conn_id) else {
            return;
        };
        self.commands.forget(&format!("conn:{conn_id}"));
        // A server or an API closes when the user changes it (its token, its variables, its
        // address) or turns it off: who may use it starts over. An application's other
        // instances, and those it opens later, keep it.
        let program = ProgramRef::of(&app);
        if !matches!(program, ProgramRef::App(_)) {
            self.uses.retain(|(_, p)| *p != program);
        }
        self.grants.retain(|_, g| g.app_id != conn_id);
        self.blocks
            .retain(|_, b| b.target != ProgramRef::Conn(conn_id.to_owned()));
        let routed: Vec<String> = self
            .calls
            .iter()
            .filter(|(_, c)| c.conn_id == conn_id)
            .map(|(k, _)| k.clone())
            .collect();
        for id in routed {
            self.complete(
                &id,
                failure("the program was closed during the command"),
                outcome::CLOSED,
            );
        }
        self.log(format!(
            "program disconnected: {} = {} ({reason})",
            app.name,
            app.resource.label()
        ));
        self.touch();
    }

    /// The panel removes a program from the remembered ones. Whoever is already connected stays.
    pub fn forget_app(&mut self, key: Option<&str>) {
        if let Some(key) = key
            && self.permissions.forget(key)
        {
            // What the user approved of it goes too: allowed again, it is a new program. Its
            // instances still connected are seen afresh, so their commands can be decided.
            self.commands.forget(&format!("app:{key}"));
            let open: Vec<(String, Vec<ToolDef>)> = self
                .apps
                .values()
                .filter(|a| a.key.as_deref() == Some(key))
                .map(|a| (a.conn_id.clone(), a.manifest.clone()))
                .collect();
            for (conn_id, manifest) in open {
                self.observe(&conn_id, &manifest);
            }
            self.log("program removed from the allowed ones");
        }
        self.touch();
    }

    /// The user gave an allowed application a name or a command of their own (`None`: the
    /// program's own). Its open documents follow at once, as a renamed connector does.
    pub fn edit_app(
        &mut self,
        key: &str,
        label: Option<&str>,
        command: Option<&str>,
    ) -> Result<(), String> {
        let Some(entry) = self.permissions.get(key) else {
            return Err(GONE_APP.into());
        };
        let declared = entry.name.clone();
        let label = label
            .map(str::trim)
            .filter(|l| !l.is_empty() && *l != declared)
            .map(str::to_owned);
        if label
            .as_ref()
            .is_some_and(|l| l.chars().count() > MAX_APP_LABEL)
        {
            return Err(format!(
                "The name is too long ({MAX_APP_LABEL} characters at most)."
            ));
        }
        let command = command
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        if let Some(problem) = cli_names::problem(command.as_deref()) {
            return Err(problem);
        }
        if let Some(taken) = &command
            && self
                .permissions
                .all()
                .iter()
                .any(|e| e.key != key && e.command.as_ref() == Some(taken))
        {
            return Err(format!(
                "“{taken}” is already the command of another application."
            ));
        }
        self.permissions.edit(key, label.clone(), command.clone());
        let shown = label.unwrap_or(declared);
        let open: Vec<(String, ResourceDef)> = self
            .apps
            .values()
            .filter(|a| a.key.as_deref() == Some(key))
            .map(|a| {
                let mut resource = a.resource.clone();
                resource.app = shown.clone();
                (a.conn_id.clone(), resource)
            })
            .collect();
        if open.is_empty() {
            self.log(format!("application changed: {shown}"));
        }
        for (conn_id, resource) in open {
            self.rename(&conn_id, resource, command.as_deref());
        }
        self.touch();
        Ok(())
    }

    /// An allowed application has moved (see [`Broker::move_app`]): remembered at its new
    /// place under its new key, with its approved commands. Whoever is connected from the
    /// old place stays as it is.
    pub fn move_app(&mut self, key: &str, to: &str, origin: &Value) -> Result<(), String> {
        let Some(entry) = self.permissions.get(key) else {
            return Err(GONE_APP.into());
        };
        if to != key && self.permissions.get(to).is_some() {
            return Err("That program is already among the allowed applications.".into());
        }
        let shown = entry.label.clone().unwrap_or_else(|| entry.name.clone());
        self.permissions.moved(key, to, origin);
        self.commands
            .follow(&format!("app:{key}"), &format!("app:{to}"));
        let exe = origin.get("exe").and_then(Value::as_str).unwrap_or("");
        self.log(format!("application moved: {shown} → {exe}"));
        self.touch();
        Ok(())
    }

    /// An allowed application from a backup, its key already checked against the executable
    /// on this computer. A command another application already has is left out. False if it
    /// is already here.
    pub fn restore_app(&mut self, mut entry: crate::permissions::Entry) -> bool {
        if entry.command.as_ref().is_some_and(|taken| {
            cli_names::problem(Some(taken)).is_some()
                || self
                    .permissions
                    .all()
                    .iter()
                    .any(|e| e.command.as_ref() == Some(taken))
        }) {
            entry.command = None;
        }
        let shown = entry.label.clone().unwrap_or_else(|| entry.name.clone());
        let exe = entry.exe.clone();
        if !self.permissions.restore(entry) {
            return false;
        }
        self.log(format!("application restored: {shown} ({exe})"));
        self.touch();
        true
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
        let Some(key) = self
            .sessions
            .iter()
            .find(|(_, s)| Some(s.id.as_str()) == session_id)
            .map(|(k, _)| k.clone())
        else {
            return;
        };
        let session = self.sessions.shift_remove(&key).expect("found above");
        // A block outlives the session the user (or anyone on the channel) disconnects: it
        // ends with the agent's process, which the sweep watches.
        if reason == "harness_gone" {
            self.blocks.retain(|_, b| b.session_key != key);
        }
        self.uses.retain(|(session, _)| *session != key);
        self.permits
            .retain(|call, _| !call.starts_with(&format!("call:{key}:")));
        let ended: Vec<String> = self
            .grants
            .values()
            .filter(|g| g.session_key == key)
            .map(|g| g.id.clone())
            .collect();
        for grant in ended {
            self.end(&grant, reason, &session);
        }
        let why = if reason == "harness_gone" {
            "process ended"
        } else {
            reason
        };
        self.log(format!("agent disconnected: {} ({why})", session.label()));
        self.touch();
    }

    /// The authorized agents, to check outside the lock whether they are still alive.
    pub fn sessions(&self) -> Vec<(String, AgentIdentity)> {
        self.sessions
            .values()
            .map(|s| (s.id.clone(), s.agent.clone()))
            .collect()
    }

    // =====================================================================
    // Permissions
    // =====================================================================

    pub fn grant_of(&self, session_key: &str, conn_id: &str) -> Option<&GrantEntry> {
        self.grants
            .values()
            .find(|g| g.session_key == session_key && g.app_id == conn_id)
    }

    pub fn grant_by_id(&self, grant_id: &str) -> Option<&GrantEntry> {
        self.grants.get(grant_id)
    }

    pub fn grant_by_id_mut(&mut self, grant_id: &str) -> Option<&mut GrantEntry> {
        self.dirty = true;
        self.grants.get_mut(grant_id)
    }

    /// Whether the agent may use the program: the user said yes, for its session.
    pub fn may_use(&self, session_key: &str, app: &AppConn) -> bool {
        self.uses
            .iter()
            .any(|(session, program)| session == session_key && program.covers(app))
    }

    /// The user lets the agent use the program, for its session: all the instances of the
    /// application. The agent is connected if it isn't yet, and gets its permission on this
    /// instance. Returns the agent's key.
    pub fn allow_use(
        &mut self,
        agent: &AgentIdentity,
        cwd: Option<&str>,
        via: &str,
        conn_id: &str,
    ) -> Option<String> {
        let program = ProgramRef::of(self.apps.get(conn_id)?);
        let session = self.authorize(agent, cwd, via);
        if !self
            .uses
            .iter()
            .any(|(s, p)| *s == session && *p == program)
        {
            self.uses.push((session.clone(), program));
        }
        self.enter(&session, conn_id);
        Some(session)
    }

    /// The agent's permission on this instance of a program it may use, made if it isn't
    /// there: the program is told who uses it. Returns its id.
    pub fn enter(&mut self, session_key: &str, conn_id: &str) -> Option<String> {
        if let Some(known) = self.grant_of(session_key, conn_id) {
            return Some(known.id.clone());
        }
        let session = self.sessions.get(session_key)?;
        let app = self.apps.get(conn_id)?;
        let grant = GrantEntry {
            id: new_id("grant"),
            session_key: session_key.to_owned(),
            app_id: conn_id.to_owned(),
            approved: HashMap::new(),
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
        let line = format!("{label} uses {} ({})", app.name, app.resource.label());
        let id = grant.id.clone();
        self.grants.insert(id.clone(), grant);
        self.log(line);
        self.touch();
        Some(id)
    }

    /// The user allowed these commands, as they were, for the agent's session.
    pub fn allow_for_session(
        &mut self,
        session_key: &str,
        conn_id: &str,
        allowed: &[(String, String)],
    ) -> Option<String> {
        let id = self.enter(session_key, conn_id)?;
        let grant = self.grants.get_mut(&id)?;
        for (name, digest) in allowed {
            grant.approved.insert(name.clone(), digest.clone());
        }
        self.touch();
        Some(id)
    }

    /// Whether the command runs for this agent without asking, as it is now: approved in
    /// Clak, or allowed for the agent's session. Clak's settings: everything the session
    /// allowed.
    pub fn runs(&self, session_key: &str, conn_id: &str, tool: &ToolDef) -> bool {
        let Some(app) = self.apps.get(conn_id) else {
            return false;
        };
        let session = self
            .grant_of(session_key, conn_id)
            .is_some_and(|g| g.approves(tool));
        let approved = self.status_of(app, tool) == Status::Approved;
        session || approved
    }

    /// "Only this call", answered on the web page: the agent's next identical call runs once.
    pub fn permit(&mut self, call_key: &str) {
        self.permits
            .insert(call_key.to_owned(), Instant::now() + PERMIT_TTL);
    }

    /// The permit for this call, used up.
    pub fn take_permit(&mut self, call_key: &str) -> bool {
        let now = Instant::now();
        self.permits.retain(|_, until| *until > now);
        self.permits.remove(call_key).is_some()
    }

    /// The user blocked the program for the agent: its permission on every instance the
    /// block covers ends, and its next calls are refused without a question.
    pub fn block(&mut self, agent: &AgentIdentity, conn_id: &str) {
        let Some(app) = self.apps.get(conn_id) else {
            return;
        };
        let session_key = agent.key();
        let target = ProgramRef::of(app);
        let label = match &app.key {
            Some(_) => app.resource.app.clone(),
            None => app.name.clone(),
        };
        if self
            .blocks
            .values()
            .any(|b| b.session_key == session_key && b.target == target)
        {
            return;
        }
        let ended: Vec<String> = self
            .grants
            .values()
            .filter(|g| {
                g.session_key == session_key
                    && self.apps.get(&g.app_id).is_some_and(|a| target.covers(a))
            })
            .map(|g| g.id.clone())
            .collect();
        let who = self
            .sessions
            .get(&session_key)
            .map(SessionEntry::label)
            .unwrap_or_else(|| agent.name.clone());
        for id in ended {
            if let Some(grant) = self.grants.shift_remove(&id)
                && let Some(app) = self.apps.get(&grant.app_id)
            {
                app.send(json!({ "type": "revoked", "grant_id": grant.id, "reason": "blocked", "harness": who }));
            }
        }
        self.uses
            .retain(|(s, p)| !(*s == session_key && *p == target));
        let block = Block {
            id: new_id("block"),
            session_key,
            agent: agent.clone(),
            target,
            label: label.clone(),
            since: now(),
        };
        self.blocks.insert(block.id.clone(), block);
        self.log(format!("blocked: {who} can no longer use {label}"));
        self.touch();
    }

    /// Whether the user blocked the program for this agent.
    pub fn blocks(&self, session_key: &str, app: &AppConn) -> bool {
        self.blocks
            .values()
            .any(|b| b.session_key == session_key && b.target.covers(app))
    }

    /// The user lifts a block: the agent's next call asks again.
    pub fn unblock(&mut self, id: Option<&str>) {
        if let Some(block) = id.and_then(|id| self.blocks.shift_remove(id)) {
            self.log(format!(
                "unblocked: {} may ask to use {} again",
                block.agent.name, block.label
            ));
            self.touch();
        }
    }

    /// The blocks of agents that never connected: to check outside the lock whether they
    /// are still alive. Those of connected agents end with their session.
    pub fn loose_blocks(&self) -> Vec<(String, AgentIdentity)> {
        self.blocks
            .values()
            .filter(|b| !self.sessions.contains_key(&b.session_key))
            .map(|b| (b.id.clone(), b.agent.clone()))
            .collect()
    }

    pub fn drop_block(&mut self, id: &str) {
        if self.blocks.shift_remove(id).is_some() {
            self.touch();
        }
    }

    /// Another question open about a call of this agent on this program (not `except`): a
    /// way to wait for its answer. One window at a time per agent and program: an agent
    /// firing many calls doesn't bury the user under windows.
    pub fn open_call(
        &self,
        session_key: &str,
        conn_id: &str,
        except: &str,
    ) -> Option<watch::Receiver<Option<Verdict>>> {
        self.pending
            .iter()
            .find(|(key, p)| {
                key.as_str() != except
                    && p.call
                        .as_ref()
                        .is_some_and(|c| c.session_key == session_key && c.conn_id == conn_id)
            })
            .map(|(_, p)| p.done.subscribe())
    }

    /// After an answer about one of the agent's commands (or a command approved in the
    /// panel, for every agent: `None`): the other questions about calls that are now
    /// covered are settled without the user — allowed if the command now runs, refused if
    /// the program is now blocked — and their windows withdrawn.
    pub fn settle_calls(&mut self, session_key: Option<&str>) {
        let settled: Vec<(String, Verdict)> = self
            .pending
            .iter()
            .filter_map(|(key, p)| {
                let call = p
                    .call
                    .as_ref()
                    .filter(|c| session_key.is_none_or(|s| c.session_key == s))?;
                let app = self.apps.get(&call.conn_id)?;
                if self.blocks(&call.session_key, app) {
                    return Some((key.clone(), Verdict::Blocked));
                }
                let tool = app.tools.iter().find(|t| t.name == call.tool)?;
                (tool.digest == call.digest && self.runs(&call.session_key, &call.conn_id, tool))
                    .then(|| (key.clone(), Verdict::Allowed))
            })
            .collect();
        for (key, verdict) in settled {
            if let Some(pending) = self.pending.shift_remove(&key) {
                pending.cancel.cancel();
                let _ = pending.done.send(Some(verdict));
                self.touch();
            }
        }
    }

    fn end(&mut self, grant_id: &str, reason: &str, session: &SessionEntry) {
        let Some(grant) = self.grants.shift_remove(grant_id) else {
            return;
        };
        if let Some(app) = self.apps.get(&grant.app_id) {
            app.send(json!({
                "type": "revoked", "grant_id": grant.id, "reason": reason, "harness": session.label(),
            }));
        }
        self.touch();
    }

    /// Revocation from the panel: whoever looks at the panel is the user.
    pub fn revoke(&mut self, grant_id: Option<&str>) {
        let Some(grant) = grant_id.and_then(|id| self.grants.get(id)) else {
            return;
        };
        let (Some(session), Some(app)) = (
            self.sessions.get(&grant.session_key),
            self.apps.get(&grant.app_id),
        ) else {
            return;
        };
        let line = format!(
            "revoked: {} can no longer use {}",
            session.label(),
            app.name
        );
        let session_key = grant.session_key.clone();
        let label = session.label();
        // The agent may no longer use the program: on any of its instances, until it asks
        // again.
        let program = ProgramRef::of(app);
        self.uses
            .retain(|(s, p)| !(*s == session_key && *p == program));
        let ended: Vec<String> = self
            .grants
            .values()
            .filter(|g| {
                g.session_key == session_key
                    && self.apps.get(&g.app_id).is_some_and(|a| program.covers(a))
            })
            .map(|g| g.id.clone())
            .collect();
        self.log(line);
        for id in ended {
            if let Some(grant) = self.grants.shift_remove(&id) {
                let unused = format!("call:{}:{}:", grant.session_key, grant.app_id);
                self.permits.retain(|call, _| !call.starts_with(&unused));
                if let Some(app) = self.apps.get(&grant.app_id) {
                    app.send(json!({ "type": "revoked", "grant_id": grant.id, "reason": "user", "harness": label }));
                }
            }
        }
        self.touch();
    }

    // =====================================================================
    // The commands, as the user decides about them in the panel
    // =====================================================================

    /// A program's commands for the panel: state, what changed, when approved.
    pub fn panel_commands(&self, key: Option<&str>) -> Value {
        let mut reply = self.commands.panel(key.unwrap_or(""));
        reply["type"] = json!("commands");
        reply
    }

    /// The user sets a command's state in the panel: approved (as the user saw it,
    /// `digest`), asking every time, or off. Asking or off also takes it back from the
    /// agents that had it for their session. The questions it now answers are settled.
    pub fn set_command_state(
        &mut self,
        key: Option<&str>,
        name: Option<&str>,
        state: Option<&str>,
        digest: Option<&str>,
    ) {
        let (Some(key), Some(name), Some(state)) =
            (key, name, state.and_then(crate::commands::State::parse))
        else {
            return;
        };
        if !self.commands.set_state(key, name, state, digest) {
            return;
        }
        if state != crate::commands::State::Approved {
            self.take_back(key, Some(name));
        }
        let label = self
            .commands
            .get(key)
            .map(|p| p.label.clone())
            .unwrap_or_default();
        self.log(format!(
            "“{name}” of {label}: {} in the panel",
            state.as_str()
        ));
        self.settle_calls(None);
        self.touch();
    }

    /// The commands allowed for the session on the program's instances, taken back: one
    /// (`name`), or all of them.
    fn take_back(&mut self, key: &str, name: Option<&str>) {
        let conns: Vec<String> = self
            .apps
            .values()
            .filter(|a| self.cache_key(a).is_some_and(|(k, ..)| k == key))
            .map(|a| a.conn_id.clone())
            .collect();
        for grant in self
            .grants
            .values_mut()
            .filter(|g| conns.contains(&g.app_id))
        {
            match name {
                Some(name) => {
                    grant.approved.remove(name);
                }
                None => grant.approved.clear(),
            }
        }
    }

    /// The commands the user saw (`shown`: name and digest), approved in the panel.
    pub fn approve_all_commands(&mut self, key: Option<&str>, shown: &[(String, String)]) {
        let Some(key) = key else {
            return;
        };
        let count = self.commands.approve_all(key, "panel", Some(shown));
        let label = self
            .commands
            .get(key)
            .map(|p| p.label.clone())
            .unwrap_or_default();
        self.log(format!("{label}: {count} commands approved in the panel"));
        self.settle_calls(None);
        self.touch();
    }

    /// All the program's approvals go, and what agents had for their session: every
    /// command asks again.
    pub fn reset_commands(&mut self, key: Option<&str>) {
        if let Some(key) = key {
            self.commands.reset(key);
            self.take_back(key, None);
            self.log(format!("{key}: approvals reset in the panel"));
            self.touch();
        }
    }

    /// The commands the program no longer offers are forgotten. From the channel, not the
    /// ones turned off: forgotten, one would come back asking.
    pub fn forget_missing_commands(&mut self, key: Option<&str>, inside: bool) {
        if let Some(key) = key {
            self.commands.forget_missing(key, !inside);
            self.touch();
        }
    }

    // =====================================================================
    // What an agent sees
    // =====================================================================

    /// The connected programs as an agent sees them, each with how it may use it (`access`):
    /// `granted` (the user let it, for its session) or null (the first use asks the user);
    /// `blocked` if the user closed it to the agent (then `access` is null: an older `clak`
    /// takes it as one to ask about). The commands the user hasn't approved still ask.
    pub fn listing(&self, session_key: &str) -> Value {
        Value::Array(
            self.apps
                .values()
                .map(|a| {
                    let blocked = self.blocks(session_key, a);
                    let access = (!blocked && self.may_use(session_key, a)).then_some("granted");
                    json!({
                        "name": a.name,
                        "app": a.resource.app,
                        "title": a.resource.title,
                        "publisher": a.resource.publisher,
                        "origin": match &a.status {
                            Some(status) => format!("{} — {status}", origin_short(&a.origin)),
                            None => origin_short(&a.origin),
                        },
                        "tools": a.tools.iter().filter(|t| self.status_of(a, t) != Status::Off).count(),
                        "allowed": access == Some("granted"),
                        "access": access,
                        "blocked": blocked,
                    })
                })
                .collect(),
        )
    }

    /// The command as the user decided in Clak, for its version now. Clak's settings are
    /// Clak's own: approved.
    pub fn status_of(&self, app: &AppConn, tool: &ToolDef) -> Status {
        match self.cache_key(app) {
            Some((key, ..)) => self.commands.status(&key, tool),
            None => Status::Approved,
        }
    }

    /// A program's commands as the agent sees them, each with whether it runs without
    /// asking (`approved`) and how Clak knows it (`status`). The ones the user turned off
    /// aren't there.
    pub fn describe(
        &self,
        session_key: &str,
        conn_id: &str,
        query: Option<&str>,
    ) -> Option<Map<String, Value>> {
        let app = self.apps.get(conn_id)?;
        let access = self.may_use(session_key, app);
        let terms: Vec<String> = query
            .unwrap_or("")
            .split(' ')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_lowercase)
            .collect();
        let tools: Vec<Value> = app
            .tools
            .iter()
            .filter(|t| self.status_of(app, t) != Status::Off)
            .filter(|t| {
                let name = t.name.to_lowercase();
                let description = t.description.to_lowercase();
                terms
                    .iter()
                    .all(|term| name.contains(term) || description.contains(term))
            })
            .map(|t| {
                let mut item = t.to_json();
                // Whether it runs without a question once the agent may use the program.
                item["approved"] = Value::Bool(self.runs(session_key, conn_id, t));
                item["status"] = Value::String(self.status_of(app, t).as_str().into());
                item
            })
            .collect();
        let value = json!({
            "name": app.name,
            "app": app.resource.app,
            "title": app.resource.title,
            "allowed": access && !self.blocks(session_key, app),
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
            None => {
                json!({ "type": "tools", "resource_id": resource_id, "found": false, "tools": [] })
            }
        }
    }

    /// How a program has been answering, from its permissions' calls since they began: how
    /// many, how many failed, how long they took on average, the last one. Nothing if it
    /// hasn't run a command yet.
    fn health(&self, conn_id: &str) -> Value {
        let grants: Vec<&GrantEntry> = self
            .grants
            .values()
            .filter(|g| g.app_id == conn_id)
            .collect();
        let (calls, errors, seconds) = grants
            .iter()
            .flat_map(|g| g.stats.values())
            .fold((0, 0, 0.0), |(c, e, s), t| {
                (c + t.calls, e + t.errors, s + t.seconds)
            });
        let last = grants
            .iter()
            .filter_map(|g| g.history.back())
            .max_by(|a, b| a.at.total_cmp(&b.at));
        match last {
            Some(last) if calls > 0 => json!({
                "calls": calls,
                "errors": errors,
                "average": round3(seconds / calls as f64),
                "last_at": float(last.at),
                "last_outcome": last.outcome,
            }),
            _ => Value::Null,
        }
    }

    /// A command run by the user from the panel, not by an agent: no permission to check,
    /// the user is the one who gives them.
    pub fn begin_user_call(
        &mut self,
        conn_id: &str,
        tool: &str,
        arguments: Map<String, Value>,
    ) -> Option<(String, oneshot::Receiver<Value>)> {
        let app = self.apps.get(conn_id)?;
        let tool = app.tools.iter().find(|t| t.name == tool)?.clone();
        let routed = new_id("call");
        let (answer, receiver) = oneshot::channel();
        let arguments = Value::Object(arguments);
        let kept = slim(&arguments);
        app.send(json!({
            "type": "call",
            "call_id": routed,
            "tool": tool.name,
            "arguments": arguments,
            "harness": "You, from Clak's panel",
        }));
        self.log(format!("{} {}: tried from the panel", app.name, tool.name));
        self.calls.insert(
            routed.clone(),
            PendingCall {
                answer,
                conn_id: conn_id.to_owned(),
                session_key: String::new(),
                grant_id: None,
                tool: tool.name.clone(),
                arguments: kept,
                started: now(),
                progress: Progress::default(),
            },
        );
        self.touch();
        Some((routed, receiver))
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
            None => {
                json!({ "type": "history", "grant_id": grant_id, "found": false, "calls": [], "stats": [] })
            }
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
            None => {
                json!({ "type": "call", "grant_id": grant_id, "call_id": call_id, "found": false })
            }
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
        let session = self.sessions.get(&session_key);
        let label = session.map(SessionEntry::label).unwrap_or_default();
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
        let Some(call) = self.calls.remove(routed) else {
            return;
        };
        if let Some(grant) = call
            .grant_id
            .as_deref()
            .and_then(|id| self.grants.get_mut(id))
        {
            grant.record(CallRecord {
                id: routed.to_owned(),
                tool: call.tool.clone(),
                at: call.started,
                seconds: (now() - call.started).max(0.0),
                outcome,
                detail: Some(CallDetail {
                    arguments: call.arguments.clone(),
                    result: slim(&result),
                }),
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
    pub fn result(
        &mut self,
        conn_id: &str,
        message: &Map<String, Value>,
    ) -> Result<(), crate::wire::Malformed> {
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
        self.calls
            .get(routed)
            .map(|c| c.progress.clone())
            .unwrap_or_default()
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
            blocks: IndexMap::new(),
            uses: Vec::new(),
            permits: HashMap::new(),
            confirms: IndexMap::new(),
            monitors: IndexMap::new(),
            events: VecDeque::new(),
            dirty: false,
            commands: CommandCache::default(),
            sink,
            connectors: json!([]),
            apis: json!([]),
        };
        Broker(Arc::new(Inner {
            core: Mutex::new(core),
            options,
            stop,
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        // A panic under the lock must not stop the whole hub: the state stays consistent
        // because every step leaves it so before whatever can fail.
        self.0
            .core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    /// The user points an allowed application to where it has moved: its key is made again
    /// from that file, as when the program connects (the path, and the signer or the content).
    /// Its new key.
    pub async fn move_app(&self, key: &str, exe: String) -> Result<String, String> {
        let name = self
            .run(|c| c.permissions().get(key).map(|e| e.name.clone()))
            .ok_or(GONE_APP)?;
        let platform = self.options().platform.clone();
        let (origin, to) = tokio::task::spawn_blocking(move || {
            if !std::path::Path::new(&exe).is_file() {
                return Err("There is no program at that path.".to_owned());
            }
            let origin = platform.origin_of(&exe);
            let to = AppPermissions::key_for(platform.as_ref(), &origin, &name)
                .ok_or("That file can't be read.")?;
            Ok((origin, to))
        })
        .await
        .map_err(|e| e.to_string())??;
        self.run(|c| c.move_app(key, &to, &origin))?;
        Ok(to)
    }

    /// Cancelled when the hub stops.
    pub fn stopping(&self) -> CancellationToken {
        self.0.stop.clone()
    }

    /// The token, compared in constant time: it is what stops a web page from connecting on
    /// its own.
    pub fn token_matches(&self, offered: Option<&Value>) -> bool {
        let Some(text) = offered.and_then(Value::as_str) else {
            return false;
        };
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
        let allow: Allow = Box::new(move |c: &mut Core, reply: Reply| {
            if reply.allows() {
                allow(c);
            }
        });
        match self
            .consent_with(key, build, allow, None, gone, None)
            .await?
        {
            Asked::Verdict(verdict) => Ok(verdict),
            Asked::Link(_) => unreachable!("without a link the question is waited for"),
        }
    }

    /// As [`Broker::consent`], with the user's reply: the question may offer more than yes
    /// and no (a box to tick).
    pub async fn consent_reply(
        &self,
        key: &str,
        build: impl FnOnce(String, &Core) -> ConsentRequest,
        allow: impl FnOnce(&mut Core, Reply) + Send + 'static,
        gone: &CancellationToken,
    ) -> Result<Verdict, Gone> {
        match self
            .consent_with(key, build, Box::new(allow), None, gone, None)
            .await?
        {
            Asked::Verdict(verdict) => Ok(verdict),
            Asked::Link(_) => unreachable!("without a link the question is waited for"),
        }
    }

    /// An agent's question, as [`Broker::consent`]; `allow` also gets the user's reply (a
    /// command's question offers more than yes and no), and `call` says which command it is
    /// about. With the questions on a web page the agent doesn't wait for it: the link comes
    /// back at once, the question stays open even when the agent has gone, and the answer is
    /// applied when it comes. Asking again before the answer gives the same question, and
    /// the same link.
    pub async fn agent_consent(
        &self,
        key: &str,
        build: impl FnOnce(String, &Core) -> ConsentRequest,
        allow: impl FnOnce(&mut Core, Reply) + Send + 'static,
        call: Option<CallTarget>,
        gone: &CancellationToken,
    ) -> Result<Asked, Gone> {
        let link = self.0.options.question_link.clone();
        self.consent_with(key, build, Box::new(allow), call, gone, link)
            .await
    }

    async fn consent_with(
        &self,
        key: &str,
        build: impl FnOnce(String, &Core) -> ConsentRequest,
        allow: Allow,
        call: Option<CallTarget>,
        gone: &CancellationToken,
        link: Option<crate::consent::QuestionLink>,
    ) -> Result<Asked, Gone> {
        #[allow(clippy::large_enum_variant)]
        enum Start {
            Now(Verdict),
            Wait {
                rx: watch::Receiver<Option<Verdict>>,
                ask: Option<(ConsentRequest, CancellationToken)>,
                id: String,
            },
            Elsewhere {
                ask: Option<(ConsentRequest, CancellationToken)>,
                id: String,
            },
        }
        let command_key = call.as_ref().map(CallTarget::command_key);
        let start = self.run(|c| {
            let now = Instant::now();
            if [Some(key), command_key.as_deref()]
                .into_iter()
                .flatten()
                .any(|k| c.denied.get(k).is_some_and(|until| *until > now))
            {
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
                c.pending.insert(
                    key.to_owned(),
                    PendingConsent {
                        request,
                        allow: Some(allow),
                        call,
                        done,
                        cancel,
                        waiters: 0,
                    },
                );
                c.touch();
            }
            let pending = c.pending.get_mut(key).expect("inserted above");
            // Nobody waits for it: so nobody's going away withdraws it.
            if link.is_some() {
                return Start::Elsewhere {
                    ask,
                    id: pending.request.id.clone(),
                };
            }
            pending.waiters += 1;
            Start::Wait {
                rx: pending.done.subscribe(),
                ask,
                id: pending.request.id.clone(),
            }
        });
        let (mut rx, ask, id) = match start {
            Start::Now(verdict) => return Ok(Asked::Verdict(verdict)),
            Start::Elsewhere { ask, id } => {
                if let Some((request, cancel)) = ask {
                    self.spawn_prompt(key.to_owned(), request, cancel);
                }
                let link = link.expect("only with a link");
                return Ok(Asked::Link(link(&id)));
            }
            Start::Wait { rx, ask, id } => (rx, ask, id),
        };
        if let Some((request, cancel)) = ask {
            self.spawn_prompt(key.to_owned(), request, cancel);
        }
        tokio::select! {
            verdict = rx.wait_for(Option::is_some) => Ok(Asked::Verdict(verdict.ok().and_then(|v| *v).unwrap_or(Verdict::NoAnswer))),
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

    /// Runs a command for the user, from the panel (see [`Core::begin_user_call`]): what came
    /// back and how long it took, or why it couldn't run. Waits as long as an agent's command.
    pub async fn try_command(
        &self,
        conn_id: &str,
        tool: &str,
        arguments: Map<String, Value>,
    ) -> Value {
        let started = Instant::now();
        let Some((routed, answer)) = self.run(|c| c.begin_user_call(conn_id, tool, arguments))
        else {
            return json!({ "ok": false, "message": "The program or the command is no longer there." });
        };
        let limit = self.options().call_timeout;
        let result = match tokio::time::timeout(limit, answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => failure("the program was closed during the command"),
            Err(_) => {
                self.run(|c| c.abandon(&routed, "timed out", outcome::TIMEOUT));
                return json!({
                    "ok": false,
                    "message": format!("No answer within {} s.", limit.as_secs()),
                });
            }
        };
        json!({
            "ok": true,
            "is_error": json::truthy(result.get("is_error")),
            "content": result.get("content").cloned().unwrap_or(Value::Null),
            "seconds": round3(started.elapsed().as_secs_f64()),
        })
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
            let id = request.id.clone();
            let answer = tokio::select! {
                answer = prompt.ask(request, cancel.clone()) => Some(answer),
                _ = cancel.cancelled() => None,
            };
            timer.abort();
            let (verdict, reply) = verdict_of(&broker, answer, &cancel);
            let done = broker.run(|c| {
                // Another answer may have settled it already (see `settle_calls`).
                if c.pending.get(&key).is_none_or(|p| p.request.id != id) {
                    return None;
                }
                let mut pending = c.pending.shift_remove(&key)?;
                // Only a question that offers it can block.
                let offers_block = pending
                    .request
                    .choices
                    .as_ref()
                    .is_some_and(|c| c.block.is_some());
                let (verdict, reply) = match verdict {
                    Verdict::Blocked if !offers_block => (Verdict::Denied, Some(Reply::Deny)),
                    _ => (verdict, reply),
                };
                // Only what the question offered: whoever showed it may have sent more.
                let reply = reply
                    .map(|r| crate::consent::Choices::allowed(pending.request.choices.as_ref(), r));
                match (verdict, reply) {
                    (Verdict::Allowed | Verdict::Blocked, Some(reply)) => {
                        c.log(format!(
                            "{}: {title}",
                            if verdict == Verdict::Blocked {
                                "blocked"
                            } else {
                                "allowed"
                            }
                        ));
                        if let Some(allow) = pending.allow.take() {
                            allow(c, reply);
                        }
                    }
                    (Verdict::Denied, _) => {
                        c.log(format!("denied: {title}"));
                        // One per call asked (its arguments too), and one for any call of the
                        // command: the old ones go.
                        let now = Instant::now();
                        c.denied.retain(|_, until| *until > now);
                        c.denied.insert(key.clone(), now + c.denial_memory);
                        if let Some(call) = &pending.call {
                            c.denied.insert(call.command_key(), now + c.denial_memory);
                        }
                    }
                    _ => c.log(format!("no answer: {title}")),
                }
                c.touch();
                Some((pending, verdict))
            });
            if let Some((pending, verdict)) = done {
                let _ = pending.done.send(Some(verdict));
                if let Some(call) = &pending.call
                    && matches!(verdict, Verdict::Allowed | Verdict::Blocked)
                {
                    broker.run(|c| c.settle_calls(Some(&call.session_key)));
                }
            }
        });
    }

    /// A question that holds for a single command: a settings change proposed by an agent.
    /// Neither yes nor no is remembered. `build` receives the id, the agent that launched the
    /// command, its folder and the state: it runs under the lock, and must not take it again.
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
            let asked = build(
                new_id("ask"),
                caller.as_ref().map(|(a, _)| a),
                caller.as_ref().and_then(|(_, cwd)| cwd.as_deref()),
                c,
            );
            c.confirms.insert(asked.id.clone(), asked.clone());
            c.log(format!("asking the user: {}", asked.title));
            c.touch();
            let (is_hub, name) = c
                .app_by_id(conn_id)
                .map(|a| (a.is_hub(), a.name.clone()))
                .unwrap_or((false, String::new()));
            Some((asked, c.progress_of(routed), is_hub, name))
        });
        let Some((request, progress, is_hub, name)) = started else {
            return Verdict::NoPrompt;
        };
        let link = self
            .0
            .options
            .question_link
            .as_ref()
            .map(|link| link(&request.id));
        progress.tell(
            "consent",
            &match (is_hub, link) {
                (true, None) => "waiting for the user: Clak is asking them, in its own window, to confirm this change…".to_owned(),
                (false, None) => format!("waiting for the user: “{name}” is asking them to confirm, in a Clak window…"),
                (true, Some(link)) => format!("waiting for the user: Clak is asking them to confirm this change, at {link}…"),
                (false, Some(link)) => format!("waiting for the user: “{name}” is asking them to confirm, at {link}…"),
            },
        );

        let mut open = self.open_question(&request.id, cancel);
        let prompt = self.0.options.consent.clone().expect("can_ask checked");
        let title = request.title.clone();
        let asking = tokio::spawn(prompt.ask(request, open.limit.clone()));
        let answer = tokio::select! {
            answer = asking => Some(answer.unwrap_or_else(|e| Err(e.to_string()))),
            _ = open.limit.cancelled() => None,
        };
        let verdict = match verdict_of(self, answer, &open.limit).0 {
            Verdict::Blocked => Verdict::Denied,
            verdict => verdict,
        };
        open.close(
            match verdict {
                Verdict::Allowed => "confirmed",
                Verdict::Denied => "canceled",
                _ => "no answer",
            },
            &title,
        );
        progress.tell(
            if verdict == Verdict::Allowed {
                "allowed"
            } else {
                "refused"
            },
            match verdict {
                Verdict::Allowed if is_hub => "confirmed: applying the change.",
                Verdict::Allowed => "confirmed: the command continues.",
                Verdict::Denied => "the user canceled: the command stops.",
                _ => "no answer to the confirmation: the command stops.",
            },
        );
        verdict
    }

    /// A server's question halfway through a command (elicitation): a confirmation, a form to
    /// fill in, or a page to open. It holds for that command only, and nothing is remembered.
    /// `others` are commands the question may come from as well (a program on this computer
    /// can't say which of its commands asks): `build` gets the names of their agents. `done`
    /// ends the question when the server says the user has finished in the browser.
    pub async fn elicit(
        &self,
        conn_id: &str,
        routed: &str,
        others: &[String],
        build: impl FnOnce(String, Option<&AgentIdentity>, Option<&str>, &[String]) -> ConsentRequest,
        cancel: &CancellationToken,
        done: Option<&CancellationToken>,
    ) -> Elicitation {
        let started = self.run(|c| {
            if !c.can_ask {
                return None;
            }
            let caller = c.caller_of(routed);
            let mut names: Vec<String> = Vec::new();
            for other in others.iter().filter(|o| o.as_str() != routed) {
                if let Some((agent, _)) = c.caller_of(other)
                    && !names.contains(&agent.name)
                {
                    names.push(agent.name);
                }
            }
            let asked = build(
                new_id("ask"),
                caller.as_ref().map(|(a, _)| a),
                caller.as_ref().and_then(|(_, cwd)| cwd.as_deref()),
                &names,
            );
            c.confirms.insert(asked.id.clone(), asked.clone());
            c.log(format!("asking the user: {}", asked.title));
            c.touch();
            let name = c
                .app_by_id(conn_id)
                .map(|a| a.name.clone())
                .unwrap_or_default();
            Some((asked, c.progress_of(routed), name))
        });
        let Some((request, progress, name)) = started else {
            return Elicitation::NoPrompt;
        };
        let asks = request.asks.clone();
        let (form, page, wait) = match &asks {
            Some(Asks::Form(form)) if !form.fields.is_empty() => (Some(form.clone()), false, false),
            Some(Asks::Page(page)) => (None, true, page.wait),
            _ => (None, false, false),
        };
        let what = match (page, wait, form.is_some()) {
            (true, true, _) => "needs them to finish something in their browser",
            (true, false, _) => "asks them to open a page in their browser",
            (_, _, true) => "asks them to fill in a form",
            _ => "is asking them to confirm",
        };
        let link = self
            .0
            .options
            .question_link
            .as_ref()
            .map(|link| link(&request.id));
        progress.tell(
            "consent",
            &match link {
                None => format!("waiting for the user: “{name}” {what}, in a Clak window…"),
                Some(link) => format!("waiting for the user: “{name}” {what}, at {link}…"),
            },
        );

        let open = self.open_question(&request.id, cancel);
        let prompt = self.0.options.consent.clone().expect("can_ask checked");
        let title = request.title.clone();
        let asking = tokio::spawn(prompt.elicit(request, open.limit.clone()));
        let finished = async {
            match done {
                Some(done) => done.cancelled().await,
                None => std::future::pending().await,
            }
        };
        let answer = tokio::select! {
            answer = asking => Some(answer.unwrap_or_else(|e| Err(e.to_string()))),
            _ = open.limit.cancelled() => None,
            _ = finished => {
                // The interface withdraws the question: it no longer needs an answer.
                open.limit.cancel();
                return self.elicited(open, &title, progress, Elicitation::Completed, page, wait, form.is_some());
            }
        };
        let withdrawn = open.limit.is_cancelled();
        let outcome = match answer {
            Some(Ok(Elicited::Accepted(content))) => match &form {
                Some(form) => match form.check(&content) {
                    Ok(content) => Elicitation::Accepted(content),
                    Err(problem) => {
                        self.run(|c| c.log(format!("the form came back unfit: {problem}")));
                        Elicitation::Cancelled
                    }
                },
                None => Elicitation::Accepted(Map::new()),
            },
            Some(Ok(Elicited::Declined)) if !withdrawn => Elicitation::Declined,
            Some(Ok(Elicited::Cancelled)) if !withdrawn => Elicitation::Cancelled,
            Some(Err(problem)) => {
                self.run(|c| c.log(format!("the question couldn't be shown: {problem}")));
                Elicitation::NoAnswer
            }
            _ => Elicitation::NoAnswer,
        };
        self.elicited(open, &title, progress, outcome, page, wait, form.is_some())
    }

    /// A server's question has an outcome: the list, the log and the agent know.
    #[allow(clippy::too_many_arguments)]
    fn elicited(
        &self,
        mut open: OpenQuestion,
        title: &str,
        progress: Progress,
        outcome: Elicitation,
        page: bool,
        wait: bool,
        form: bool,
    ) -> Elicitation {
        let word = match &outcome {
            Elicitation::Accepted(_) if page && !wait => "page opened",
            Elicitation::Accepted(_) if form => "form sent",
            Elicitation::Accepted(_) => "confirmed",
            Elicitation::Completed => "finished in the browser",
            Elicitation::Declined => "declined",
            Elicitation::Cancelled => "closed",
            _ => "no answer",
        };
        open.close(word, title);
        let (stage, text) = match (&outcome, page, wait) {
            (Elicitation::Accepted(_), true, true) | (Elicitation::Completed, _, _) => (
                "allowed",
                "the user is done in the browser: the command runs again.",
            ),
            (Elicitation::Accepted(_), true, false) => (
                "allowed",
                "the user opened the page: the command continues.",
            ),
            (Elicitation::Accepted(_), _, _) if form => {
                ("allowed", "the user sent the form: the command continues.")
            }
            (Elicitation::Accepted(_), _, _) => ("allowed", "confirmed: the command continues."),
            (Elicitation::Declined, true, true) => {
                ("refused", "the user declined: the command stops.")
            }
            (Elicitation::Declined, _, _) if page || form => {
                ("refused", "the user declined: the server is told.")
            }
            (Elicitation::Declined, _, _) => ("refused", "the user canceled: the command stops."),
            (Elicitation::Cancelled, _, _) => (
                "refused",
                "the user closed the question without answering: the server is told.",
            ),
            _ => (
                "refused",
                "no answer to the confirmation: the command stops.",
            ),
        };
        progress.tell(stage, text);
        outcome
    }

    /// A question that holds for one command is open: its time limit runs, and it is in the
    /// list until [`OpenQuestion::close`]. The interface asks in a task of its own, with the
    /// limit: if whoever waits goes away first, it still sees the question withdrawn.
    fn open_question(&self, id: &str, cancel: &CancellationToken) -> OpenQuestion {
        let limit = CancellationToken::new();
        let (watch_cancel, watch_stop, watch_limit) =
            (cancel.clone(), self.stopping(), limit.clone());
        let timeout = self.0.options.consent_timeout;
        tokio::spawn(async move {
            tokio::select! {
                _ = watch_cancel.cancelled() => {}
                _ = watch_stop.cancelled() => {}
                _ = watch_limit.cancelled() => {}
                _ = tokio::time::sleep(timeout) => {}
            }
            watch_limit.cancel();
        });
        OpenQuestion {
            broker: self.clone(),
            id: id.to_owned(),
            limit,
            closed: false,
        }
    }
}

/// See [`Broker::open_question`]. Dropped before it is closed (whoever waited is gone), the
/// question is withdrawn and leaves the list.
struct OpenQuestion {
    broker: Broker,
    id: String,
    limit: CancellationToken,
    closed: bool,
}

impl OpenQuestion {
    fn close(&mut self, word: &str, title: &str) {
        self.closed = true;
        self.limit.cancel();
        self.broker.run(|c| {
            c.confirms.shift_remove(&self.id);
            c.log(format!("{word}: {title}"));
            c.touch();
        });
    }
}

impl Drop for OpenQuestion {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        self.limit.cancel();
        self.broker.run(|c| {
            if c.confirms.shift_remove(&self.id).is_some() {
                c.touch();
            }
        });
    }
}

/// What a window's answer means, and the reply when it is the user's. A «no» that came
/// because the question was withdrawn (the asker went away, time ran out, another answer
/// covered it) is not a «no» from the user.
fn verdict_of(
    broker: &Broker,
    answer: Option<Result<Reply, String>>,
    cancel: &CancellationToken,
) -> (Verdict, Option<Reply>) {
    match answer {
        Some(Ok(reply @ Reply::Allow { .. })) => (Verdict::Allowed, Some(reply)),
        Some(Ok(Reply::Block)) if !cancel.is_cancelled() => (Verdict::Blocked, Some(Reply::Block)),
        Some(Ok(_)) if !cancel.is_cancelled() => (Verdict::Denied, Some(Reply::Deny)),
        Some(Err(problem)) => {
            broker.run(|c| c.log(format!("the question couldn't be shown: {problem}")));
            (Verdict::NoAnswer, None)
        }
        _ => (Verdict::NoAnswer, None),
    }
}

/// The questions for the user, already composed: used by whoever builds the requests.
pub use texts::{app as app_request, change as change_request, elicit as elicit_request};
