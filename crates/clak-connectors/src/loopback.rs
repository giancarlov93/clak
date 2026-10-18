//! The sign-in's return page: a small HTTP server on 127.0.0.1, only for the duration
//! of the sign-in. It accepts the response with the expected `state` and ignores
//! everything else (a page with another state may be an attempt to slip in someone
//! else's code). The pages it sends are fixed: they repeat nothing of what it receives.

use crate::oauth::CALLBACK_PATH;
use crate::text::unescape_lossy;
use std::collections::HashMap;
use std::io;
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

const MAX_REQUEST: usize = 16 * 1024;

pub(crate) struct LoopbackCallback {
    listener: TcpListener,
    port: u16,
    /// The open connections: they end with the server (dropping it interrupts them).
    serving: Mutex<JoinSet<()>>,
}

impl LoopbackCallback {
    /// On the previous port, if it's free (the registered client knows it), otherwise on any port.
    pub async fn start(preferred: u16) -> io::Result<LoopbackCallback> {
        if preferred > 0
            && let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, preferred)).await
        {
            return Self::with(listener);
        }
        Self::with(TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?)
    }

    fn with(listener: TcpListener) -> io::Result<LoopbackCallback> {
        let port = listener.local_addr()?.port();
        Ok(LoopbackCallback {
            listener,
            port,
            serving: Mutex::new(JoinSet::new()),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The parameters of the first return page with the expected `state`. Stop waiting by
    /// dropping the future (the time limit, or the user canceling).
    pub async fn wait(&self, state: &str) -> HashMap<String, String> {
        let (done, mut answer) = mpsc::channel::<HashMap<String, String>>(4);
        loop {
            tokio::select! {
                Some(values) = answer.recv() => return values,
                accepted = self.listener.accept() => match accepted {
                    // A browser sometimes opens connections it doesn't use: each one on its own, with a time limit.
                    Ok((stream, _)) => {
                        let mut serving = self.serving.lock().unwrap_or_else(|p| p.into_inner());
                        while serving.try_join_next().is_some() {}
                        serving.spawn(serve(stream, state.to_owned(), done.clone()));
                    }
                    // A connection dropped before being accepted (happens on Windows): move on.
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                },
            }
        }
    }
}

async fn serve(mut stream: TcpStream, state: String, done: mpsc::Sender<HashMap<String, String>>) {
    // A connection that brings nothing: just close it.
    let _ = tokio::time::timeout(Duration::from_secs(15), async {
        let Some(target) = read_target(&mut stream).await? else { return Ok::<(), io::Error>(()) };
        let (path, query) = match target.find('?') {
            Some(question) => (&target[..question], &target[question + 1..]),
            None => (target.as_str(), ""),
        };
        if path != CALLBACK_PATH {
            return respond(&mut stream, 404, "Not found", "Nothing here.").await;
        }
        let values = parse_query(query);
        if values.get("state").map(String::as_str) != Some(state.as_str()) {
            return respond(
                &mut stream,
                400,
                "Not the expected sign-in",
                "This page isn't the sign-in Clak is waiting for. If you started a sign-in, go back to Clak and try again.",
            )
            .await;
        }
        let ok = values.contains_key("code") && !values.contains_key("error");
        if ok {
            respond(&mut stream, 200, "Signed in", "You're signed in. You can close this tab and go back to Clak.").await?;
        } else {
            respond(
                &mut stream,
                200,
                "Sign-in not completed",
                "The sign-in didn't complete. You can close this tab: Clak tells you what happened.",
            )
            .await?;
        }
        let _ = done.send(values).await;
        Ok(())
    })
    .await;
}

/// The request's path (`GET /callback?… HTTP/1.1`), or `None`.
async fn read_target(stream: &mut TcpStream) -> io::Result<Option<String>> {
    let mut buffer = vec![0u8; MAX_REQUEST];
    let mut length = 0;
    while length < buffer.len() {
        let read = stream.read(&mut buffer[length..]).await?;
        if read == 0 {
            break;
        }
        length += read;
        if buffer[..length].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    // ASCII: a byte that isn't becomes "?", as in Encoding.ASCII.
    let text: String = buffer[..length]
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { '?' })
        .collect();
    let first = text.split("\r\n").next().unwrap_or("");
    let parts: Vec<&str> = first.split(' ').collect();
    Ok(
        (parts.len() == 3 && parts[0] == "GET" && parts[1].starts_with('/'))
            .then(|| parts[1].to_owned()),
    )
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = match pair.find('=') {
            Some(equals) => (
                unescape_lossy(&pair[..equals]),
                unescape_lossy(&pair[equals + 1..]),
            ),
            None => (unescape_lossy(pair), String::new()),
        };
        values.entry(key).or_insert(value);
    }
    values
}

async fn respond(stream: &mut TcpStream, status: u16, title: &str, text: &str) -> io::Result<()> {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Clak — {title}</title>\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <style>body{{font-family:\"Segoe UI\",system-ui,sans-serif;margin:0;min-height:100vh;display:grid;place-items:center;background:#f6f6f8;color:#1d1d24}}\n\
         main{{max-width:420px;padding:32px;text-align:center}}h1{{font-size:20px;font-weight:600;margin:0 0 8px}}p{{margin:0;color:#555;line-height:1.5}}\n\
         @media (prefers-color-scheme:dark){{body{{background:#1d1a24;color:#eee}}p{{color:#aaa}} }}</style></head>\n\
         <body><main><h1>{title}</h1><p>{text}</p></main></body></html>"
    );
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        html.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(html.as_bytes()).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_is_parsed_and_the_first_wins() {
        let values = parse_query("code=a%2Bb&state=x+y&state=z&flag");
        assert_eq!(values["code"], "a+b");
        assert_eq!(values["state"], "x y");
        assert_eq!(values["flag"], "");
    }
}
