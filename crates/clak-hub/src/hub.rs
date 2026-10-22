//! The listening hub: one per folder, shared by all the agents and all the programs on the
//! machine.
//!
//! The system decides who the hub is, atomically: on Windows the first instance of the
//! control pipe (whoever creates it first is the hub), on the Mac a `flock` on `hub.lock`,
//! held for the hub's whole life — a socket left over by a dead hub says nothing, a lock
//! does, because the system releases it when the process dies.

use crate::app_channel;
use crate::consent::HubOptions;
use crate::core::Broker;
use crate::permissions::AppPermissions;
use clak_wire::json;
use clak_wire::location::{self, Lockfile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Who receives the log, one line at a time and in order, outside the hub's lock.
pub type LogSink = Arc<dyn Fn(String) + Send + Sync>;

pub struct Hub {
    home: PathBuf,
    control: String,
    ws_port: u16,
    broker: Broker,
    stop: CancellationToken,
    tracker: TaskTracker,
    tasks: Vec<JoinHandle<()>>,
    logging: Option<JoinHandle<()>>,
    #[cfg(unix)]
    lock: std::fs::File,
}

impl Hub {
    /// Starts the hub on this folder, or returns nothing if there already is one.
    // The tasks are pushed one by one: which listener comes first depends on the system.
    #[allow(clippy::vec_init_then_push)]
    pub async fn try_start(
        home: &Path,
        options: HubOptions,
        log: Option<LogSink>,
    ) -> std::io::Result<Option<Hub>> {
        std::fs::create_dir_all(home)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The folder belongs to the user and nobody else: the token is inside.
            let _ = std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700));
        }

        #[cfg(windows)]
        let (first, control) = {
            let control = location::computed_control(home);
            match crate::control_channel::pipe::open(&control, true) {
                Ok(first) => (first, control),
                // The pipe already exists: there is already a hub.
                Err(_) => return Ok(None),
            }
        };
        #[cfg(unix)]
        let (lock, listener, control) = {
            use std::os::unix::fs::OpenOptionsExt;
            use std::os::unix::io::AsRawFd;
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(location::hub_lock(home))?;
            // SAFETY: the descriptor is alive as long as `lock` is.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Ok(None);
            }
            let control = location::computed_control(home);
            let _ = std::fs::remove_file(&control);
            let listener = tokio::net::UnixListener::bind(&control)?;
            (lock, listener, control)
        };

        let stop = CancellationToken::new();
        let tracker = TaskTracker::new();
        let (sink, logging) = match log {
            Some(log) => {
                let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                let task = tokio::spawn(async move {
                    while let Some(line) = rx.recv().await {
                        // Whoever listens to the log cannot stop the hub.
                        let log = log.clone();
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                            log(line)
                        }));
                    }
                });
                (Some(tx), Some(task))
            }
            None => (None, None),
        };
        let can_ask = options.consent.is_some();
        let sweep_interval = options.sweep_interval;
        let permissions = AppPermissions::load(home.join("apps.json"));
        let broker = Broker::new(control.clone(), options, permissions, sink, stop.clone());
        let usage = crate::usage::Usage::load(home.join("usage.json"));
        broker.run(|c| c.usage = usage);

        let (listener_ws, ws_port) = app_channel::bind().await?;
        broker.run(|c| c.ws_port = ws_port);

        // The token lives in a file only the user can read: it is what stops any web page from
        // connecting on its own. It is written aside and renamed, so whoever waits for the
        // lockfile never reads a half-written one.
        let lockfile = Lockfile {
            ws_port,
            token: broker.run(|c| c.token().to_owned()),
            control: control.clone(),
            pid: std::process::id(),
            protocol: clak_wire::PROTOCOL,
        };
        let draft = location::lockfile(home).with_extension("json.tmp");
        write_private(&draft, &json::encode_pretty(&lockfile.to_json()))?;

        // Listen first, then announce it: whoever finds the lockfile must be able to get in at once.
        let mut tasks = Vec::new();
        #[cfg(windows)]
        tasks.push(tokio::spawn(crate::control_channel::pipe::listen(
            first,
            control.clone(),
            broker.clone(),
            tracker.clone(),
            stop.clone(),
        )));
        #[cfg(unix)]
        tasks.push(tokio::spawn(crate::control_channel::socket::listen(
            listener,
            broker.clone(),
            tracker.clone(),
            stop.clone(),
        )));
        tasks.push(tokio::spawn(app_channel::accept(
            listener_ws,
            broker.clone(),
            tracker.clone(),
            stop.clone(),
        )));
        tasks.push(tokio::spawn(sweep(
            broker.clone(),
            sweep_interval,
            stop.clone(),
        )));
        std::fs::rename(&draft, location::lockfile(home))?;
        broker.run(|c| {
            c.log(format!(
                "listening — programs ws://127.0.0.1:{ws_port}  agents {control}{}",
                if can_ask {
                    ""
                } else {
                    "  (no interface: no new permissions)"
                }
            ))
        });

        Ok(Some(Hub {
            home: home.to_path_buf(),
            control,
            ws_port,
            broker,
            stop,
            tracker,
            tasks,
            logging,
            #[cfg(unix)]
            lock,
        }))
    }

    pub fn broker(&self) -> &Broker {
        &self.broker
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The address of the control channel.
    pub fn control(&self) -> &str {
        &self.control
    }

    /// The loopback port programs connect to.
    pub fn ws_port(&self) -> u16 {
        self.ws_port
    }

    /// Stops listening, drops whoever is connected and removes the lockfile.
    pub async fn stop(self) {
        // The lockfile goes *before* the channel: while the channel is ours no other hub can be
        // born, so the file being deleted is surely ours.
        let lockfile = location::lockfile(&self.home);
        for path in [lockfile.with_extension("json.tmp"), lockfile] {
            let _ = std::fs::remove_file(path);
        }
        // The last counts, not yet written.
        self.broker
            .run(|c| c.usage.flush(clak_wire::json::now(), true));
        self.stop.cancel();
        self.tracker.close();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            for task in self.tasks {
                let _ = task.await;
            }
            self.tracker.wait().await;
        })
        .await;
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.control);
            drop(self.lock);
        }
        drop(self.broker);
        if let Some(logging) = self.logging {
            let _ = tokio::time::timeout(Duration::from_secs(2), logging).await;
        }
    }
}

/// An agent is a process: when it exits, its permissions end. This is checked every few
/// seconds, outside the lock (asking the system is costly).
async fn sweep(broker: Broker, interval: Duration, stop: CancellationToken) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = stop.cancelled() => return,
        }
        let sessions = broker.run(|c| c.sessions());
        let platform = broker.options().platform.clone();
        let gone = tokio::task::spawn_blocking(move || {
            sessions
                .into_iter()
                .filter(|(_, agent)| !platform.is_alive(agent))
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for id in gone {
            broker.run(|c| c.forget_session(Some(&id), "harness_gone"));
        }
    }
}

/// A file only the user can read (on the Mac; on Windows the profile folder already is).
fn write_private(path: &Path, content: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content)
    }
    #[cfg(windows)]
    {
        std::fs::write(path, content)
    }
}
