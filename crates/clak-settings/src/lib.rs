//! The hub settings as a program: `clak settings …`. An agent sees the MCP servers and the
//! REST APIs, and proposes adding, changing, turning off and removing them.
//!
//! The name "settings" is reserved ([`cli_names::SETTINGS`]): no program, connector or API
//! can take it, and the core grants it only to whoever registers with the origin
//! `{"kind":"hub"}`. Using it asks for permission like any program; on top of that the user
//! confirms every change, one by one, in a hub window that says what changes: permission to
//! use "settings" isn't permission to change anything at all.
//!
//! Secrets stay with the hub: an agent never reads them, and a saved secret doesn't follow
//! a new address on another host (nor a changed OAuth) when an agent makes the change. An
//! agent can give a token or a key, which it already knows; an OAuth sign-in instead is
//! done by the user from the panel, because the browser opens only from there.

mod apis;
mod args;
mod servers;
mod tools;

use args::Args;
use clak_apis::ApiHub;
use clak_connectors::ConnectorHub;
use clak_hub::{Broker, Outbox, ResourceDef, SettingsChange, ToolDef, Verdict, cli_names, texts};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use tools::{INSTRUCTIONS, tools};

/// After adding a server, how long to wait to learn whether it answers, to tell the agent.
pub const SETTLE: Duration = Duration::from_secs(15);

/// Why a command doesn't complete.
pub(crate) enum Stop {
    /// A missing or invalid argument, or a no from the user: it becomes the answer to the
    /// agent, not a hub error.
    Say(String),
    /// Something broke along the way (the disk, the network): "Couldn't complete it: …".
    Failed(String),
    /// The caller has left (or the hub is closing): nobody is waiting anymore.
    Canceled,
}

/// The answer of a command: the `result` without `type` and `call_id`.
pub(crate) type Answer = Result<Map<String, Value>, Stop>;

pub(crate) fn say(text: impl Into<String>) -> Stop {
    Stop::Say(text.into())
}

/// The hub settings as a program. Built with [`HubSettings::new`], registered in the core
/// with [`HubSettings::start`] and stopped with [`HubSettings::stop`].
pub struct HubSettings {
    pub(crate) broker: Broker,
    pub(crate) connectors: Arc<ConnectorHub>,
    pub(crate) apis: Arc<ApiHub>,
    /// The program in the core, while it's registered. Written while held during
    /// registration: a command that arrives right away waits to know whom to answer.
    conn_id: Mutex<Option<String>>,
    stop: CancellationToken,
    /// The running commands, by `call_id`: a `cancel` from the hub stops them.
    calls: Mutex<HashMap<String, CancellationToken>>,
    pump: Mutex<Option<JoinHandle<()>>>,
    settle: Mutex<Duration>,
    me: Weak<HubSettings>,
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

impl HubSettings {
    pub fn new(
        broker: Broker,
        connectors: Arc<ConnectorHub>,
        apis: Arc<ApiHub>,
    ) -> Arc<HubSettings> {
        Arc::new_cyclic(|me| HubSettings {
            broker,
            connectors,
            apis,
            conn_id: Mutex::new(None),
            stop: CancellationToken::new(),
            calls: Mutex::new(HashMap::new()),
            pump: Mutex::new(None),
            settle: Mutex::new(SETTLE),
            me: me.clone(),
        })
    }

    /// How long to wait, after a change, to learn whether a server answers ([`SETTLE`]).
    pub fn settle(&self) -> Duration {
        *guard(&self.settle)
    }

    pub fn set_settle(&self, settle: Duration) {
        *guard(&self.settle) = settle;
    }

    /// The program in the core, if it's registered.
    pub fn conn_id(&self) -> Option<String> {
        guard(&self.conn_id).clone()
    }

    /// Registers in the core as `settings`, with its instructions: from here on agents see it.
    pub fn start(&self) {
        let (outbox, rx) = Outbox::channel();
        {
            let mut slot = guard(&self.conn_id);
            let id = self.broker.run(|c| {
                let id = c.register(
                    outbox,
                    ResourceDef {
                        app: "Clak".into(),
                        title: "Settings".into(),
                        instance_id: cli_names::SETTINGS.into(),
                        publisher: None,
                    },
                    tools(),
                    json!({ "kind": "hub" }),
                    None,
                );
                c.set_instructions(&id, Some(INSTRUCTIONS.to_owned()));
                id
            });
            *slot = Some(id);
        }
        let me = self.me.upgrade().expect("HubSettings lives in an Arc");
        let pump = tokio::spawn(me.pump(rx));
        *guard(&self.pump) = Some(pump);
    }

    /// Leaves the core: running commands end without an answer, the program with
    /// `reason`.
    pub async fn stop(&self, reason: &str) {
        self.stop.cancel();
        for (_, call) in guard(&self.calls).drain() {
            call.cancel();
        }
        let conn_id = guard(&self.conn_id).take();
        if let Some(conn_id) = conn_id {
            self.broker.run(|c| c.app_closed(&conn_id, reason));
        }
        let pump = guard(&self.pump).take();
        if let Some(pump) = pump {
            let _ = tokio::time::timeout(Duration::from_secs(5), pump).await;
        }
    }

    /// What the hub sends to the program: `call` and `cancel`; the rest (`registered`…)
    /// asks for nothing.
    async fn pump(self: Arc<Self>, mut rx: UnboundedReceiver<Value>) {
        loop {
            let message = tokio::select! {
                message = rx.recv() => message,
                _ = self.stop.cancelled() => None,
            };
            let Some(message) = message else { return };
            match message.get("type").and_then(Value::as_str) {
                Some("call") => {
                    let call_id = text_of(&message, "call_id");
                    let tool = text_of(&message, "tool");
                    let arguments = message
                        .get("arguments")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let cancel = self.stop.child_token();
                    guard(&self.calls).insert(call_id.clone(), cancel.clone());
                    let me = self.clone();
                    tokio::spawn(async move { me.call(call_id, tool, arguments, cancel).await });
                }
                Some("cancel") => {
                    if let Some(id) = message.get("call_id").and_then(Value::as_str) {
                        let call = guard(&self.calls).remove(id);
                        if let Some(call) = call {
                            call.cancel();
                        }
                    }
                }
                _ => {}
            }
        }
    }

    async fn call(
        &self,
        call_id: String,
        tool: String,
        arguments: Map<String, Value>,
        cancel: CancellationToken,
    ) {
        let outcome = self
            .run(&call_id, &tool, &Args::new(&arguments), &cancel)
            .await;
        guard(&self.calls).remove(&call_id);
        let mut result = match outcome {
            Ok(result) => result,
            Err(Stop::Say(message)) => text(&message, true),
            Err(Stop::Failed(message)) => text(&format!("Couldn't complete it: {message}"), true),
            // The caller has left: nobody is waiting anymore.
            Err(Stop::Canceled) => return,
        };
        if cancel.is_cancelled() {
            return;
        }
        result.insert("type".into(), "result".into());
        result.insert("call_id".into(), call_id.into());
        if let Some(conn_id) = self.conn_id() {
            self.broker.run(|c| {
                let _ = c.result(&conn_id, &result);
            });
        }
    }

    async fn run(
        &self,
        call_id: &str,
        tool: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        match tool {
            "servers" => {
                let rows = rows(&self.connectors.view());
                Ok(json_text(&Value::Array(
                    rows.iter().map(servers::server_row).collect(),
                )))
            }
            "apis" => {
                let rows = rows(&self.apis.view());
                Ok(json_text(&Value::Array(
                    rows.iter().map(apis::api_row).collect(),
                )))
            }
            "show-api" => {
                let entry = self.find_api(&args.required("api")?)?;
                Ok(json_text(&self.show_api(&entry)?))
            }
            "add-server" => self.add_server(call_id, args, cancel).await,
            "edit-server" => self.edit_server(call_id, args, cancel).await,
            "remove-server" => self.remove_server(call_id, args, cancel).await,
            "add-api" => self.add_api(call_id, args, cancel).await,
            "edit-api" => self.edit_api(call_id, args, cancel).await,
            "set-endpoint" => self.set_endpoint(call_id, args, cancel).await,
            "remove-endpoint" => self.remove_endpoint(call_id, args, cancel).await,
            "remove-api" => self.remove_api(call_id, args, cancel).await,
            _ => Ok(text(
                &format!("Clak's settings have no command “{tool}”."),
                true,
            )),
        }
    }

    /// The question to the user. `Ok` if they confirmed; otherwise the answer to give the agent.
    pub(crate) async fn confirm(
        &self,
        call_id: &str,
        change: SettingsChange,
        cancel: &CancellationToken,
    ) -> Result<(), Stop> {
        let Some(conn_id) = self.conn_id() else {
            return Err(say("Clak's settings are closing."));
        };
        let verdict = self
            .broker
            .confirm(
                &conn_id,
                call_id,
                |id, agent, cwd, _| texts::change(id, agent, cwd, &change),
                cancel,
            )
            .await;
        if cancel.is_cancelled() {
            return Err(Stop::Canceled);
        }
        match verdict {
            Verdict::Allowed => Ok(()),
            Verdict::Denied => Err(say(
                "The user declined this change. Do not retry unless they ask you to.",
            )),
            Verdict::NoPrompt => Err(say(
                "Changes need the user's confirmation, but this hub has no interface to ask for it: the Clak app must be started.",
            )),
            _ => Err(say(
                "Nobody confirmed the change: the question appeared in a Clak window. Ask the user to look at it and try again.",
            )),
        }
    }
}

// =========================================================================
// Shared
// =========================================================================

fn text_of(message: &Value, key: &str) -> String {
    message
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// The rows of a (panel) view: only the objects.
pub(crate) fn rows(view: &Value) -> Vec<Map<String, Value>> {
    view.as_array()
        .map(|rows| rows.iter().filter_map(Value::as_object).cloned().collect())
        .unwrap_or_default()
}

/// The row with this id, if any.
pub(crate) fn row_of<'a>(
    rows: &'a [Map<String, Value>],
    id: &str,
) -> Option<&'a Map<String, Value>> {
    rows.iter()
        .find(|r| r.get("id").and_then(Value::as_str) == Some(id))
}

pub(crate) fn str_of<'a>(row: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// By id, by command, by name: the first way that finds something decides.
pub(crate) fn find<T: Clone>(
    entries: &[T],
    key: &str,
    id: impl Fn(&T) -> &str,
    cli: impl Fn(&T) -> String,
    name: impl Fn(&T) -> &str,
    what: &str,
    list: &str,
) -> Result<T, Stop> {
    let key = key.trim();
    let mut found: Vec<&T> = entries.iter().filter(|e| id(e) == key).collect();
    if found.is_empty() {
        found = entries.iter().filter(|e| cli(e) == key).collect();
    }
    if found.is_empty() {
        let lowered = key.to_lowercase();
        found = entries
            .iter()
            .filter(|e| name(e).to_lowercase() == lowered)
            .collect();
    }
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(say(format!(
            "No {what} is called “{key}”: `clak settings {list}` lists them."
        ))),
        _ => Err(say(format!(
            "More than one {what} is called “{key}”: use its command or its id (`clak settings {list}`)."
        ))),
    }
}

/// An `http` address to another computer: the question says so.
pub(crate) fn unencrypted(url: &str, secret: bool) -> Option<String> {
    ConnectorHub::unencrypted(url).then(|| {
        format!(
            "The address isn't encrypted (http): {}everything sent travels over the network in clear text.",
            if secret { "the secret and " } else { "" }
        )
    })
}

/// A command written by the agent; empty means the automatic one.
pub(crate) fn command(command: Option<String>) -> Option<String> {
    command
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty())
}

pub(crate) fn secret(secret: Option<String>) -> Option<String> {
    secret
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub(crate) fn clean(text: &str) -> String {
    texts::clean(Some(text), 40)
}

/// At most twenty names, and how many more.
pub(crate) fn some<S: AsRef<str>>(names: &[S]) -> String {
    let all: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
    if all.len() <= 20 {
        all.join(", ")
    } else {
        format!("{} and {} more", all[..20].join(", "), all.len() - 20)
    }
}

pub(crate) fn text(text: &str, error: bool) -> Map<String, Value> {
    let mut result = Map::new();
    result.insert("is_error".into(), error.into());
    result.insert("content".into(), json!([{ "type": "text", "text": text }]));
    result
}

/// A JSON for the agent: indented, in a text block.
pub(crate) fn json_text(value: &Value) -> Map<String, Value> {
    text(
        &serde_json::to_string_pretty(value).unwrap_or_default(),
        false,
    )
}

/// The JSON of a command for the core: the definitions are ours, and must be valid.
pub(crate) fn tool_def(value: Value) -> ToolDef {
    ToolDef::from_json(&value).expect("a well-formed settings command")
}
