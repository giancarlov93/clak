//! The thread between the hub and what the user sees: tray, notifications, pages.
//!
//! The hub usually lives in here. If I find one already running (another copy of the app, the
//! daemon) I watch it from the control channel as a panel would, and as soon as it goes away I
//! try to take its place. In both cases the same messages
//! arrive — `log`, `state`, the replies to `describe` and `history` — and go to all the
//! open pages as a `hub` event.

use crate::prompt::WindowPrompt;
use crate::{notices, tray};
use clak_hub::{LogSink, Outbox};
use clak_setup::preferences::Preferences;
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc;

const BACKLOG: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// The hub is in here.
    Local,
    /// The hub is in another process: I watch it.
    Remote,
    /// No hub.
    Offline,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Local => "local",
            Mode::Remote => "remote",
            Mode::Offline => "offline",
        }
    }
}

enum Link {
    None,
    Local(clak_host::ClakHost),
    Remote(clak_wire::ipc::Stream),
}

struct Inner {
    link: Link,
    mode: Mode,
    state: Value,
    log: VecDeque<Value>,
    /// The first state after connecting reports nothing: it is just the start.
    fresh: bool,
}

pub struct Bridge {
    app: AppHandle,
    home: PathBuf,
    pub prefs: Preferences,
    prompt: WindowPrompt,
    inner: Mutex<Inner>,
    messages: mpsc::UnboundedSender<Value>,
    quitting: tokio_util::sync::CancellationToken,
}

fn offline() -> Value {
    json!({ "type": "state", "offline": true, "sessions": [], "resources": [], "grants": [], "pending": [], "apps": [] })
}

impl Bridge {
    pub fn new(app: AppHandle, home: PathBuf, prompt: WindowPrompt) -> Arc<Bridge> {
        let (messages, receiver) = mpsc::unbounded_channel();
        let bridge = Arc::new(Bridge {
            app,
            prefs: Preferences::new(&home),
            home,
            prompt,
            inner: Mutex::new(Inner {
                link: Link::None,
                mode: Mode::Offline,
                state: offline(),
                log: VecDeque::new(),
                fresh: true,
            }),
            messages,
            quitting: tokio_util::sync::CancellationToken::new(),
        });
        tauri::async_runtime::spawn(Bridge::deliver(bridge.clone(), receiver));
        bridge
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn home(&self) -> &PathBuf {
        &self.home
    }

    pub fn mode(&self) -> Mode {
        self.inner().mode
    }

    /// What a freshly opened page needs to know: how it is connected, the state, the log.
    pub fn hello(&self) -> Value {
        let inner = self.inner();
        json!({
            "mode": inner.mode.as_str(),
            "home": self.home.to_string_lossy(),
            "state": inner.state,
            "log": inner.log.iter().collect::<Vec<_>>(),
        })
    }

    pub fn state(&self) -> Value {
        self.inner().state.clone()
    }

    /// What the panel asks the hub: revoke, forget, describe, history, a call in full (only
    /// with the hub inside the app).
    pub fn send(&self, message: Map<String, Value>) {
        let mut inner = self.inner();
        match &mut inner.link {
            Link::Local(host) => {
                let (outbox, mut replies) = Outbox::channel();
                clak_hub::control_channel::panel_message(host.broker(), &message, &outbox, true);
                drop(outbox);
                while let Ok(reply) = replies.try_recv() {
                    let _ = self.messages.send(reply);
                }
            }
            Link::Remote(stream) => {
                let _ = stream.write_all(&clak_wire::json::encode_line(&Value::Object(message)));
            }
            Link::None => {}
        }
    }

    /// Starts the hub here, or watches the one already running; and so on while the app is open.
    pub async fn run(self: Arc<Bridge>, own: bool) {
        loop {
            if self.quitting.is_cancelled() {
                return;
            }
            if own && self.start().await {
                return;
            }
            match self.clone().observe().await {
                Ok(()) => {}
                Err(_) => {
                    self.set_mode(Mode::Offline);
                    let _ = self.messages.send(offline());
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                _ = self.quitting.cancelled() => return,
            }
        }
    }

    fn set_mode(&self, mode: Mode) {
        let changed = {
            let mut inner = self.inner();
            let changed = inner.mode != mode;
            inner.mode = mode;
            if changed {
                inner.fresh = true;
            }
            changed
        };
        if changed {
            let _ = self.app.emit("hub-mode", mode.as_str());
        }
    }

    /// The hub in this process. False if there is already another one.
    async fn start(&self) -> bool {
        let mut options = clak_host::options();
        options.consent = Some(Arc::new(self.prompt.clone()));
        let log: LogSink = Arc::new(move |line: String| {
            eprintln!("[clak {}] {line}", chrono::Local::now().format("%H:%M:%S"));
        });
        let host = match clak_host::ClakHost::try_start(&self.home, options, Some(log)).await {
            Ok(Some(host)) => host,
            Ok(None) => return false,
            Err(e) => {
                eprintln!("the hub couldn't start: {e}");
                return false;
            }
        };
        // Whoever quit the app from the menu paused it; reopening it resumes it.
        let _ = std::fs::remove_file(clak_wire::location::pause_flag(&self.home));
        let (outbox, mut receiver) = Outbox::channel();
        host.broker().run(|c| c.watch("app", outbox));
        {
            let mut inner = self.inner();
            inner.link = Link::Local(host);
        }
        self.set_mode(Mode::Local);
        let messages = self.messages.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(message) = receiver.recv().await {
                let _ = messages.send(message);
            }
        });
        true
    }

    /// Watches another process's hub for as long as it is there. Error if there is none.
    async fn observe(self: Arc<Bridge>) -> std::io::Result<()> {
        let home = self.home.clone();
        let control = clak_wire::location::Lockfile::read(&home)
            .map(|l| l.control)
            .unwrap_or_else(|| clak_wire::location::control(&home));
        let mut stream = tokio::task::spawn_blocking(move || {
            clak_wire::ipc::connect(&control, Duration::from_secs(2))
        })
        .await
        .map_err(std::io::Error::other)??;
        stream.write_all(b"{\"type\":\"watch\"}\n")?;
        let reader = stream.try_clone()?;
        self.inner().link = Link::Remote(stream);
        self.set_mode(Mode::Remote);
        let messages = self.messages.clone();
        let quitting = self.quitting.clone();
        let read = tokio::task::spawn_blocking(move || {
            let mut lines = BufReader::new(reader);
            let mut line = String::new();
            while !quitting.is_cancelled() {
                line.clear();
                match lines.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if let Ok(message) = clak_wire::json::parse(line.trim().as_bytes()) {
                    let _ = messages.send(message);
                }
            }
        });
        let _ = read.await;
        self.inner().link = Link::None;
        Ok(())
    }

    /// Every hub message: remembered, reported (tray, notifications) and sent to the pages.
    async fn deliver(self: Arc<Bridge>, mut receiver: mpsc::UnboundedReceiver<Value>) {
        while let Some(message) = receiver.recv().await {
            match message.get("type").and_then(Value::as_str) {
                Some("state") => {
                    let (before, mode) = {
                        let mut inner = self.inner();
                        let before = if inner.fresh || inner.state.get("offline").is_some() {
                            None
                        } else {
                            Some(inner.state.clone())
                        };
                        inner.fresh = false;
                        inner.state = message.clone();
                        (before, inner.mode)
                    };
                    if self.prefs.notifications() {
                        for notice in notices::between(before.as_ref(), &message) {
                            let _ = self
                                .app
                                .notification()
                                .builder()
                                .title(&notice.title)
                                .body(&notice.text)
                                .show();
                        }
                    }
                    tray::update(&self.app, &message, mode == Mode::Offline);
                }
                Some("log") => {
                    let mut inner = self.inner();
                    for item in message
                        .get("items")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        inner.log.push_back(item.clone());
                        if inner.log.len() > BACKLOG {
                            inner.log.pop_front();
                        }
                    }
                }
                _ => {}
            }
            let _ = self.app.emit("hub", &message);
        }
    }

    /// Closes everything: the hub (which removes the lockfile) and the channel to the other process.
    pub async fn stop(&self) {
        self.quitting.cancel();
        let link = std::mem::replace(&mut self.inner().link, Link::None);
        match link {
            Link::Local(host) => host.stop().await,
            Link::Remote(stream) => stream.shutdown(),
            Link::None => {}
        }
    }

    /// The MCP servers, only if the hub is here: watching another process's hub, they can't be changed.
    pub fn connectors(&self) -> Option<Arc<clak_connectors::ConnectorHub>> {
        match &self.inner().link {
            Link::Local(host) => Some(host.connectors().clone()),
            _ => None,
        }
    }

    /// The REST APIs, likewise.
    pub fn apis(&self) -> Option<Arc<clak_apis::ApiHub>> {
        match &self.inner().link {
            Link::Local(host) => host.apis().cloned(),
            _ => None,
        }
    }

    pub fn prompt(&self) -> &WindowPrompt {
        &self.prompt
    }
}

/// The bridge, from Tauri's state.
pub fn of(app: &AppHandle) -> Arc<Bridge> {
    app.state::<Arc<Bridge>>().inner().clone()
}
