//! Program side: the handful of lines a program writes to be used by agents.
//!
//! ```no_run
//! # async fn example() -> Result<(), clak_kit::Unavailable> {
//! use clak_kit::{LiveResource, text};
//! let doc = LiveResource::new("Word", "contratto.docx", None, None);
//! doc.tool("get_text", "Returns the document's text.", None, |_| async { text("…") });
//! doc.connect().await?;          // the user's gesture: "this document can be used"
//! // ...
//! doc.close("resource_closed").await; // I close the document → no agent sees it anymore
//! # Ok(()) }
//! ```
//!
//! The first time an executable connects, the hub asks the user whether to allow it, and
//! remembers it. Then every agent that wants to use the document must have its own
//! permission, asked of the user by the hub: the program decides nothing, it gets told
//! (`granted`, `revoked`).
//!
//! An instance is *one opening* of a resource, not the application: a reopened document is
//! another instance, and agents will have to ask for permission again.
//!
//! Commands can be turned on and off: a command that's off doesn't exist for the hub. A
//! command added or changed after an agent's permission requires, for that agent, its own
//! permission.

use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// The result of a command: text or JSON, or an error to tell the agent.
pub type ToolResult = Result<Value, String>;

type Handler = Arc<
    dyn Fn(Map<String, Value>) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> + Send + Sync,
>;

/// Who listens to what the hub says: the message type and the message.
pub type Listener = Arc<dyn Fn(&str, &Map<String, Value>) + Send + Sync>;

/// The hub isn't there, or doesn't answer: no agent can use the program.
#[derive(Debug, Clone)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

/// A text-only result, the most common form.
pub fn text(text: impl Into<String>) -> ToolResult {
    Ok(Value::String(text.into()))
}

#[derive(Clone)]
struct Entry {
    name: String,
    description: String,
    schema: Value,
    handler: Handler,
}

#[derive(Default)]
struct State {
    /// What is open: it can change name while connected (`set_title`).
    title: String,
    tools: Vec<Entry>,
    active: BTreeSet<String>,
    users: Vec<(String, String)>,
    name: Option<String>,
    instructions: Option<String>,
    sender: Option<mpsc::UnboundedSender<Message>>,
    /// Grows on every connection: an old connection that ends doesn't touch the new one.
    generation: u64,
    listener: Option<Listener>,
}

struct Inner {
    app: String,
    publisher: Option<String>,
    instance_id: String,
    home: PathBuf,
    state: Mutex<State>,
}

/// One opening of a resource, connected (or to be connected) to the hub. Cheap to clone.
#[derive(Clone)]
pub struct LiveResource(Arc<Inner>);

impl LiveResource {
    /// `app` is the program ("Word"), `title` the live resource ("contratto.docx"), `publisher`
    /// who claims to be the author (the hub shows it for what it is: a claim), `home` the
    /// hub's folder (normally `~/.clak`, or `CLAK_HOME`).
    pub fn new(
        app: &str,
        title: &str,
        publisher: Option<&str>,
        home: Option<PathBuf>,
    ) -> LiveResource {
        LiveResource(Arc::new(Inner {
            app: app.to_owned(),
            publisher: publisher.map(str::to_owned),
            instance_id: clak_wire::json::new_id("inst"),
            home: home.unwrap_or_else(clak_wire::location::home),
            state: Mutex::new(State {
                title: title.to_owned(),
                ..State::default()
            }),
        }))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn app(&self) -> &str {
        &self.0.app
    }

    pub fn title(&self) -> String {
        self.state().title.clone()
    }

    /// The same document has a new name (saved as, renamed): the hub shows it to the user and
    /// to agents from now on, and the agents keep their permissions. Sent at once if
    /// connected, otherwise with the next `register`. Another document is not a new name:
    /// it's another `LiveResource`, and the agents ask again.
    pub fn set_title(&self, title: &str) {
        let mut state = self.state();
        if state.title == title {
            return;
        }
        state.title = title.to_owned();
        send(
            &state,
            &json!({ "type": "resource_changed", "title": title }),
        );
    }

    /// The identity of *this* opening, not of the file.
    pub fn instance_id(&self) -> &str {
        &self.0.instance_id
    }

    /// The name agents call it by (`clak blocco-di-prova …`): the hub decides it.
    pub fn name(&self) -> Option<String> {
        self.state().name.clone()
    }

    /// The agents allowed to use it now: permission id and agent name.
    pub fn used_by(&self) -> Vec<(String, String)> {
        self.state().users.clone()
    }

    pub fn connected(&self) -> bool {
        self.state().sender.as_ref().is_some_and(|s| !s.is_closed())
    }

    /// Optional: how the program wants to be used, what applies to all commands
    /// ("get_scene_summary first"). Agents read them with `clak <program> -h`; this way the
    /// command descriptions stay short. Sent when connecting.
    pub fn set_instructions(&self, instructions: Option<&str>) {
        self.state().instructions = instructions.map(str::to_owned);
    }

    /// What the hub says: `waiting` (the user must allow the connection),
    /// `registered`, `denied`, `granted` and `revoked` (an agent got or lost the
    /// permission), `call`, `cancel`, `error` — and `disconnected` when the hub disappears or
    /// closes the connection. Arrives on any thread.
    pub fn on_event(&self, listener: impl Fn(&str, &Map<String, Value>) + Send + Sync + 'static) {
        self.state().listener = Some(Arc::new(listener));
    }

    // --- the commands ---------------------------------------------------------

    /// Declares a command, or redefines it. After connecting, [`LiveResource::publish`] is needed.
    pub fn tool<F, Fut>(&self, name: &str, description: &str, schema: Option<Value>, handler: F)
    where
        F: Fn(Map<String, Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        self.tool_with(name, description, schema, true, handler);
    }

    pub fn tool_with<F, Fut>(
        &self,
        name: &str,
        description: &str,
        schema: Option<Value>,
        active: bool,
        handler: F,
    ) where
        F: Fn(Map<String, Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        let entry = Entry {
            name: name.to_owned(),
            description: description.to_owned(),
            schema: schema.unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            handler: Arc::new(move |arguments| Box::pin(handler(arguments))),
        };
        let mut state = self.state();
        match state.tools.iter().position(|t| t.name == name) {
            Some(at) => state.tools[at] = entry,
            None => state.tools.push(entry),
        }
        if active {
            state.active.insert(name.to_owned());
        } else {
            state.active.remove(name);
        }
    }

    pub fn drop_tool(&self, name: &str) {
        let mut state = self.state();
        state.tools.retain(|t| t.name != name);
        state.active.remove(name);
    }

    /// The set of commands is dynamic: the selection changes, the commands change.
    pub fn set_active(&self, on: bool, names: &[&str]) {
        {
            let mut state = self.state();
            for name in names {
                if on {
                    state.active.insert((*name).to_owned());
                } else {
                    state.active.remove(*name);
                }
            }
        }
        self.publish();
    }

    /// Tells the hub which commands exist now. Without a connection it does nothing.
    pub fn publish(&self) {
        let state = self.state();
        let tools = describe(
            state
                .tools
                .iter()
                .filter(|t| state.active.contains(&t.name)),
        );
        let mut message = json!({ "type": "tools_changed", "tools": tools });
        if let Some(instructions) = state.instructions.as_deref().filter(|i| !i.is_empty()) {
            message["instructions"] = Value::String(instructions.to_owned());
        }
        send(&state, &message);
    }

    // --- the life cycle -------------------------------------------------------

    /// Connects to the hub: from here agents see it, and can ask the user for permission
    /// to use it. It's what's behind the application's button. If it's already connected
    /// it does nothing.
    pub async fn connect(&self) -> Result<(), Unavailable> {
        if self.connected() {
            return Ok(());
        }
        let lockfile = clak_wire::location::lockfile(&self.0.home);
        // The token is what stops any web page from connecting on its own: it's in a file
        // only the user can read.
        let Some(lock) = clak_wire::location::Lockfile::read(&self.0.home) else {
            return Err(Unavailable(format!(
                "Clak isn't running ({} is missing).",
                lockfile.display()
            )));
        };
        if lock.protocol != clak_wire::PROTOCOL {
            return Err(Unavailable(format!(
                "Clak speaks version {} of the protocol, this program version {}: one of the two needs updating.",
                lock.protocol,
                clak_wire::PROTOCOL
            )));
        }
        let (socket, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}", lock.ws_port))
                .await
                .map_err(|_| Unavailable("Clak isn't responding: maybe it was closed.".into()))?;
        let (mut sink, mut source) = socket.split();
        let (sender, mut queue) = mpsc::unbounded_channel::<Message>();
        let generation = {
            let mut state = self.state();
            state.generation += 1;
            state.sender = Some(sender);
            let register = {
                let mut message = json!({
                    "type": "register",
                    "protocol": clak_wire::PROTOCOL,
                    "token": lock.token,
                    "resource": {
                        "app": self.0.app,
                        "title": state.title,
                        "instance_id": self.0.instance_id,
                        "publisher": self.0.publisher,
                    },
                    "tools": describe(state.tools.iter()),
                    "active": state.active.iter().cloned().collect::<Vec<_>>(),
                });
                if let Some(instructions) = state.instructions.as_deref().filter(|i| !i.is_empty())
                {
                    message["instructions"] = Value::String(instructions.to_owned());
                }
                message
            };
            send(&state, &register);
            state.generation
        };

        tokio::spawn(async move {
            while let Some(message) = queue.recv().await {
                let closing = matches!(message, Message::Close(_));
                if sink.send(message).await.is_err() || closing {
                    break;
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(Ok(frame)) = source.next().await {
                let raw = match frame {
                    Message::Text(text) => text.as_bytes().to_vec(),
                    Message::Binary(bytes) => bytes.to_vec(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(Value::Object(message)) = clak_wire::json::parse(&raw) else {
                    continue;
                };
                let Some(kind) = message
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    continue;
                };
                this.received(&kind, &message);
            }
            // If the connection is still ours, it was the hub that closed it.
            let ended = {
                let mut state = this.state();
                if state.generation == generation && state.sender.is_some() {
                    state.sender = None;
                    state.users.clear();
                    state.name = None;
                    true
                } else {
                    false
                }
            };
            if ended {
                this.raise(
                    "disconnected",
                    &Map::from_iter([("type".to_owned(), json!("disconnected"))]),
                );
            }
        });
        Ok(())
    }

    fn received(&self, kind: &str, message: &Map<String, Value>) {
        let text = |key: &str| {
            message
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        match kind {
            "registered" => self.state().name = Some(text("name")),
            "granted" => {
                let harness = message
                    .get("harness")
                    .and_then(Value::as_str)
                    .unwrap_or("an agent")
                    .to_owned();
                self.state().users.push((text("grant_id"), harness));
            }
            "revoked" => {
                let id = text("grant_id");
                self.state().users.retain(|(grant, _)| *grant != id);
            }
            // In a task: a slow call must not hold up the others.
            "call" => {
                let this = self.clone();
                let call = message.clone();
                tokio::spawn(async move { this.dispatch(call).await });
            }
            _ => {}
        }
        self.raise(kind, message);
    }

    fn raise(&self, kind: &str, message: &Map<String, Value>) {
        let listener = self.state().listener.clone();
        if let Some(listener) = listener {
            // An error from the listener must not cut the line to the hub.
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(kind, message)));
        }
    }

    async fn dispatch(&self, call: Map<String, Value>) {
        let name = call.get("tool").and_then(Value::as_str).unwrap_or("");
        let entry = {
            let state = self.state();
            if state.active.contains(name) {
                state.tools.iter().find(|t| t.name == name).cloned()
            } else {
                None
            }
        };
        let mut reply = json!({ "type": "result", "call_id": call.get("call_id").cloned().unwrap_or(Value::Null) });
        match entry {
            None => {
                reply["is_error"] = Value::Bool(true);
                reply["content"] = json!("no such command");
            }
            Some(entry) => {
                let arguments = call
                    .get("arguments")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                // A command that breaks reports its error, without taking the program down with it.
                let outcome = tokio::spawn((entry.handler)(arguments)).await;
                match outcome {
                    Ok(Ok(content)) => reply["content"] = content,
                    Ok(Err(problem)) => {
                        reply["is_error"] = Value::Bool(true);
                        reply["content"] = Value::String(problem);
                    }
                    Err(join) => {
                        reply["is_error"] = Value::Bool(true);
                        reply["content"] = Value::String(panic_text(join));
                    }
                }
            }
        }
        send(&self.state(), &reply);
    }

    /// I close the resource → the permissions die with it, and no agent sees it anymore.
    pub async fn close(&self, reason: &str) {
        let sender = {
            let mut state = self.state();
            let sender = state.sender.take();
            state.users.clear();
            state.name = None;
            sender
        };
        if let Some(sender) = sender {
            let _ = sender.send(Message::text(clak_wire::json::text(
                &json!({ "type": "close", "reason": reason }),
            )));
            let _ = sender.send(Message::Close(None));
            // Time for the two messages to go out.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            while !sender.is_closed() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    }
}

fn panic_text(join: tokio::task::JoinError) -> String {
    match join.try_into_panic() {
        Ok(payload) => payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "the command failed".into()),
        Err(_) => "the command was canceled".into(),
    }
}

fn describe<'a>(tools: impl Iterator<Item = &'a Entry>) -> Value {
    Value::Array(
        tools.map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.schema })).collect(),
    )
}

fn send(state: &State, message: &Value) {
    if let Some(sender) = &state.sender {
        let _ = sender.send(Message::text(clak_wire::json::text(message)));
    }
}
