//! The program side: a loopback WebSocket, one connection per live resource.
//!
//! Listening is a `TcpListener` and the handshake is the minimal one from RFC 6455: the two
//! TCP ports are needed anyway to ask the operating system who is on the other side, and a
//! whole HTTP server for a single `101` would be extra surface on a port that any web page
//! can probe.

use crate::core::{Broker, Outbox};
use crate::permissions::AppPermissions;
use crate::texts;
use crate::wire::{self, Malformed, ResourceDef, ToolDef};
use crate::consent::Verdict;
use clak_wire::json::{self, MAX_MESSAGE_BYTES};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const KEEP_ALIVE: Duration = Duration::from_secs(20);
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(10);

/// Instructions longer than this are cut: a program does not fill the agent's context.
pub const MAX_INSTRUCTIONS: usize = 4000;

/// Opens the port, chosen by the operating system.
pub async fn bind() -> std::io::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

pub async fn accept(listener: TcpListener, broker: Broker, tracker: TaskTracker, stop: CancellationToken) {
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = stop.cancelled() => return,
        };
        // A connection aborted before being accepted does not stop listening.
        let Ok((stream, _)) = accepted else { continue };
        tracker.spawn(serve(stream, broker.clone(), stop.clone()));
    }
}

async fn serve(stream: TcpStream, broker: Broker, stop: CancellationToken) {
    let _ = stream.set_nodelay(true);
    let (Ok(peer), Ok(local)) = (stream.peer_addr(), stream.local_addr()) else { return };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let handshake = tokio_tungstenite::accept_async_with_config(stream, Some(config));
    let Ok(Ok(socket)) = tokio::time::timeout(HANDSHAKE_LIMIT, handshake).await else { return };
    let (mut sink, mut source) = socket.split();

    let (outbox, mut queue) = Outbox::channel();
    // There is a single writer: messages go out in the order the hub decided them. Between
    // messages, a ping now and then: a suspended program is found out.
    let (closing_tx, mut closing_rx) = tokio::sync::oneshot::channel::<CloseCode>();
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(KEEP_ALIVE);
        ping.tick().await;
        loop {
            tokio::select! {
                // Closing first: the outbox is dropped right after, and an empty queue must not
                // end the writer before the Close frame goes out.
                biased;
                code = &mut closing_rx => {
                    // What is already queued goes out, then it closes.
                    while let Ok(message) = queue.try_recv() {
                        if sink.send(Message::text(json::text(&message))).await.is_err() {
                            return;
                        }
                    }
                    let code = code.unwrap_or(CloseCode::Normal);
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        sink.send(Message::Close(Some(CloseFrame { code, reason: "".into() }))),
                    )
                    .await;
                    return;
                }
                message = queue.recv() => match message {
                    Some(message) => {
                        if sink.send(Message::text(json::text(&message))).await.is_err() {
                            return;
                        }
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        return;
                    }
                }
            }
        }
    });

    let mut app: Option<String> = None;
    let mut close_reason = "app_gone".to_owned();
    let mut code = CloseCode::Normal;
    loop {
        let frame = tokio::select! {
            frame = source.next() => frame,
            _ = stop.cancelled() => break,
        };
        let raw = match frame {
            Some(Ok(Message::Text(text))) => text.as_bytes().to_vec(),
            Some(Ok(Message::Binary(bytes))) => bytes.to_vec(),
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
            Some(Err(tokio_tungstenite::tungstenite::Error::Capacity(_))) => {
                code = CloseCode::Size;
                break;
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
        };
        let Ok(Value::Object(message)) = json::parse(&raw) else { break };
        match handle(&broker, &outbox, &mut app, &message, peer.port(), local.port()).await {
            Ok(Flow::Continue) => {}
            Ok(Flow::Close(reason)) => {
                close_reason = reason;
                break;
            }
            Ok(Flow::Drop) => break,
            Err(e) => {
                broker.run(|c| {
                    c.log(format!("malformed message from the app: {}", e.repr()));
                    outbox.send(error(&format!("malformed message: {}", e.text())));
                });
                break;
            }
        }
    }

    if let Some(conn_id) = &app {
        broker.run(|c| c.app_closed(conn_id, &close_reason));
    }
    let _ = closing_tx.send(code);
    drop(outbox);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
}

enum Flow {
    Continue,
    /// The program said `close`, with its reason.
    Close(String),
    /// The connection must be dropped (wrong token, consent denied).
    Drop,
}

async fn handle(
    broker: &Broker,
    outbox: &Outbox,
    app: &mut Option<String>,
    message: &Map<String, Value>,
    peer_port: u16,
    local_port: u16,
) -> Result<Flow, Malformed> {
    let kind = message.get("type").and_then(Value::as_str);

    // Registration happens once per connection.
    if kind == Some("register") && app.is_none() {
        if !broker.token_matches(message.get("token")) {
            broker.run(|_| outbox.send(error("invalid token")));
            return Ok(Flow::Drop);
        }
        // No version means the first one; one the hub does not know is refused, and is told so.
        if message.get("protocol").map_or(Some(1), Value::as_u64) != Some(clak_wire::PROTOCOL) {
            let text = format!("unsupported protocol version: this Clak speaks version {}", clak_wire::PROTOCOL);
            broker.run(|_| outbox.send(json!({ "type": "error", "error": text, "protocol": clak_wire::PROTOCOL })));
            return Ok(Flow::Drop);
        }
        let tools = wire::active(wire::parse_tools(wire::required(message, "tools")?)?, message.get("active"))?;
        let resource = ResourceDef::from_json(wire::required(message, "resource")?)?;
        // Slow, and outside the lock: meanwhile the hub serves the others.
        let platform = broker.options().platform.clone();
        let origin = tokio::task::spawn_blocking(move || platform.attest_loopback(peer_port, local_port))
            .await
            .unwrap_or_else(|_| json!({}));
        if !admit(broker, outbox, &resource, &origin, &tools).await {
            return Ok(Flow::Drop);
        }
        let instructions = instructions(message);
        let conn_id = broker.run(|c| {
            let id = c.register(outbox.clone(), resource, tools, origin, None);
            c.set_instructions(&id, instructions);
            id
        });
        *app = Some(conn_id);
        return Ok(Flow::Continue);
    }

    let Some(conn_id) = app.as_deref() else { return Ok(Flow::Continue) };
    match kind {
        Some("tools_changed" | "register") => {
            let tools = wire::active(wire::parse_tools(wire::required(message, "tools")?)?, message.get("active"))?;
            let update = message.contains_key("instructions").then(|| instructions(message));
            broker.run(|c| {
                c.tools_changed(conn_id, tools);
                if let Some(text) = update {
                    c.set_instructions(conn_id, text);
                }
                c.touch();
            });
        }
        Some("result") => broker.run(|c| {
            let done = c.result(conn_id, message);
            c.touch();
            done
        })?,
        Some("close") => {
            return Ok(Flow::Close(message.get("reason").and_then(Value::as_str).unwrap_or("resource_closed").to_owned()));
        }
        _ => broker.run(|c| c.touch()),
    }
    Ok(Flow::Continue)
}

fn error(text: &str) -> Value {
    json!({ "type": "error", "error": text })
}

/// How the program wants to be used, optional in `register`: `clak <program> -h` shows it
/// before the commands. It is the program's text, like the descriptions.
pub fn instructions(message: &Map<String, Value>) -> Option<String> {
    let text = message.get("instructions").and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }
    // The limit is counted in UTF-16 units.
    if text.encode_utf16().count() <= MAX_INSTRUCTIONS {
        return Some(text.to_owned());
    }
    let mut cut = String::new();
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > MAX_INSTRUCTIONS {
            break;
        }
        units += c.len_utf16();
        cut.push(c);
    }
    cut.push('…');
    Some(cut)
}

/// A program gets in only if the user allows it: once per executable, and the hub remembers
/// it. While the question is open the program knows it (`waiting`); if the user says no it
/// receives `denied` and the connection closes.
async fn admit(broker: &Broker, outbox: &Outbox, resource: &ResourceDef, origin: &Value, tools: &[ToolDef]) -> bool {
    let platform = broker.options().platform.clone();
    let (o, name) = (origin.clone(), resource.app.clone());
    let key = tokio::task::spawn_blocking(move || AppPermissions::key_for(platform.as_ref(), &o, &name)).await.ok().flatten();
    if broker.run(|c| c.permissions().allows(key.as_deref())) {
        return true;
    }

    outbox.send(json!({ "type": "waiting", "reason": "consent" }));
    let remembered = key.is_some();
    let consent_key = format!("app:{}", key.clone().unwrap_or_else(|| clak_wire::json::new_id("unknown")));
    let (allow_key, allow_origin, allow_name) = (key, origin.clone(), resource.app.clone());
    let stop = broker.stopping();
    let verdict = broker
        .consent(
            &consent_key,
            |id, _| texts::app(id, resource, origin, tools, remembered),
            move |c| {
                if let Some(key) = allow_key {
                    c.permissions_mut().allow(&key, &allow_origin, &allow_name);
                }
            },
            &stop,
        )
        .await
        .unwrap_or(Verdict::NoAnswer);
    if verdict == Verdict::Allowed {
        return true;
    }
    outbox.send(json!({
        "type": "denied",
        "reason": match verdict {
            Verdict::Denied => "user",
            Verdict::NoPrompt => "no_prompt",
            _ => "no_answer",
        },
    }));
    false
}
