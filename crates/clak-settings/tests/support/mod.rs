//! A real hub in its own folder, with connectors, APIs and settings on top, the fake user
//! and fake agents: who's behind a request is given by the `test_agent` field, not by the
//! operating system. It's the `clak-connectors` test harness, plus the APIs and
//! `settings`.

#![allow(dead_code)]

use clak_apis::{ApiHub, ApiStore, rest};
use clak_connectors::{ConnectorHub, ConnectorStore};
use clak_hub::{
    AgentIdentity, ConnectorTiming, ConsentKind, ConsentPrompt, ConsentRequest, Hub, HubOptions,
    Platform,
};
use clak_platform::MemoryBox;
use clak_settings::HubSettings;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub struct Question {
    pub request: ConsentRequest,
    answer: Option<oneshot::Sender<bool>>,
}

impl Question {
    pub fn allow(mut self) {
        let _ = self.answer.take().unwrap().send(true);
    }

    pub fn deny(mut self) {
        let _ = self.answer.take().unwrap().send(false);
    }

    /// The value of the "what" line with this label.
    pub fn what(&self, label: &str) -> Option<&str> {
        self.request
            .what
            .iter()
            .find(|l| l.label == label)
            .map(|l| l.value.as_str())
    }

    pub fn who(&self, label: &str) -> Option<&str> {
        self.request
            .who
            .iter()
            .find(|l| l.label == label)
            .map(|l| l.value.as_str())
    }
}

/// The fake user: the hub's questions arrive here, and the test decides the answer. With
/// `allow_all` it always says yes, and counts.
pub struct FakePrompt {
    allow_all: AtomicBool,
    count: AtomicUsize,
    sender: mpsc::UnboundedSender<Question>,
    receiver: tokio::sync::Mutex<mpsc::UnboundedReceiver<Question>>,
}

impl FakePrompt {
    fn new() -> Arc<FakePrompt> {
        let (sender, receiver) = mpsc::unbounded_channel();
        Arc::new(FakePrompt {
            allow_all: AtomicBool::new(false),
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
        });
        Box::pin(async move {
            tokio::select! {
                answer = receiver => Ok(answer.unwrap_or(false)),
                _ = cancel.cancelled() => Ok(false),
            }
        })
    }
}

/// The fake operating system: the request names the agents.
#[derive(Default)]
pub struct FakePlatform {
    agents: Mutex<HashMap<String, AgentIdentity>>,
    pids: AtomicU32,
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
                    exe: Some(format!(r"C:\agenti\{name}.exe")),
                    name: name.to_owned(),
                    chain: vec![format!("{name}.exe")],
                    signed: "valid".into(),
                    signer: Some("Prove".into()),
                })
                .clone(),
        )
    }

    fn is_alive(&self, _agent: &AgentIdentity) -> bool {
        true
    }

    fn attest_loopback(&self, _peer_port: u16, _local_port: u16) -> Value {
        json!({ "exe": r"C:\prove\blocco.exe", "signed": "valid", "signer": "Prove", "pid": 4242 })
    }

    fn executable_hash(&self, _path: &str) -> Option<String> {
        Some("0123456789abcdef".into())
    }
}

pub struct TestHub {
    pub home: PathBuf,
    pub prompt: Arc<FakePrompt>,
    pub hub: Option<Hub>,
    pub connectors: Arc<ConnectorHub>,
    pub apis: Arc<ApiHub>,
    pub settings: Arc<HubSettings>,
    pub log: Arc<Mutex<Vec<String>>>,
    /// The `progress` lines that arrived before the responses, as "stage: text".
    pub progress: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone, Default)]
pub struct Setup {
    /// A hub without an interface: nobody to ask.
    pub no_prompt: bool,
}

impl TestHub {
    pub async fn start() -> TestHub {
        TestHub::start_with(Setup::default()).await
    }

    pub async fn start_with(setup: Setup) -> TestHub {
        let home =
            std::env::temp_dir().join(format!("clak-settings-{}", clak_wire::json::random_hex(6)));
        std::fs::create_dir_all(&home).unwrap();
        let prompt = FakePrompt::new();
        let mut options = HubOptions::new(Arc::new(FakePlatform::default()));
        if !setup.no_prompt {
            options.consent = Some(prompt.clone());
        }
        options.sweep_interval = Duration::from_millis(100);
        let timing = ConnectorTiming {
            retry_min: Duration::from_millis(100),
            retry_max: Duration::from_millis(400),
            refresh: Duration::from_secs(600),
        };
        options.connector_timing = timing;
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
        let store = Arc::new(ConnectorStore::new(&home, crate::fake::test_box(&home)));
        let connectors = ConnectorHub::new(hub.broker().clone(), store, timing);
        connectors.start_all();
        let apis = ApiHub::new(
            hub.broker().clone(),
            Arc::new(ApiStore::new(&home, Arc::new(MemoryBox::new()))),
            rest::new_http_client().unwrap(),
        );
        apis.start_all();
        let settings = HubSettings::new(hub.broker().clone(), connectors.clone(), apis.clone());
        settings.start();
        TestHub {
            home,
            prompt,
            hub: Some(hub),
            connectors,
            apis,
            settings,
            log,
            progress: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn hub(&self) -> &Hub {
        self.hub.as_ref().unwrap()
    }

    pub fn log_text(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    pub fn progress_text(&self) -> String {
        self.progress.lock().unwrap().join("\n")
    }

    /// A request, as the command line makes it: one connection, one response.
    pub async fn ask(&self, agent: &str, op: &str, body: Option<Value>) -> Map<String, Value> {
        let mut request = body
            .and_then(|b| b.as_object().cloned())
            .unwrap_or_default();
        request.insert("type".into(), json!("request"));
        request.insert("op".into(), json!(op));
        request.entry("cwd").or_insert(json!(r"C:\Progetti\prova"));
        request.insert("test_agent".into(), json!(agent));
        let mut stream = connect(self.hub().control()).await;
        stream
            .write_all(&clak_wire::json::encode_line(&Value::Object(request)))
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let mut reader = BufReader::new(stream);
        tokio::time::timeout(Duration::from_secs(30), async {
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
                return message;
            }
        })
        .await
        .expect("no response")
    }

    /// A command with its JSON arguments.
    pub async fn call(
        &self,
        agent: &str,
        program: &str,
        tool: &str,
        arguments: Value,
    ) -> Map<String, Value> {
        let body = json!({ "resource": program, "tool": tool, "arguments": arguments });
        self.ask(agent, "call", Some(body)).await
    }

    /// A command as the command line writes it: `--name value`, which the hub translates
    /// with the command's schema.
    pub fn call_flags(
        &self,
        agent: &str,
        program: &str,
        tool: &str,
        flags: &[(&str, &str)],
    ) -> impl Future<Output = Map<String, Value>> + '_ {
        let flags: Vec<Value> = flags
            .iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect();
        let body = json!({ "resource": program, "tool": tool, "flags": flags });
        let agent = agent.to_owned();
        async move { self.ask(&agent, "call", Some(body)).await }
    }

    /// The hub snapshot, as the panel receives it.
    pub fn snapshot(&self) -> Value {
        self.hub().broker().run(|c| c.snapshot())
    }

    /// The connector in the snapshot, as soon as it's in this state.
    pub async fn connector(&self, id: &str, state: &str) -> Value {
        for _ in 0..200 {
            let found = self.snapshot()["connectors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .cloned();
            if let Some(found) = found
                && found["state"] == state
            {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let last = self.snapshot()["connectors"].clone();
        panic!("the connector never reaches the expected state: {last}");
    }

    pub async fn stop(mut self) {
        self.settings.stop("hub closed").await;
        self.connectors.stop("hub closed").await;
        self.apis.stop().await;
        if let Some(hub) = self.hub.take() {
            hub.stop().await;
        }
        let _ = std::fs::remove_dir_all(&self.home);
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
    panic!("the control channel doesn't answer");
}

pub fn is_error(response: &Map<String, Value>) -> bool {
    response.get("is_error").and_then(Value::as_bool) == Some(true)
}

pub fn code(response: &Map<String, Value>) -> Option<&str> {
    response.get("code").and_then(Value::as_str)
}

/// The text of the response: programs pass it in blocks, like MCP.
pub fn content(response: &Map<String, Value>) -> String {
    match response.get("content") {
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| b["text"].as_str().unwrap_or("").to_owned())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

/// The response of a successful command, read as JSON.
pub fn result(response: &Map<String, Value>) -> Value {
    assert!(!is_error(response), "{}", content(response));
    assert!(
        code(response).is_none(),
        "{}",
        Value::Object(response.clone())
    );
    clak_wire::json::parse_str(&content(response)).expect("the response isn't JSON")
}
