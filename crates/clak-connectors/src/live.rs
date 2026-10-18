//! An MCP server reached by the hub, which appears to agents as a connected program.
//! It registers in the broker like the others: on the other side of its [`Outbox`] there
//! is no WebSocket but this object, which turns every `call` into a `tools/call` and sends
//! back the outcome with [`Core::result`].
//!
//! A server that doesn't answer is not a closed document: the connector stays registered,
//! with its permissions, the commands answer "not responding" and it retries with growing
//! waits. A rejected token stops the retries until the user changes it; an OAuth sign-in
//! refreshes itself, and when it no longer can (revoked, expired) it waits for the user
//! to redo it from the panel: the browser is never opened from here. The permissions end
//! when the user removes or turns off the connector, or changes its token or sign-in.
//!
//! A server that is a program on this computer (stdio) is started by the connector, and
//! restarted with the same waits if it stops. Its environment variables count as a
//! token: changing them makes the permissions start over.
//!
//! Locks: the connector's state is taken *before* the hub's lock, never inside it
//! ([`Broker::run`] never calls back into this module); whoever must be told about
//! changes (the panel) is told after releasing it.

use crate::launch::Launch;
use crate::mcp::McpClient;
use crate::mcp_http::{AccessToken, Elicit, McpError, McpHttp};
use crate::mcp_stdio::McpStdio;
use crate::store::ConnectorEntry;
use crate::uri;
use clak_hub::texts::{self, clean};
use clak_hub::{AppConn, Broker, ConnectorTiming, Outbox, ResourceDef, ToolDef, Verdict};
use clak_wire::json::{self as wire, now, quoted};
use reqwest::Client;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

/// A connector's states, as the panel shows them.
pub mod phase {
    pub const ACTIVE: &str = "active";
    pub const CONNECTING: &str = "connecting";
    pub const UNREACHABLE: &str = "unreachable";
    pub const REJECTED: &str = "token rejected";
    pub const SIGN_IN_REQUIRED: &str = "sign-in required";
    pub const OFF: &str = "off";
}
use phase::*;

/// How long to wait for a just-started program to answer: `npx` may have to download the
/// package, but a program that doesn't talk must not stay "connecting" forever.
const START_LIMIT: Duration = Duration::from_secs(120);

/// Where the server is: at an address, or in a program to start.
enum Target {
    Http(Url),
    Stdio(Launch),
}

/// Who must be told when something the panel shows changes.
pub(crate) type Changed = Arc<dyn Fn() + Send + Sync>;

struct State {
    entry: ConnectorEntry,
    phase: &'static str,
    error: Option<String>,
    /// The id in the broker, while registered.
    conn_id: Option<String>,
    active_since: Option<f64>,
    /// The icon the server declares, from its last `initialize`.
    icon: Option<String>,
    credential: Option<Arc<dyn AccessToken>>,
    /// A program's environment variables (stdio).
    env: BTreeMap<String, String>,
    /// The client of the open session; nothing when the server must be contacted again.
    client: Option<Arc<McpClient>>,
}

/// What the panel sees of a running connector.
pub(crate) struct Row {
    pub phase: &'static str,
    pub error: Option<String>,
    pub conn_id: Option<String>,
    pub active_since: Option<f64>,
    pub icon: Option<String>,
    pub has_credential: bool,
    pub signin_host: Option<String>,
}

pub(crate) struct Connector {
    broker: Broker,
    http: Client,
    timing: ConnectorTiming,
    target: Target,
    outbox: Outbox,
    inbox: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
    stop: CancellationToken,
    wake: Notify,
    /// A token changed in the meantime makes an attempt stale: it starts over.
    generation: AtomicU64,
    state: Mutex<State>,
    calls: Mutex<HashMap<String, CancellationToken>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    changed: Changed,
}

impl Connector {
    pub fn new(
        broker: Broker,
        entry: ConnectorEntry,
        credential: Option<Arc<dyn AccessToken>>,
        env: BTreeMap<String, String>,
        http: Client,
        timing: ConnectorTiming,
        changed: Changed,
    ) -> Arc<Connector> {
        let target = match &entry.launch {
            Some(launch) => Target::Stdio(launch.clone()),
            None => Target::Http(
                uri::absolute(&entry.url).expect("a saved connector has a valid address"),
            ),
        };
        let (outbox, inbox) = Outbox::channel();
        let stop = broker.stopping().child_token();
        Arc::new(Connector {
            broker,
            http,
            timing,
            target,
            outbox,
            inbox: Mutex::new(Some(inbox)),
            stop,
            wake: Notify::new(),
            generation: AtomicU64::new(0),
            state: Mutex::new(State {
                entry,
                phase: CONNECTING,
                error: None,
                conn_id: None,
                active_since: None,
                icon: None,
                credential,
                env,
                client: None,
            }),
            calls: Mutex::new(HashMap::new()),
            tasks: Mutex::new(Vec::new()),
            changed,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn row(&self) -> Row {
        let st = self.lock();
        Row {
            phase: st.phase,
            error: st.error.clone(),
            conn_id: st.conn_id.clone(),
            active_since: st.active_since,
            icon: st.icon.clone(),
            has_credential: st.credential.is_some(),
            signin_host: st.credential.as_ref().and_then(|c| c.host()),
        }
    }

    fn name(&self) -> String {
        clean(Some(&self.lock().entry.name), 40)
    }

    pub fn start(self: &Arc<Self>) {
        let inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner()).take();
        let mut tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(inbox) = inbox {
            tasks.push(tokio::spawn(self.clone().pump(inbox)));
        }
        tasks.push(tokio::spawn(self.clone().run()));
    }

    /// A new token or sign-in: for agents it's another credential, and the permissions start over.
    pub fn set_credential(
        &self,
        entry: ConnectorEntry,
        credential: Option<Arc<dyn AccessToken>>,
        reason: &str,
    ) {
        let old = {
            let mut st = self.lock();
            let old = std::mem::replace(&mut st.credential, credential);
            st.client = None;
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.broker.run(|c| {
                st.entry = entry;
                if let Some(id) = st.conn_id.take() {
                    c.app_closed(&id, reason);
                }
                st.phase = CONNECTING;
                st.error = None;
                c.touch();
            });
            old
        };
        if let Some(old) = old {
            old.retire();
        }
        self.wake.notify_one();
        (self.changed)();
    }

    /// New environment variables for the program: it restarts, and the permissions start over.
    pub fn set_env(&self, entry: ConnectorEntry, env: BTreeMap<String, String>, reason: &str) {
        {
            let mut st = self.lock();
            st.env = env;
            st.client = None;
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.broker.run(|c| {
                st.entry = entry;
                if let Some(id) = st.conn_id.take() {
                    c.app_closed(&id, reason);
                }
                st.phase = CONNECTING;
                st.error = None;
                c.touch();
            });
        }
        self.wake.notify_one();
        (self.changed)();
    }

    /// A new name or command: the same server, so the permissions stay.
    pub fn rename(&self, entry: ConnectorEntry) {
        {
            let mut st = self.lock();
            let resource = self.resource(&entry);
            self.broker.run(|c| {
                if let Some(id) = &st.conn_id {
                    c.rename(id, resource, entry.command.as_deref());
                }
                st.entry = entry;
                c.touch();
            });
        }
        (self.changed)();
    }

    fn resource(&self, entry: &ConnectorEntry) -> ResourceDef {
        let title = match &self.target {
            Target::Http(url) => match url.port() {
                Some(port) => format!("{}:{port}", uri::host(url)),
                None => uri::host(url),
            },
            Target::Stdio(launch) => launch.short_name(),
        };
        ResourceDef {
            app: entry.name.clone(),
            title,
            instance_id: entry.id.clone(),
            publisher: None,
        }
    }

    pub async fn stop(&self, reason: &str) {
        self.stop.cancel();
        {
            let mut st = self.lock();
            // A program stops along with its client.
            st.client = None;
            self.broker.run(|c| {
                if let Some(id) = st.conn_id.take() {
                    c.app_closed(&id, reason);
                }
                c.touch();
            });
        }
        let tasks: Vec<JoinHandle<()>> = self
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .collect();
        // Closing down: how an attempt ended no longer matters.
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            for task in tasks {
                let _ = task.await;
            }
        })
        .await;
    }

    // =========================================================================
    // The link: open it, keep it, retry
    // =========================================================================

    async fn run(self: Arc<Self>) {
        let mut backoff = self.timing.retry_min;
        loop {
            let outcome = tokio::select! {
                _ = self.stop.cancelled() => return,
                outcome = self.session(&mut backoff) => outcome,
            };
            match outcome {
                Ok(()) => {}
                // A new session, right away.
                Err(McpError::SessionLost) => {}
                Err(e @ McpError::Auth { .. }) => {
                    self.lock().client = None;
                    let (phase, reason) = self.auth_phase(&e);
                    self.set_phase(phase, Some(reason));
                    // Retrying with the same token is noise: wait for the user to change it or sign in again.
                    tokio::select! {
                        _ = self.stop.cancelled() => return,
                        _ = self.wake.notified() => {}
                    }
                }
                Err(e) => {
                    self.lock().client = None;
                    self.set_phase(UNREACHABLE, Some(e.to_string()));
                    tokio::select! {
                        _ = self.stop.cancelled() => return,
                        _ = self.wake.notified() => {}
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(self.timing.retry_max);
                }
            }
        }
    }

    /// A session: open it, publish, and keep it by re-reading the list every so often, or
    /// right away if a command suggests it changed. `Ok` means "start over": the token
    /// changed, or a command found the server down or the session expired.
    async fn session(&self, backoff: &mut Duration) -> Result<(), McpError> {
        let generation = self.generation.load(Ordering::SeqCst);
        let (credential, env) = {
            let st = self.lock();
            (st.credential.clone(), st.env.clone())
        };
        let can_elicit = self.broker.options().consent.is_some();
        let client = Arc::new(match &self.target {
            Target::Http(url) => McpClient::Http(McpHttp::new(
                self.http.clone(),
                url.clone(),
                credential,
                can_elicit,
            )),
            Target::Stdio(launch) => {
                McpClient::Stdio(McpStdio::start(launch, &env, can_elicit).await?)
            }
        });
        let opening = async {
            client.initialize().await?;
            client.list_tools().await
        };
        let tools = match &self.target {
            Target::Http(_) => opening.await?,
            Target::Stdio(_) => tokio::select! {
                tools = tokio::time::timeout(START_LIMIT, opening) => tools.map_err(|_| {
                    McpError::Transport(format!("the program didn't answer within {} seconds", START_LIMIT.as_secs()))
                })??,
                // A program that exits before answering: it tells why itself.
                reason = client.ended() => return Err(McpError::Transport(reason)),
            },
        };
        if generation != self.generation.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.publish(&client, tools);
        *backoff = self.timing.retry_min;
        loop {
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(self.timing.refresh) => {}
                _ = client.changed() => {}
                reason = client.ended() => return Err(McpError::Transport(reason)),
            }
            if self.lock().client.is_none() || generation != self.generation.load(Ordering::SeqCst)
            {
                return Ok(());
            }
            let tools = client.list_tools().await?;
            if generation != self.generation.load(Ordering::SeqCst) {
                return Ok(());
            }
            self.publish(&client, tools);
        }
    }

    /// An OAuth sign-in to redo, or a server that asks for one and there is none; otherwise a rejected token.
    fn auth_phase(&self, error: &McpError) -> (&'static str, String) {
        let McpError::Auth {
            message,
            challenge,
            sign_in,
        } = error
        else {
            unreachable!("only for auth errors")
        };
        let none = self.lock().credential.is_none();
        let wants_sign_in = *sign_in
            || (none
                && challenge
                    .as_ref()
                    .is_some_and(|c| c.resource_metadata.is_some()));
        let phase = if wants_sign_in {
            SIGN_IN_REQUIRED
        } else {
            REJECTED
        };
        let reason = match (none, wants_sign_in) {
            (true, true) => "the server asks you to sign in".to_owned(),
            (true, false) => {
                "the server asks for a token or a sign-in, and there is none".to_owned()
            }
            _ => message.clone(),
        };
        (phase, reason)
    }

    /// The server's commands become hub commands; the first time, the connector registers.
    fn publish(&self, client: &Arc<McpClient>, raw: Vec<Map<String, Value>>) {
        let mut tools: Vec<ToolDef> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for item in raw {
            let name = wire::str(item.get("name"));
            let Some(name) = name.filter(|n| valid_name(n) && !tools.iter().any(|t| t.name == *n))
            else {
                skipped.push(name.unwrap_or("?").to_owned());
                continue;
            };
            let definition = json!({
                "name": name,
                "description": wire::str(item.get("description")).unwrap_or(""),
                "input_schema": item.get("inputSchema").cloned().unwrap_or(Value::Null),
            });
            match ToolDef::from_json(&definition) {
                Ok(tool) => tools.push(tool),
                Err(_) => skipped.push(name.to_owned()),
            }
        }
        {
            let mut st = self.lock();
            st.client = Some(client.clone());
            st.icon = client.icon();
            let name = clean(Some(&st.entry.name), 40);
            let resource = self.resource(&st.entry);
            let origin = self.origin();
            let command = st.entry.command.clone();
            let instructions = client.instructions();
            self.broker.run(|c| {
                if !skipped.is_empty() {
                    c.log(format!(
                        "{name}: commands skipped (invalid or duplicate name): {}",
                        quoted(&skipped)
                    ));
                }
                let id = match st.conn_id.clone().filter(|id| c.registered(id)) {
                    Some(id) => {
                        c.tools_changed(&id, tools);
                        id
                    }
                    None => c.register(
                        self.outbox.clone(),
                        resource,
                        tools,
                        origin,
                        command.as_deref(),
                    ),
                };
                st.conn_id = Some(id.clone());
                // The server's instructions are not a command: "clak <program> -h" shows them.
                c.set_instructions(&id, instructions);
                if st.phase != ACTIVE {
                    if st.phase == UNREACHABLE {
                        c.log(format!("{name}: reachable again"));
                    }
                    st.active_since = Some(now());
                }
                st.phase = ACTIVE;
                st.error = None;
                c.set_status(&id, None);
                c.touch();
            });
        }
        (self.changed)();
    }

    fn origin(&self) -> Value {
        match &self.target {
            Target::Http(url) => json!({
                "kind": "connector",
                "url": format!("{}{}", uri::origin(url), url.path()),
                "host": uri::host(url),
                "secure": url.scheme() == "https",
                "loopback": uri::is_loopback(url),
            }),
            Target::Stdio(launch) => json!({
                "kind": "connector",
                "transport": "stdio",
                "program": launch.short_name(),
                "command": launch.display(),
            }),
        }
    }

    fn set_phase(&self, phase: &'static str, error: Option<String>) {
        {
            let mut st = self.lock();
            let name = clean(Some(&st.entry.name), 40);
            self.broker.run(|c| {
                if st.phase != phase || st.error != error {
                    let why = error
                        .as_deref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default();
                    c.log(format!("{name}: {phase}{why}"));
                }
                st.phase = phase;
                st.error = error;
                if let Some(id) = &st.conn_id {
                    c.set_status(id, (phase != ACTIVE).then(|| phase.to_owned()));
                }
                c.touch();
            });
        }
        (self.changed)();
    }

    // =========================================================================
    // The commands
    // =========================================================================

    async fn pump(self: Arc<Self>, mut inbox: mpsc::UnboundedReceiver<Value>) {
        loop {
            let message = tokio::select! {
                _ = self.stop.cancelled() => return,
                message = inbox.recv() => message,
            };
            let Some(message) = message else { return };
            match wire::str(message.get("type")) {
                Some("call") => {
                    tokio::spawn(self.clone().call(message));
                }
                Some("cancel") => {
                    if let Some(id) = wire::str(message.get("call_id"))
                        && let Some(cancel) = self
                            .calls
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .remove(id)
                    {
                        cancel.cancel();
                    }
                }
                _ => {}
            }
        }
    }

    async fn call(self: Arc<Self>, message: Value) {
        let call_id = wire::str(message.get("call_id")).unwrap_or("").to_owned();
        let tool = wire::str(message.get("tool")).unwrap_or("").to_owned();
        let arguments = message
            .get("arguments")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let cancel = self.stop.child_token();
        self.calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(call_id.clone(), cancel.clone());
        let result = tokio::select! {
            // Whoever asked is gone, or the hub is stopping: nobody waits for the answer anymore.
            _ = cancel.cancelled() => None,
            result = self.execute(&call_id, &tool, &arguments, &cancel) => Some(result),
        };
        self.calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&call_id);
        let Some(mut result) = result else { return };
        result.insert("type".into(), json!("result"));
        result.insert("call_id".into(), json!(call_id));
        let conn_id = self.lock().conn_id.clone();
        if let Some(conn_id) = conn_id {
            self.broker.run(|c| {
                let _ = c.result(&conn_id, &result);
            });
        }
    }

    async fn execute(
        self: &Arc<Self>,
        call_id: &str,
        tool: &str,
        arguments: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Map<String, Value> {
        let name = self.name();
        let (client, phase, error) = {
            let st = self.lock();
            (st.client.clone(), st.phase, st.error.clone())
        };
        let Some(client) = client else {
            return if phase == SIGN_IN_REQUIRED || phase == REJECTED {
                text(&self.auth_text(), true)
            } else {
                let why = error.map(|e| format!(" ({e})")).unwrap_or_default();
                text(
                    &format!("{name} is not responding{why}: try again shortly."),
                    true,
                )
            };
        };
        let elicit = self.elicit_for(call_id, cancel);
        match client.call_tool(tool, arguments, Some(&elicit)).await {
            Ok(answer) => {
                let mut result = Map::new();
                result.insert(
                    "is_error".into(),
                    Value::Bool(wire::truthy(answer.get("isError"))),
                );
                result.insert("content".into(), content(&answer));
                result
            }
            Err(McpError::Rpc { code, message }) => {
                // A command the server no longer knows: the list has changed.
                if code == -32601 || code == -32602 {
                    self.wake.notify_one();
                }
                text(&format!("{name}: {message}"), true)
            }
            Err(e @ McpError::Auth { .. }) => {
                self.lock().client = None;
                let (phase, reason) = self.auth_phase(&e);
                self.set_phase(phase, Some(reason));
                text(&self.auth_text(), true)
            }
            Err(McpError::SessionLost) => {
                self.lock().client = None;
                self.wake.notify_one();
                text(
                    &format!("The session with {name} expired: try again."),
                    true,
                )
            }
            Err(e) => {
                self.lock().client = None;
                let reason = e.to_string();
                self.set_phase(UNREACHABLE, Some(reason.clone()));
                self.wake.notify_one();
                text(
                    &format!("{name} is not responding ({reason}): try again shortly."),
                    true,
                )
            }
        }
    }

    /// What the user must do for the connector to work again: the agent tells them.
    fn auth_text(&self) -> String {
        let name = self.name();
        if self.lock().phase == SIGN_IN_REQUIRED {
            format!(
                "{name} needs the user to sign in again: ask them to open Clak, MCP tab, and choose Sign in on {name}."
            )
        } else {
            format!("{name} rejected the token: ask the user to change it in Clak, MCP tab.")
        }
    }

    /// The confirmation the server asks for in the middle of a command, with a hub window.
    fn elicit_for(self: &Arc<Self>, call_id: &str, cancel: &CancellationToken) -> Elicit {
        let this = Arc::downgrade(self);
        let call_id = call_id.to_owned();
        let cancel = cancel.clone();
        Arc::new(move |parameters: Map<String, Value>| {
            let this = this.clone();
            let call_id = call_id.clone();
            let cancel = cancel.clone();
            Box::pin(async move {
                let verdict = match this.upgrade() {
                    None => Verdict::NoAnswer,
                    Some(connector) => connector.confirm(&call_id, &parameters, &cancel).await,
                };
                let mut answer = Map::new();
                match verdict {
                    Verdict::Allowed => {
                        answer.insert("action".into(), json!("accept"));
                        let schema = parameters.get("requestedSchema").and_then(Value::as_object);
                        answer.insert(
                            "content".into(),
                            Value::Object(crate::connector::fill(schema)),
                        );
                    }
                    Verdict::Denied => {
                        answer.insert("action".into(), json!("decline"));
                    }
                    _ => {
                        answer.insert("action".into(), json!("cancel"));
                    }
                }
                answer
            })
        })
    }

    async fn confirm(
        &self,
        call_id: &str,
        parameters: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Verdict {
        let conn_id = self.lock().conn_id.clone();
        // The AppConn is copied first: the question is built under the hub's lock, and the
        // connector may have closed in the meantime.
        let app: Option<AppConn> =
            conn_id.and_then(|id| self.broker.run(|c| c.app_by_id(&id).cloned()));
        let Some(app) = app else {
            return Verdict::NoAnswer;
        };
        let message = wire::str(parameters.get("message"))
            .unwrap_or("")
            .to_owned();
        let conn_id = app.conn_id.clone();
        self.broker
            .confirm(
                &conn_id,
                call_id,
                |id, agent, cwd, _core| texts::confirm(id, &app, agent, cwd, &message),
                cancel,
            )
            .await
    }
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The blocks of the answer, as they are; if there is only structured data, as text.
fn content(answer: &Map<String, Value>) -> Value {
    if let Some(Value::Array(blocks)) = answer.get("content")
        && !blocks.is_empty()
    {
        return Value::Array(blocks.clone());
    }
    if let Some(data) = answer.get("structuredContent") {
        return json!([{ "type": "text", "text": wire::text(data) }]);
    }
    json!([])
}

fn text(text: &str, error: bool) -> Map<String, Value> {
    let mut result = Map::new();
    result.insert("is_error".into(), Value::Bool(error));
    result.insert("content".into(), json!([{ "type": "text", "text": text }]));
    result
}
