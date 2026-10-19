//! Questions to the user and how the hub behaves: who shows them, how long it waits, and
//! what the hub asks the operating system (who is behind a process).

use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// What the user is asked permission for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentKind {
    /// An agent wants to connect to the hub: to see which programs are there.
    Session,
    /// An agent wants to use a connected program.
    Use,
    /// A program has a new command, or one changed after consent.
    Tool,
    /// A program wants to connect to the hub.
    App,
    /// An MCP server, halfway through a command, asks for a confirmation (elicitation). Holds only for that command.
    Confirm,
    /// An agent proposes a change to the settings (`clak settings …`). Holds only for that change.
    Change,
}

impl ConsentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConsentKind::Session => "session",
            ConsentKind::Use => "use",
            ConsentKind::Tool => "tool",
            ConsentKind::App => "app",
            ConsentKind::Confirm => "confirm",
            ConsentKind::Change => "change",
        }
    }
}

/// One line of the request, already in words: «Executable» → «C:\…\claude.exe».
#[derive(Debug, Clone, PartialEq)]
pub struct ConsentLine {
    pub label: String,
    pub value: String,
}

impl ConsentLine {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        ConsentLine {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// A command as the agent will see it. `note` says whether it is new or changed.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsentTool {
    pub name: String,
    pub description: String,
    pub note: Option<String>,
}

/// A question for the user. The hub composes it, not whoever shows it: so the words are the
/// same in every interface and can be tested.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsentRequest {
    pub id: String,
    pub kind: ConsentKind,
    pub title: String,
    pub question: String,
    /// Who is asking, according to the operating system.
    pub who: Vec<ConsentLine>,
    /// What is being asked.
    pub what: Vec<ConsentLine>,
    pub tools: Vec<ConsentTool>,
    pub warning: Option<String>,
    /// How long the permission lasts, if given.
    pub note: String,
}

impl ConsentRequest {
    /// The whole question for an interface: the window shows it as it is.
    pub fn to_json(&self) -> Value {
        let lines = |list: &[ConsentLine]| -> Value {
            list.iter()
                .map(|l| json!({ "label": l.label, "value": l.value }))
                .collect()
        };
        json!({
            "id": self.id,
            "kind": self.kind.as_str(),
            "title": self.title,
            "question": self.question,
            "who": lines(&self.who),
            "what": lines(&self.what),
            "tools": self.tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "note": t.note })).collect::<Vec<_>>(),
            "warning": self.warning,
            "note": self.note,
        })
    }
}

/// A change to the settings proposed by an agent, as the user reads it.
#[derive(Debug, Clone, Default)]
pub struct SettingsChange {
    pub title: String,
    /// «add the MCP server Notion»: completes «Allow Claude Code to …?».
    pub action: String,
    pub lines: Vec<ConsentLine>,
    pub tools: Vec<ConsentTool>,
    pub warning: Option<String>,
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Who shows the questions to the user. In the app it is a window; a hub without an
/// interface has none, and then no permission is given.
pub trait ConsentPrompt: Send + Sync + 'static {
    /// True if the user consents. The token is cancelled when the answer is no longer needed
    /// (the asker has gone away, or too much time has passed): the question must be removed.
    /// An error means the question could not be shown.
    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<bool, String>>;
}

/// The agent behind a request: the first process, walking up from whoever opened the
/// channel, that is not a shell. Pid and start time together: once a process has died, the
/// system gives its pid alone to another one.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentIdentity {
    pub pid: u32,
    /// The start time as the system gives it (FILETIME on Windows, microseconds on the Mac):
    /// it only matters for recognizing the process.
    pub started: i64,
    /// The same time in seconds since the epoch, for display.
    pub started_at: Option<f64>,
    pub exe: Option<String>,
    pub name: String,
    pub chain: Vec<String>,
    pub signed: String,
    pub signer: Option<String>,
}

impl AgentIdentity {
    pub fn key(&self) -> String {
        format!("{}:{}", self.pid, self.started)
    }
}

/// What the hub asks the operating system. Whoever starts the hub provides it
/// (`clak-host`, with `clak-platform`); the tests provide a fake one.
pub trait Platform: Send + Sync + 'static {
    /// The agent behind process `pid` (at the other end of the control channel), or none
    /// if the process chain is broken. Slow: never under the lock.
    fn agent_of(
        &self,
        pid: Option<u32>,
        request: &Map<String, Value>,
        authorized: &HashSet<String>,
    ) -> Option<AgentIdentity>;

    /// Whether the agent's process is still alive.
    fn is_alive(&self, agent: &AgentIdentity) -> bool;

    /// Who is on the other side of a loopback TCP connection: a program's origin (`exe`,
    /// `signed`, `signer`, `pid`…), read from the system and not declared.
    fn attest_loopback(&self, peer_port: u16, local_port: u16) -> Value;

    /// The sha256 of an executable, to remember an unsigned program.
    fn executable_hash(&self, path: &str) -> Option<String>;

    /// The names of the processes above `pid`, nearest first, as the walk names them
    /// ("Visual Studio Code"): an agent launched inside an app runs below it. Slow: never
    /// under the lock. Nothing where they can't be read.
    fn above(&self, pid: u32) -> Vec<String> {
        let _ = pid;
        Vec::new()
    }
}

/// How long a connector waits: between attempts when the server does not answer (from
/// `retry_min`, doubling, up to `retry_max`), and how often it rereads the command list
/// when all is well.
#[derive(Debug, Clone, Copy)]
pub struct ConnectorTiming {
    pub retry_min: Duration,
    pub retry_max: Duration,
    pub refresh: Duration,
}

impl Default for ConnectorTiming {
    fn default() -> Self {
        ConnectorTiming {
            retry_min: Duration::from_secs(5),
            retry_max: Duration::from_secs(60),
            refresh: Duration::from_secs(600),
        }
    }
}

/// How the hub behaves. Everything has a sensible default, except the platform.
#[derive(Clone)]
pub struct HubOptions {
    /// Who asks the user. Without it, every new permission is denied.
    pub consent: Option<Arc<dyn ConsentPrompt>>,
    /// What the hub asks the operating system.
    pub platform: Arc<dyn Platform>,
    /// After this time an unanswered question is withdrawn.
    pub consent_timeout: Duration,
    /// How long a «no» holds without asking again: an agent that insists does not reopen the window.
    pub denial_memory: Duration,
    /// The cap on waiting for a program's answer: only against a stuck program. Usually the
    /// caller decides sooner (`clak --wait`), because a slow command — a render, a long
    /// script — is legitimate.
    pub call_timeout: Duration,
    /// The hub's settings as a program, `clak settings …`.
    pub settings: bool,
    /// How often to check that authorized agents are still alive.
    pub sweep_interval: Duration,
    /// The connector timings (shorter in tests).
    pub connector_timing: ConnectorTiming,
}

impl HubOptions {
    pub fn new(platform: Arc<dyn Platform>) -> Self {
        HubOptions {
            consent: None,
            platform,
            consent_timeout: Duration::from_secs(120),
            denial_memory: Duration::from_secs(60),
            call_timeout: Duration::from_secs(3600),
            settings: true,
            sweep_interval: Duration::from_secs(3),
            connector_timing: ConnectorTiming::default(),
        }
    }
}

/// The outcome of a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Denied,
    /// Nobody answered in time, or the asker has gone away.
    NoAnswer,
    /// The hub has no interface to ask with.
    NoPrompt,
}
