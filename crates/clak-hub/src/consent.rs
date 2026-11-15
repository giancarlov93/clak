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
    /// An agent wants to run a command of a program: every call asks, unless the user
    /// allowed that command, or all the program's commands, for the agent's session.
    Call,
    /// An agent wants to use Clak's own settings, for its session: every change asks anyway.
    Use,
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
            ConsentKind::Call => "call",
            ConsentKind::Use => "use",
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
    /// What the agent sends with a command, one line per argument, already cleaned and with
    /// what looks like a secret masked: only in a command's question.
    pub arguments: Vec<ConsentLine>,
    /// The wider answers a command's question offers, in words.
    pub choices: Option<CallChoices>,
}

/// What a command's question offers besides "only this call" and "deny". The window shows
/// the scopes as a choice, "always" as a box to tick with the scope, and the block as a
/// button of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct CallChoices {
    /// «“get_text” for this session».
    pub command: String,
    /// «All 3 commands of Notion for this session».
    pub program: String,
    /// «Block Notion for Claude Code».
    pub block: String,
    /// The offer to make the command's yes standing, when it can be.
    pub always_command: Option<String>,
    /// The same for all the program's commands.
    pub always_program: Option<String>,
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
            "arguments": lines(&self.arguments),
            "choices": self.choices.as_ref().map(|c| json!({
                "command": c.command,
                "program": c.program,
                "block": c.block,
                "always_command": c.always_command,
                "always_program": c.always_program,
            })),
        })
    }
}

/// How far a yes to a command goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// This call only.
    Once,
    /// This command, as it is now, for the agent's session.
    Command,
    /// All the program's commands the question showed, for the agent's session.
    Program,
}

impl Scope {
    pub fn parse(text: &str) -> Option<Scope> {
        match text {
            "once" => Some(Scope::Once),
            "command" => Some(Scope::Command),
            "program" => Some(Scope::Program),
            _ => None,
        }
    }
}

/// The user's answer to a question. Only a command's question offers more than
/// [`Reply::YES`] and [`Reply::Deny`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Deny,
    /// No, and the agent may not use this program for the rest of its session.
    Block,
    Allow {
        scope: Scope,
        /// The user also ticked "always": the yes becomes a standing rule.
        always: bool,
    },
}

impl Reply {
    /// A plain yes.
    pub const YES: Reply = Reply::Allow {
        scope: Scope::Once,
        always: false,
    };

    pub fn allows(self) -> bool {
        matches!(self, Reply::Allow { .. })
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
    /// The user's answer. The token is cancelled when the answer is no longer needed (the
    /// asker has gone away, too much time has passed, or another answer covered it): the
    /// question must be removed. An error means the question could not be shown.
    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<Reply, String>>;
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

    /// A program's origin read from its file instead of from its process (`exe`, `signed`,
    /// `signer`): where the user points an application that has moved. Slow: never under
    /// the lock.
    fn origin_of(&self, exe: &str) -> Value {
        json!({ "exe": exe })
    }

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
    /// Where the user answers a question, from its id, when the questions are on a web page
    /// rather than on this screen (`clak-daemon --web`). Then an agent isn't kept waiting:
    /// it gets the link at once and asks again after the answer, and the question stays open
    /// until it is answered or its time runs out, whether the agent waits or not.
    pub question_link: Option<QuestionLink>,
}

/// See [`HubOptions::question_link`].
pub type QuestionLink = Arc<dyn Fn(&str) -> String + Send + Sync>;

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
            question_link: None,
        }
    }
}

/// The outcome of a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Denied,
    /// Denied, and the program is closed to the agent for its session.
    Blocked,
    /// Nobody answered in time, or the asker has gone away.
    NoAnswer,
    /// The hub has no interface to ask with.
    NoPrompt,
}
