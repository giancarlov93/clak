//! A real hub in its own folder, with the fake user and fake agents (like the `clak-hub`
//! tests), and the hub's APIs on top, with secrets in memory.

#![allow(dead_code)]

use clak_apis::{ApiHub, ApiStore, rest};
use clak_hub::{
    AgentIdentity, ConsentKind, ConsentPrompt, ConsentRequest, Hub, HubOptions, Platform,
};
use clak_platform::MemoryBox;
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
}

/// The fake user: questions arrive here; with `allow_all` it always says yes, and counts.
pub struct FakePrompt {
    pub allow_all: AtomicBool,
    count: AtomicUsize,
    sender: mpsc::UnboundedSender<Question>,
    receiver: tokio::sync::Mutex<mpsc::UnboundedReceiver<Question>>,
}

impl FakePrompt {
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

/// The fake operating system: the request names the agent (`test_agent`).
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
    pub apis: Arc<ApiHub>,
    pub secrets: Arc<MemoryBox>,
    pub log: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
pub struct Setup {
    pub call_timeout: Option<Duration>,
}

impl TestHub {
    pub async fn start() -> TestHub {
        TestHub::start_with(Setup::default()).await
    }

    pub async fn start_with(setup: Setup) -> TestHub {
        let home =
            std::env::temp_dir().join(format!("clak-apis-hub-{}", clak_wire::json::random_hex(6)));
        std::fs::create_dir_all(&home).unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        let prompt = Arc::new(FakePrompt {
            allow_all: AtomicBool::new(false),
            count: AtomicUsize::new(0),
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
        });
        let mut options = HubOptions::new(Arc::new(FakePlatform::default()));
        options.consent = Some(prompt.clone());
        options.settings = false;
        options.sweep_interval = Duration::from_millis(100);
        if let Some(t) = setup.call_timeout {
            options.call_timeout = t;
        }
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
        let secrets = Arc::new(MemoryBox::new());
        let store = Arc::new(ApiStore::new(&home, secrets.clone()));
        let apis = ApiHub::new(
            hub.broker().clone(),
            store,
            rest::new_http_client().unwrap(),
        );
        apis.start_all();
        TestHub {
            home,
            prompt,
            hub: Some(hub),
            apis,
            secrets,
            log,
        }
    }

    pub fn hub(&self) -> &Hub {
        self.hub.as_ref().unwrap()
    }

    /// The APIs again, on the same store: like a hub restarting.
    pub fn reopen(&self) -> Arc<ApiHub> {
        let store = Arc::new(ApiStore::new(&self.home, self.secrets.clone()));
        ApiHub::new(
            self.hub().broker().clone(),
            store,
            rest::new_http_client().unwrap(),
        )
    }

    /// A request, as the command line makes it: one connection, one response.
    pub async fn ask(&self, agent: &str, op: &str, body: Value) -> Map<String, Value> {
        let mut request = body.as_object().cloned().unwrap_or_default();
        request.insert("type".into(), json!("request"));
        request.insert("op".into(), json!(op));
        request.insert("cwd".into(), json!(r"C:\Progetti\prova"));
        request.insert("test_agent".into(), json!(agent));
        let mut stream = connect(self.hub().control()).await;
        stream
            .write_all(&clak_wire::json::encode_line(&Value::Object(request)))
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let mut reader = BufReader::new(stream);
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let mut line = String::new();
                assert!(
                    reader.read_line(&mut line).await.unwrap() > 0,
                    "the hub closed without answering"
                );
                let message = clak_wire::json::object(line.trim().as_bytes()).unwrap();
                if message.get("type").and_then(Value::as_str) == Some("response") {
                    return message;
                }
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
        arguments: Value,
    ) -> Map<String, Value> {
        self.ask(
            agent,
            "call",
            json!({ "resource": program, "tool": tool, "arguments": arguments }),
        )
        .await
    }

    /// The hub snapshot, as the panel receives it.
    pub async fn snapshot(&self) -> Map<String, Value> {
        let mut stream = connect(self.hub().control()).await;
        stream.write_all(b"{\"type\":\"watch\"}\n").await.unwrap();
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
                    && message.get("type").and_then(Value::as_str) == Some("state")
                {
                    return message;
                }
            }
        })
        .await
        .expect("no snapshot")
    }

    /// The API `id` in the snapshot, once `until` is true.
    pub async fn api(&self, id: &str, until: impl Fn(&Value) -> bool) -> Value {
        let mut last = Value::Null;
        for _ in 0..100 {
            let snapshot = self.snapshot().await;
            if let Some(api) = snapshot["apis"]
                .as_array()
                .and_then(|a| a.iter().find(|a| a["id"] == id))
            {
                if until(api) {
                    return api.clone();
                }
                last = api.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the API never reaches the expected state: {last}");
    }

    pub async fn stop(mut self) {
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

/// The text of all the blocks of a hub response, one per line.
pub fn text(response: &Map<String, Value>) -> String {
    match response.get("content") {
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| b["text"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

pub fn is_error(response: &Map<String, Value>) -> bool {
    response.get("is_error").and_then(Value::as_bool) == Some(true)
}

pub fn code(response: &Map<String, Value>) -> Option<&str> {
    response.get("code").and_then(Value::as_str)
}

/// A browser that follows the sign-in page up to the redirect back to loopback.
pub fn browser() -> clak_connectors::OpenBrowser {
    Arc::new(|url: url::Url| {
        Box::pin(async move {
            tokio::spawn(async move {
                let http = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap();
                let page = http.get(url).send().await.unwrap();
                let back = page
                    .headers()
                    .get("location")
                    .and_then(|l| l.to_str().ok())
                    .expect("no redirect back")
                    .to_owned();
                let _ = http.get(back).send().await;
            });
            Ok(())
        })
    })
}
