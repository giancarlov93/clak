//! A real hub in its own folder, with a fake user and fake agents: the `test_agent` field,
//! not the operating system, says who is behind a request.

#![allow(dead_code)]

use clak_hub::{
    AgentIdentity, ConsentKind, ConsentPrompt, ConsentRequest, Hub, HubOptions, Platform,
};
use clak_kit::LiveResource;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub struct Question {
    pub request: ConsentRequest,
    answer: Option<oneshot::Sender<bool>>,
    pub cancel: CancellationToken,
    /// The questions answered yes with "Always allow" ticked.
    always: Arc<Mutex<HashSet<String>>>,
}

impl Question {
    pub fn allow(mut self) {
        let _ = self.answer.take().unwrap().send(true);
    }

    /// Yes, with "Always allow" ticked.
    pub fn allow_always(self) {
        self.always.lock().unwrap().insert(self.request.id.clone());
        self.allow();
    }

    pub fn deny(mut self) {
        let _ = self.answer.take().unwrap().send(false);
    }
}

/// The user, faked: the hub's questions arrive here, and the test decides the answer. With
/// `allow_all` it always says yes, and counts.
pub struct FakePrompt {
    pub allow_all: AtomicBool,
    always: Arc<Mutex<HashSet<String>>>,
    count: AtomicUsize,
    sender: mpsc::UnboundedSender<Question>,
    receiver: tokio::sync::Mutex<mpsc::UnboundedReceiver<Question>>,
}

impl FakePrompt {
    fn new() -> Arc<FakePrompt> {
        let (sender, receiver) = mpsc::unbounded_channel();
        Arc::new(FakePrompt {
            allow_all: AtomicBool::new(false),
            always: Arc::default(),
            count: AtomicUsize::new(0),
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
        })
    }

    /// How many questions the hub has asked so far.
    pub fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    pub fn set_allow_all(&self, on: bool) {
        self.allow_all.store(on, Ordering::SeqCst);
    }

    pub async fn next(&self, kind: ConsentKind) -> Question {
        let question =
            tokio::time::timeout(Duration::from_secs(10), self.receiver.lock().await.recv())
                .await
                .expect("no question")
                .expect("channel closed");
        assert_eq!(
            question.request.kind, kind,
            "question: {}",
            question.request.title
        );
        question
    }

    /// For a while the hub asks nothing.
    pub async fn quiet(&self, milliseconds: u64) {
        tokio::time::sleep(Duration::from_millis(milliseconds)).await;
        if let Ok(question) = self.receiver.lock().await.try_recv() {
            panic!("unexpected question: {}", question.request.title);
        }
    }
}

impl ConsentPrompt for FakePrompt {
    fn always(&self, id: &str) -> bool {
        self.always.lock().unwrap().remove(id)
    }

    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> clak_hub::consent::BoxFuture<Result<bool, String>> {
        self.count.fetch_add(1, Ordering::SeqCst);
        if self.allow_all.load(Ordering::SeqCst) {
            return Box::pin(async { Ok(true) });
        }
        let (answer, receiver) = oneshot::channel();
        let _ = self.sender.send(Question {
            request,
            answer: Some(answer),
            cancel: cancel.clone(),
            always: self.always.clone(),
        });
        Box::pin(async move {
            tokio::select! {
                answer = receiver => Ok(answer.unwrap_or(false)),
                _ = cancel.cancelled() => Ok(false),
            }
        })
    }
}

/// The operating system, faked: the request names the agents, the programs are all the
/// same signed executable.
#[derive(Default)]
pub struct FakePlatform {
    agents: Mutex<HashMap<String, AgentIdentity>>,
    dead: Mutex<HashSet<String>>,
    pids: AtomicU32,
    /// By agent name: the processes above it.
    above: Mutex<HashMap<String, Vec<String>>>,
}

impl FakePlatform {
    pub fn agent(&self, name: &str) -> AgentIdentity {
        self.agents.lock().unwrap()[name].clone()
    }

    /// The agent's process starts now (the others started long ago), below `above`.
    pub fn start_now(&self, name: &str, above: &[&str]) {
        let now = clak_wire::json::now();
        self.agents.lock().unwrap().insert(
            name.to_owned(),
            AgentIdentity {
                pid: 30_000 + self.pids.fetch_add(1, Ordering::SeqCst),
                started: now as i64,
                started_at: Some(now),
                exe: Some(format!(r"C:\agents\{name}.exe")),
                name: name.to_owned(),
                chain: vec![format!("{name}.exe")],
                signed: "unsigned".into(),
                signer: None,
            },
        );
        self.above.lock().unwrap().insert(
            name.to_owned(),
            above.iter().map(|a| (*a).to_owned()).collect(),
        );
    }

    /// The same agent, executable and signature, in a new process: the old one ends.
    pub fn restart(&self, name: &str) {
        let old = self.agent(name);
        self.dead.lock().unwrap().insert(old.key());
        let now = clak_wire::json::now();
        let mut new = old;
        new.pid = 40_000 + self.pids.fetch_add(1, Ordering::SeqCst);
        new.started = now as i64;
        self.agents.lock().unwrap().insert(name.to_owned(), new);
    }

    /// The agent's process ends.
    pub fn kill(&self, name: &str) {
        let key = self.agent(name).key();
        self.dead.lock().unwrap().insert(key);
    }
}

impl Platform for FakePlatform {
    fn agent_of(
        &self,
        _pid: Option<u32>,
        request: &Map<String, Value>,
        _authorized: &HashSet<String>,
    ) -> Option<AgentIdentity> {
        let name = request.get("test_agent")?.as_str()?;
        let mut agents = self.agents.lock().unwrap();
        Some(
            agents
                .entry(name.to_owned())
                .or_insert_with(|| AgentIdentity {
                    pid: 20_000 + self.pids.fetch_add(1, Ordering::SeqCst),
                    started: 133_000_000_000_000_000,
                    started_at: Some(1_700_000_000.0),
                    exe: Some(format!(r"C:\agents\{name}.exe")),
                    name: name.to_owned(),
                    chain: vec![format!("{name}.exe")],
                    signed: "valid".into(),
                    signer: Some("Tests".into()),
                })
                .clone(),
        )
    }

    fn is_alive(&self, agent: &AgentIdentity) -> bool {
        !self.dead.lock().unwrap().contains(&agent.key())
    }

    fn attest_loopback(&self, _peer_port: u16, _local_port: u16) -> Value {
        json!({ "exe": r"C:\tests\notepad.exe", "signed": "valid", "signer": "Tests", "pid": 4242 })
    }

    fn executable_hash(&self, _path: &str) -> Option<String> {
        Some("0123456789abcdef".into())
    }

    fn above(&self, pid: u32) -> Vec<String> {
        let agents = self.agents.lock().unwrap();
        let Some(name) = agents
            .values()
            .find(|a| a.pid == pid)
            .map(|a| a.name.clone())
        else {
            return Vec::new();
        };
        self.above
            .lock()
            .unwrap()
            .get(&name)
            .cloned()
            .unwrap_or_default()
    }
}

/// A test program connected to the hub, with a log of what the hub tells it.
pub struct TestApp {
    pub resource: LiveResource,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, Map<String, Value>)>>,
    /// The arguments of the last command received.
    pub last_arguments: Arc<Mutex<Option<Map<String, Value>>>>,
}

impl TestApp {
    pub async fn event(&self, kind: &str) -> Map<String, Value> {
        let mut events = self.events.lock().await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (seen, message) = events.recv().await.expect("events ended");
                if seen == kind {
                    return message;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {kind} event"))
    }

    pub async fn first(&self, kinds: &[&str]) -> String {
        let mut events = self.events.lock().await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (seen, _) = events.recv().await.expect("events ended");
                if kinds.contains(&seen.as_str()) {
                    return seen;
                }
            }
        })
        .await
        .expect("none of the expected events")
    }
}

pub struct TestHub {
    pub home: PathBuf,
    pub prompt: Arc<FakePrompt>,
    pub platform: Arc<FakePlatform>,
    pub hub: Option<Hub>,
    pub log: Arc<Mutex<Vec<String>>>,
    /// The `progress` lines that arrived before the responses, as «stage: text».
    pub progress: Arc<Mutex<Vec<String>>>,
    apps: Mutex<Vec<LiveResource>>,
    _dir: TempDir,
}

pub struct TempDir(pub PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default, Clone)]
pub struct Setup {
    pub no_prompt: bool,
    pub consent_timeout: Option<Duration>,
    pub call_timeout: Option<Duration>,
    pub denial_memory: Option<Duration>,
}

impl TestHub {
    pub async fn start() -> TestHub {
        TestHub::start_with(Setup::default()).await
    }

    pub async fn start_with(setup: Setup) -> TestHub {
        let home =
            std::env::temp_dir().join(format!("clak-hub-{}", clak_wire::json::random_hex(6)));
        std::fs::create_dir_all(&home).unwrap();
        let prompt = FakePrompt::new();
        let platform = Arc::new(FakePlatform::default());
        let mut options = HubOptions::new(platform.clone());
        if !setup.no_prompt {
            options.consent = Some(prompt.clone());
        }
        if let Some(t) = setup.consent_timeout {
            options.consent_timeout = t;
        }
        if let Some(t) = setup.call_timeout {
            options.call_timeout = t;
        }
        if let Some(t) = setup.denial_memory {
            options.denial_memory = t;
        }
        options.sweep_interval = Duration::from_millis(100);
        options.settings = false;
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        let hub = Hub::try_start(
            &home,
            options,
            Some(Arc::new(move |line| sink.lock().unwrap().push(line))),
        )
        .await
        .unwrap()
        .expect("hub already running");
        TestHub {
            home: home.clone(),
            prompt,
            platform,
            hub: Some(hub),
            log,
            progress: Arc::new(Mutex::new(Vec::new())),
            apps: Mutex::new(Vec::new()),
            _dir: TempDir(home),
        }
    }

    pub fn hub(&self) -> &Hub {
        self.hub.as_ref().unwrap()
    }

    // --- the agent side ----------------------------------------------------------

    /// Only the request: the response can be read, or not waited for, as one likes.
    pub async fn open(
        &self,
        agent: Option<&str>,
        op: &str,
        body: Option<Value>,
    ) -> BufReader<Control> {
        let mut request = body
            .and_then(|b| b.as_object().cloned())
            .unwrap_or_default();
        request.insert("type".into(), json!("request"));
        request.insert("op".into(), json!(op));
        request.entry("cwd").or_insert(json!(r"C:\Projects\demo"));
        if let Some(agent) = agent {
            request.insert("test_agent".into(), json!(agent));
        }
        let mut stream = connect(self.hub().control()).await;
        stream
            .write_all(&clak_wire::json::encode_line(&Value::Object(request)))
            .await
            .unwrap();
        stream.flush().await.unwrap();
        BufReader::new(stream)
    }

    /// A request, as the command line makes it: one connection, one response.
    pub async fn ask(&self, agent: &str, op: &str, body: Option<Value>) -> Map<String, Value> {
        let mut reader = self.open(Some(agent), op, body).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let mut line = String::new();
                let n = reader.read_line(&mut line).await.unwrap();
                assert!(n > 0, "the hub closed without answering");
                let message = clak_wire::json::object(line.trim().as_bytes()).unwrap();
                if message.get("type").and_then(Value::as_str) == Some("progress") {
                    self.progress.lock().unwrap().push(format!(
                        "{}: {}",
                        message["stage"].as_str().unwrap(),
                        message["text"].as_str().unwrap()
                    ));
                    continue;
                }
                assert_eq!(
                    message.get("type").and_then(Value::as_str),
                    Some("response")
                );
                return message;
            }
        })
        .await
        .expect("no response")
    }

    pub async fn call(
        &self,
        agent: &str,
        program: &str,
        tool: &str,
        arguments: Option<Value>,
        flags: Option<Value>,
    ) -> Map<String, Value> {
        let mut body = json!({ "resource": program, "tool": tool, "arguments": arguments.unwrap_or(json!({})) });
        if let Some(flags) = flags {
            body["flags"] = flags;
        }
        self.ask(agent, "call", Some(body)).await
    }

    /// The panel's first message of the wanted type, after sending `asks`.
    pub async fn panel(&self, asks: &[Value], answer: &str) -> Map<String, Value> {
        let mut stream = connect(self.hub().control()).await;
        let mut text = b"{\"type\":\"watch\"}\n".to_vec();
        for ask in asks {
            text.extend(clak_wire::json::encode_line(ask));
        }
        stream.write_all(&text).await.unwrap();
        stream.flush().await.unwrap();
        let mut reader = BufReader::new(stream);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut line = String::new();
                assert!(
                    reader.read_line(&mut line).await.unwrap() > 0,
                    "the hub closed"
                );
                if let Some(message) = clak_wire::json::object(line.trim().as_bytes())
                    && message.get("type").and_then(Value::as_str) == Some(answer)
                {
                    return message;
                }
            }
        })
        .await
        .expect("no response from the panel")
    }

    /// The hub's snapshot, as the panel receives it.
    pub async fn snapshot(&self) -> Map<String, Value> {
        self.panel(&[], "state").await
    }

    // --- the program side --------------------------------------------------------

    /// A text notepad with two commands, to connect. The test grants the program's permission.
    pub fn new_app(&self, app: &str, title: &str) -> TestApp {
        let resource = LiveResource::new(app, title, None, Some(self.home.clone()));
        let (sender, receiver) = mpsc::unbounded_channel();
        resource.on_event(move |kind, message| {
            let _ = sender.send((kind.to_owned(), message.clone()));
        });
        let text = Arc::new(Mutex::new(String::from("milk")));
        let read = text.clone();
        resource.tool("get_text", "Returns the text.", None, move |_| {
            let value = read.lock().unwrap().clone();
            async move { clak_kit::text(value) }
        });
        let last = Arc::new(Mutex::new(None));
        let seen = last.clone();
        resource.tool(
            "append_text",
            "Adds a line.",
            Some(json!({"type":"object","properties":{"text":{"type":"string"},"times":{"type":"integer"}},"required":["text"]})),
            move |arguments| {
                let line = arguments.get("text").and_then(Value::as_str).unwrap_or("").to_owned();
                *seen.lock().unwrap() = Some(arguments);
                text.lock().unwrap().push_str(&format!("\n{line}"));
                async { clak_kit::text("done") }
            },
        );
        self.apps.lock().unwrap().push(resource.clone());
        TestApp {
            resource,
            events: tokio::sync::Mutex::new(receiver),
            last_arguments: last,
        }
    }

    /// Connects the program and, if the hub asks, allows it.
    pub async fn register(&self) -> TestApp {
        self.register_as("Test Notepad", "shopping.txt").await
    }

    pub async fn register_as(&self, app: &str, title: &str) -> TestApp {
        let test = self.new_app(app, title);
        test.resource.connect().await.unwrap();
        if test.first(&["waiting", "registered"]).await == "waiting" {
            self.prompt.next(ConsentKind::App).await.allow();
            test.event("registered").await;
        }
        test
    }

    pub async fn stop(mut self) {
        let apps: Vec<LiveResource> = self.apps.lock().unwrap().drain(..).collect();
        for app in apps {
            app.close("resource_closed").await;
        }
        if let Some(hub) = self.hub.take() {
            hub.stop().await;
        }
    }
}

#[cfg(windows)]
pub type Control = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(unix)]
pub type Control = tokio::net::UnixStream;

pub async fn connect(control: &str) -> Control {
    for _ in 0..50 {
        #[cfg(windows)]
        let opened = tokio::net::windows::named_pipe::ClientOptions::new().open(control);
        #[cfg(unix)]
        let opened = tokio::net::UnixStream::connect(control).await;
        if let Ok(stream) = opened {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the control channel does not answer");
}

/// Waits for the condition to become true.
pub async fn wait_until<F, Fut>(mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if condition().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the condition never became true");
}

pub fn code(response: &Map<String, Value>) -> Option<&str> {
    response.get("code").and_then(Value::as_str)
}

pub fn ok(response: &Map<String, Value>) -> bool {
    response.get("ok").and_then(Value::as_bool) == Some(true)
}

pub fn content(response: &Map<String, Value>) -> Option<&str> {
    response.get("content").and_then(Value::as_str)
}

pub fn assert_read_write<T: AsyncRead + AsyncWrite>() {}
