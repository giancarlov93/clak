//! The agent side of the control channel, shared by the command line and the MCP shim:
//! one connection, one request, one response.
//!
//! The hub doesn't ask who we are: it reads it from the operating system, walking up from
//! the process that opens the pipe (or the socket) to the agent that launched it.

use crate::launcher;
use crate::util::Object;
use clak_wire::{ipc, location};
use serde_json::{Value, json};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Beyond this size a line is not a response: a large image in base64 fits comfortably
/// below it.
const MAX_LINE: usize = 64 * 1024 * 1024;

/// The hub is not there and cannot be started, or the user closed it on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubUnavailable(pub String);

impl fmt::Display for HubUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HubUnavailable {}

/// Asks the hub and waits for the response.
///
/// - `wait`: how long to wait for the response. It includes the time the user takes to
///   answer a question from the hub: if it runs out, the connection closes and the
///   question is withdrawn.
/// - `home`: the hub folder, normally [`location::home`].
/// - `progress`: the lines the hub sends before the response ("waiting for the
///   user…", "permission granted…"). If time runs out, they also tell what was being
///   waited for.
///
/// A timeout is not an error: it is an `ok: false` response with its code
/// (`no_answer` or `wait_timeout`), like the hub's.
pub fn ask(
    mut request: Object,
    wait: Duration,
    home: &Path,
    mut progress: impl FnMut(&Object),
) -> Result<Object, HubUnavailable> {
    request.insert("type".into(), "request".into());
    if request.get("cwd").is_none_or(Value::is_null) {
        let cwd = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        request.insert("cwd".into(), Value::String(cwd));
    }
    let closed = || HubUnavailable("Clak closed the connection without answering.".into());

    let mut stream = connect(home)?;
    let reader = stream.try_clone().map_err(|_| closed())?;
    let line = clak_wire::json::encode_line(&Value::Object(request));
    stream.write_all(&line).and_then(|()| stream.flush()).map_err(|_| closed())?;
    // Closed on exit no matter what: a read still in progress ends, and the hub sees the
    // connection closed (that is how an expired question is withdrawn).
    let _guard = Shutdown(&stream);

    // A thread reads the lines: the Windows pipe has no timeout for reads, and the caller
    // keeps the time.
    let (lines, incoming) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut reader = BufReader::with_capacity(65536, reader);
        loop {
            let mut line = Vec::new();
            match (&mut reader).take(MAX_LINE as u64).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    if lines.send(line).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + wait;
    let mut stage: Option<String> = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(timeout(wait, stage.as_deref()));
        }
        match incoming.recv_timeout(left) {
            Ok(line) => {
                let Some(message) = clak_wire::json::object(trim_line(&line)) else { continue };
                match message.get("type").and_then(Value::as_str) {
                    Some("response") => return Ok(message),
                    Some("progress") => {
                        stage = message.get("stage").and_then(Value::as_str).map(str::to_owned);
                        progress(&message);
                    }
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(timeout(wait, stage.as_deref())),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(closed()),
        }
    }
}

/// Closes the connection when [`ask`] returns, however it returns.
struct Shutdown<'a>(&'a ipc::Stream);

impl Drop for Shutdown<'_> {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Time is up: what was being waited for decides what the agent does next.
/// A user who doesn't answer must be sought out (`no_answer`, exit 7); a slow program
/// may have acted anyway, and must be checked before retrying (`wait_timeout`, exit 6).
pub(crate) fn timeout(wait: Duration, stage: Option<&str>) -> Object {
    let seconds = wait.as_secs();
    let response = if stage == Some("consent") {
        json!({
            "ok": false,
            "code": "no_answer",
            "error": format!(
                "The user did not answer Clak's request within {seconds} s, and the question was \
                 withdrawn. Ask them whether they are at the computer and to look at the Clak window, then try again."
            ),
        })
    } else {
        json!({
            "ok": false,
            "code": "wait_timeout",
            "error": format!(
                "No answer within {seconds} s. On giving up I asked the program to stop, but \
                 the command may have gone ahead anyway: before running it again, check whether it took effect. \
                 For a slow command (a render, a long script) run it again with a longer wait (--wait)."
            ),
        })
    };
    match response {
        Value::Object(map) => map,
        _ => Object::new(),
    }
}

/// Connects to the hub; if it is not there, starts it, once, and waits up to 20 seconds.
fn connect(home: &Path) -> Result<ipc::Stream, HubUnavailable> {
    let mut started = false;
    let clock = Instant::now();
    loop {
        // The channel is read again every time: the newly started hub writes its lockfile.
        if let Ok(stream) = ipc::connect(&location::control(home), Duration::from_millis(300)) {
            return Ok(stream);
        }
        if !started {
            if location::pause_flag(home).exists() {
                return Err(HubUnavailable(
                    "Clak was closed by the user: until they reopen it, no program can be reached.".into(),
                ));
            }
            if !launcher::start() {
                return Err(HubUnavailable("Clak is not running, and I cannot start it.".into()));
            }
            started = true;
        }
        if clock.elapsed() > Duration::from_secs(20) {
            return Err(HubUnavailable("Clak is not responding.".into()));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// A wait time in seconds as the agent writes it, within the limits of `Duration`.
pub fn seconds(value: f64) -> Duration {
    Duration::try_from_secs_f64(value.max(0.0)).unwrap_or(Duration::from_secs(u32::MAX.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_hub::FakeHub;

    #[test]
    fn le_righe_di_attesa_arrivano_prima_della_risposta() {
        let hub = FakeHub::new(vec![
            FakeHub::progress("consent", "waiting for the user to answer…"),
            FakeHub::progress("allowed", "permission granted."),
            FakeHub::response(json!({"ok": true, "resources": []})),
        ]);
        let mut seen = String::new();
        let request = json!({"op": "list"}).as_object().cloned().unwrap();
        let response = ask(request, Duration::from_secs(5), &hub.home, |m| {
            seen.push_str(m.get("stage").and_then(Value::as_str).unwrap_or(""));
            seen.push(';');
        })
        .unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(seen, "consent;allowed;");
        let requests = hub.requests();
        assert_eq!(requests[0]["type"], "request");
        assert!(requests[0]["cwd"].is_string());
    }

    #[test]
    fn se_il_tempo_scade_mentre_si_aspetta_l_utente_e_no_answer_e_la_domanda_si_ritira() {
        let hub = FakeHub::new(vec![FakeHub::progress("consent", "waiting for the user to answer…")]);
        let request = json!({"op": "list"}).as_object().cloned().unwrap();
        let response = ask(request, Duration::from_millis(300), &hub.home, |_| {}).unwrap();
        assert_eq!(response["code"], "no_answer");
        assert!(response["error"].as_str().unwrap().contains("whether they are at the computer"));
        // The hub sees the connection closed: that is how it withdraws the question to the user.
        assert!(hub.wait_closed(1, Duration::from_secs(3)), "the hub did not see the connection close");
    }

    #[test]
    fn se_il_tempo_scade_mentre_si_aspetta_il_programma_e_wait_timeout() {
        // After the user's yes, the wait is for the program: it may have acted anyway.
        for script in [vec![FakeHub::progress("consent", "…"), FakeHub::progress("allowed", "…")], vec![]] {
            let hub = FakeHub::new(script);
            let request = json!({"op": "call"}).as_object().cloned().unwrap();
            let response = ask(request, Duration::from_millis(300), &hub.home, |_| {}).unwrap();
            assert_eq!(response["code"], "wait_timeout");
            assert!(response["error"].as_str().unwrap().contains("check whether it took effect"));
        }
    }

    #[test]
    fn l_hub_chiuso_dall_utente_non_si_riapre() {
        let home = std::env::temp_dir().join(format!("clak-paused-{}", clak_wire::json::random_hex(4)));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(location::pause_flag(&home), b"").unwrap();
        let error = ask(Object::new(), Duration::from_secs(1), &home, |_| {}).unwrap_err();
        assert!(error.0.contains("closed by the user"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn i_secondi_restano_nei_limiti() {
        assert_eq!(seconds(0.5), Duration::from_millis(500));
        assert_eq!(seconds(1e300), Duration::from_secs(u32::MAX.into()));
    }
}
