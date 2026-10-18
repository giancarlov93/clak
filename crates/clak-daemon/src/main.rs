//! The hub on its own, without the app: the same one the app keeps in-process.
//!
//! ```text
//!     clak-daemon          no interface: every new permission is denied
//!     clak-daemon --ask    the questions appear here, answered y/n
//! ```
//!
//! The log goes to stderr, in UTF-8.

use clak_hub::consent::BoxFuture;
use clak_hub::{ConsentPrompt, ConsentRequest};
use std::io::Write;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

fn log(text: &str) {
    // Launched without a stderr to write to: the hub works anyway.
    let _ = writeln!(
        std::io::stderr(),
        "[daemon {}] {text}",
        chrono::Local::now().format("%H:%M:%S")
    );
}

#[tokio::main]
async fn main() {
    let home = clak_wire::location::home();
    let mut options = clak_host::options();
    if std::env::args().any(|a| a == "--ask") {
        options.consent = Some(Arc::new(ConsolePrompt::new()));
    }
    let host = match clak_host::ClakHost::try_start(
        &home,
        options,
        Some(Arc::new(|line: String| log(&line))),
    )
    .await
    {
        Ok(Some(host)) => host,
        Ok(None) => {
            log(&format!(
                "there's already a hub on {}: exiting",
                clak_wire::location::computed_control(&home)
            ));
            return;
        }
        Err(e) => {
            log(&format!("the hub couldn't start: {e}"));
            std::process::exit(1);
        }
    };

    // Ctrl+C, closing the console, shutdown: in every case the lockfile must be removed
    // before exiting. If the process is killed it stays there, and whoever comes next
    // overwrites it.
    quit().await;
    host.stop().await;
}

#[cfg(unix)]
async fn quit() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("signal");
    let mut hangup = signal(SignalKind::hangup()).expect("signal");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = hangup.recv() => {}
    }
}

#[cfg(windows)]
async fn quit() {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_shutdown};
    let mut brk = ctrl_break().expect("signal");
    let mut close = ctrl_close().expect("signal");
    let mut shutdown = ctrl_shutdown().expect("signal");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = brk.recv() => {}
        _ = close.recv() => {}
        _ = shutdown.recv() => {}
    }
}

/// The hub's questions on the terminal: one at a time, answered "y" or "n" on a line. The
/// lines are read on a separate thread, so a withdrawn question doesn't stay stuck on a
/// read.
struct ConsolePrompt {
    lines: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
}

impl ConsolePrompt {
    fn new() -> ConsolePrompt {
        let (sender, receiver) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("questions".into())
            .spawn(move || {
                let mut line = String::new();
                while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
                    let _ = sender.send(std::mem::take(&mut line));
                }
            })
            .expect("thread");
        ConsolePrompt {
            lines: Arc::new(Mutex::new(receiver)),
        }
    }
}

impl ConsentPrompt for ConsolePrompt {
    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<bool, String>> {
        let lines = self.lines.clone();
        Box::pin(async move {
            // One question at a time: whoever holds the receiver is the one on screen.
            let mut lines = tokio::select! {
                lines = lines.lock() => lines,
                _ = cancel.cancelled() => return Ok(false),
            };
            let mut text = format!("\n=== {}\n", request.title);
            for line in request.who.iter().chain(&request.what) {
                text.push_str(&format!("    {}: {}\n", line.label, line.value));
            }
            for tool in &request.tools {
                let note = tool
                    .note
                    .as_deref()
                    .map(|n| format!(" ({n})"))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "    • {}{note} — {}\n",
                    tool.name, tool.description
                ));
            }
            if let Some(warning) = &request.warning {
                text.push_str(&format!("    WARNING: {warning}\n"));
            }
            text.push_str(&format!(
                "    {}\n{} [y/n] ",
                request.note, request.question
            ));
            let _ = write!(std::io::stderr(), "{text}");
            loop {
                let line = tokio::select! {
                    line = lines.recv() => line,
                    _ = cancel.cancelled() => return Ok(false),
                };
                // No more input: nobody can say yes.
                let Some(line) = line else { return Ok(false) };
                match line.trim().to_lowercase().as_str() {
                    "y" | "yes" => return Ok(true),
                    "n" | "no" => return Ok(false),
                    _ => {
                        let _ = write!(std::io::stderr(), "[y/n] ");
                    }
                }
            }
        })
    }
}
