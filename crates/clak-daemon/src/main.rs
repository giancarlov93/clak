//! The hub on its own, without the app: the same one the app keeps in-process.
//!
//! ```text
//!     clak-daemon                  no interface: every new permission is denied
//!     clak-daemon --ask            the questions appear here, answered y/n
//!     clak-daemon --web            the questions on a web page, at 127.0.0.1:7780
//!         [--port <n>]             another port
//!         [--url <base>]           the address in the agents' links, when the page is
//!                                  reached through a tunnel or a proxy
//!     clak-daemon --set-password   the web page's password
//! ```
//!
//! The log goes to stderr, in UTF-8.

mod console;
mod password;
mod web;

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

const USAGE: &str =
    "usage: clak-daemon [--ask | --web [--port <n>] [--url <base>] | --set-password]";

/// On the web page a question waits longer than in a window: the user has to get to it.
const WEB_TIMEOUT: Duration = Duration::from_secs(600);

fn log(text: &str) {
    // Launched without a stderr to write to: the hub works anyway.
    let _ = writeln!(
        std::io::stderr(),
        "[daemon {}] {text}",
        chrono::Local::now().format("%H:%M:%S")
    );
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    ask: bool,
    web: bool,
    port: Option<u16>,
    url: Option<String>,
    set_password: bool,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ask" => parsed.ask = true,
            "--web" => parsed.web = true,
            "--set-password" => parsed.set_password = true,
            "--port" => {
                let value = args.next().ok_or("--port needs a number")?;
                parsed.port = Some(
                    value
                        .parse()
                        .ok()
                        .filter(|&port: &u16| port > 0)
                        .ok_or(format!("not a port: {value}"))?,
                );
            }
            "--url" => {
                let value = args.next().ok_or("--url needs an address")?;
                if !(value.starts_with("http://") || value.starts_with("https://")) {
                    return Err(format!("not an http(s) address: {value}"));
                }
                parsed.url = Some(value.trim_end_matches('/').to_owned());
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    let modes = [parsed.ask, parsed.web, parsed.set_password];
    if modes.iter().filter(|&&on| on).count() > 1 {
        return Err("--ask, --web and --set-password go one at a time".into());
    }
    if !parsed.web && (parsed.port.is_some() || parsed.url.is_some()) {
        return Err("--port and --url go with --web".into());
    }
    Ok(parsed)
}

#[tokio::main]
async fn main() {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(problem) => {
            eprintln!("clak-daemon: {problem}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let home = clak_wire::location::home();
    if args.set_password {
        if let Err(problem) = password::choose(&home) {
            eprintln!("clak-daemon: {problem}");
            std::process::exit(1);
        }
        return;
    }

    let mut options = clak_host::options();
    if args.ask {
        options.consent = Some(Arc::new(console::ConsolePrompt::new(
            options.consent_timeout,
        )));
    }
    // The page's port is taken before the hub starts: a busy one stops everything at once.
    let mut page = None;
    if args.web {
        let hash = match password::load(&home) {
            Some(hash) => hash,
            None if std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
                if let Err(problem) = password::choose(&home) {
                    eprintln!("clak-daemon: {problem}");
                    std::process::exit(1);
                }
                password::load(&home).expect("just saved")
            }
            None => {
                eprintln!(
                    "clak-daemon: the web page needs a password: run clak-daemon --set-password first."
                );
                std::process::exit(1);
            }
        };
        let port = args.port.unwrap_or(web::PORT);
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => listener,
            Err(e) => {
                log(&format!(
                    "the web page can't listen on 127.0.0.1:{port}: {e}"
                ));
                std::process::exit(1);
            }
        };
        let base = args
            .url
            .clone()
            .unwrap_or(format!("http://127.0.0.1:{port}"));
        let prompt = web::WebPrompt::new(WEB_TIMEOUT);
        options.consent_timeout = WEB_TIMEOUT;
        options.consent = Some(Arc::new(prompt.clone()));
        let links = base.clone();
        options.question_link = Some(Arc::new(move |id: &str| format!("{links}/q/{id}")));
        page = Some((listener, prompt, hash, base));
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
    let stop = tokio_util::sync::CancellationToken::new();
    if let Some((listener, prompt, hash, base)) = page {
        log(&format!("the questions are answered at {base}"));
        let page = web::Page::new(prompt, host.broker().clone(), hash);
        tokio::spawn(web::serve(listener, page, stop.clone()));
    }

    // Ctrl+C, closing the console, shutdown: in every case the lockfile must be removed
    // before exiting. If the process is killed it stays there, and whoever comes next
    // overwrites it.
    quit().await;
    stop.cancel();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Result<Args, String> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn the_options() {
        assert_eq!(args(""), Ok(Args::default()));
        assert_eq!(args("--ask").map(|a| a.ask), Ok(true));
        let web = args("--web --port 9000 --url https://box.example.ts.net/").unwrap();
        assert!(web.web);
        assert_eq!(web.port, Some(9000));
        assert_eq!(web.url.as_deref(), Some("https://box.example.ts.net"));
        assert!(args("--ask --web").is_err());
        assert!(args("--web --set-password").is_err());
        assert!(args("--port 9000").is_err());
        assert!(args("--web --port").is_err());
        assert!(args("--web --port 0").is_err());
        assert!(args("--web --port seventy").is_err());
        assert!(args("--web --url box.example").is_err());
        assert!(args("--verbose").is_err());
    }
}
