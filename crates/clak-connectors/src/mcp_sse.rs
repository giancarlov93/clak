//! An MCP client over the HTTP+SSE transport of 2024-11-05, which some servers still speak
//! (programs with an MCP server inside, like Affinity, and the older SDKs): a GET opens a
//! stream that stays open, its first event (`endpoint`) says where to POST the messages, and
//! every answer arrives on that stream, not in the POST's response.
//!
//! [`McpHttp`](crate::McpHttp) falls back to it by itself, as the specification suggests:
//! when the POST of `initialize` is refused with 400, 404 or 405, a GET at the same address
//! that opens an event stream with an `endpoint` is this transport.
//!
//! The endpoint must be on the same server as the stream (scheme, host and port): the token
//! doesn't travel elsewhere, as with redirects. The stream lives as long as the client; when
//! it closes, whoever was waiting for an answer knows, and [`McpSse::ended`] says why.
//!
//! Canceling a request means dropping its future: the server learns about it with
//! `notifications/cancelled`.

use crate::mcp_http::{
    AccessToken, Elicit, McpError, PROTOCOL_VERSION, capabilities, network_message, rpc_error,
    server_request,
};
use crate::oauth::OAuthChallenge;
use crate::uri;
use clak_wire::json::{self as wire, MAX_MESSAGE_BYTES};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{Notify, broadcast, oneshot};
use tokio_util::sync::CancellationToken;
use url::Url;

/// How long to wait for the `endpoint` event after the stream opens.
const ENDPOINT_LIMIT: Duration = Duration::from_secs(10);

type Answer = oneshot::Sender<Result<Map<String, Value>, McpError>>;

#[derive(Default)]
struct State {
    instructions: Option<String>,
    server_name: Option<String>,
    icon: Option<String>,
    /// Why the stream is gone, when it's gone.
    ended: Option<String>,
    pending: HashMap<i64, Answer>,
    /// The running commands that can ask the user. A request from the server says nothing of
    /// the command it comes from: it goes to the most recent, and names the others.
    elicits: Vec<(i64, Elicit)>,
}

struct Shared {
    state: Mutex<State>,
    list_changed: Notify,
    ended: CancellationToken,
    notifications: broadcast::Sender<Map<String, Value>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The stream is gone: whoever was waiting for an answer knows, with the reason.
    fn end(&self, reason: String) {
        let (pending, reason) = {
            let mut state = self.lock();
            let reason = state.ended.get_or_insert(reason).clone();
            let pending: Vec<Answer> = state.pending.drain().map(|(_, a)| a).collect();
            (pending, reason)
        };
        for answer in pending {
            let _ = answer.send(Err(McpError::Transport(reason.clone())));
        }
        self.ended.cancel();
    }

    fn gone(&self) -> String {
        self.lock()
            .ended
            .clone()
            .unwrap_or_else(|| "the server closed the stream".into())
    }
}

/// Where the messages go: the endpoint the server named, with the token.
#[derive(Clone)]
struct Poster {
    http: Client,
    endpoint: Url,
    token: Option<Arc<dyn AccessToken>>,
}

impl Poster {
    /// One message; the answer, if any, arrives on the stream. After a 401, a second attempt
    /// with a refreshed token, if there is one.
    async fn post(&self, message: &Value) -> Result<(), McpError> {
        let used = current(&self.token).await?;
        let mut response = self.once(message, used.as_deref()).await?;
        if response.status() == StatusCode::UNAUTHORIZED
            && let Some(token) = &self.token
            && token.renew(used).await?
        {
            drop(response);
            let fresh = token.current().await?;
            response = self.once(message, fresh.as_deref()).await?;
        }
        let status = response.status();
        if let Some(error) = auth_error(&response, &self.token) {
            return Err(error);
        }
        // The endpoint names the session: the server no longer knows it.
        if status == StatusCode::NOT_FOUND {
            return Err(McpError::SessionLost);
        }
        if !status.is_success() {
            return Err(McpError::Transport(status_text(status)));
        }
        Ok(())
    }

    async fn once(&self, message: &Value, bearer: Option<&str>) -> Result<Response, McpError> {
        let mut request = self
            .http
            .post(self.endpoint.clone())
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(wire::encode(message));
        if let Some(bearer) = bearer.filter(|b| !b.is_empty()) {
            request = request.header(AUTHORIZATION, format!("Bearer {bearer}"));
        }
        request
            .send()
            .await
            .map_err(|e| McpError::Transport(network_message(&e)))
    }
}

/// The MCP client of a server that speaks HTTP+SSE. Used by several requests at once.
pub struct McpSse {
    poster: Poster,
    shared: Arc<Shared>,
    next_id: AtomicI64,
    can_elicit: bool,
    stop: CancellationToken,
}

impl Drop for McpSse {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl McpSse {
    /// Opens the stream at `url` and waits for its `endpoint`. `Ok(None)` if the address
    /// doesn't open an event stream: the server doesn't speak this transport either. The
    /// notifications go to `notifications`, the sender of whoever fell back to this client.
    pub(crate) async fn open(
        http: Client,
        url: &Url,
        token: Option<Arc<dyn AccessToken>>,
        can_elicit: bool,
        notifications: broadcast::Sender<Map<String, Value>>,
    ) -> Result<Option<McpSse>, McpError> {
        let used = current(&token).await?;
        let mut response = get(&http, url, used.as_deref()).await?;
        if response.status() == StatusCode::UNAUTHORIZED
            && let Some(renewing) = &token
            && renewing.renew(used).await?
        {
            drop(response);
            let fresh = renewing.current().await?;
            response = get(&http, url, fresh.as_deref()).await?;
        }
        if let Some(error) = auth_error(&response, &token) {
            return Err(error);
        }
        let events = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("text/event-stream")
            });
        if !response.status().is_success() || !events {
            return Ok(None);
        }

        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            list_changed: Notify::new(),
            ended: CancellationToken::new(),
            notifications,
        });
        let stop = CancellationToken::new();
        let (found, endpoint) = oneshot::channel();
        let reader = Reader {
            http: http.clone(),
            url: url.clone(),
            token: token.clone(),
            shared: shared.clone(),
            can_elicit,
            found: Some(found),
            poster: None,
        };
        let (closing, ending) = (stop.clone(), shared.clone());
        tokio::spawn(async move {
            tokio::select! {
                () = reader.run(response) => {}
                _ = closing.cancelled() => ending.end("the client closed the stream".into()),
            }
        });
        let endpoint = match tokio::time::timeout(ENDPOINT_LIMIT, endpoint).await {
            Ok(Ok(Ok(endpoint))) => endpoint,
            Ok(Ok(Err(why))) => {
                stop.cancel();
                return Err(McpError::Transport(why));
            }
            Ok(Err(_)) => {
                stop.cancel();
                return Err(McpError::Transport(shared.gone()));
            }
            Err(_) => {
                stop.cancel();
                return Err(McpError::Transport(format!(
                    "the server opened its stream but didn't say where to send messages within {} seconds",
                    ENDPOINT_LIMIT.as_secs()
                )));
            }
        };
        Ok(Some(McpSse {
            poster: Poster {
                http,
                endpoint,
                token,
            },
            shared,
            next_id: AtomicI64::new(0),
            can_elicit,
            stop,
        }))
    }

    /// The instructions for how the server wants to be used, if it has any.
    pub fn instructions(&self) -> Option<String> {
        self.shared.lock().instructions.clone()
    }

    pub fn server_name(&self) -> Option<String> {
        self.shared.lock().server_name.clone()
    }

    /// The icon the server declares (`serverInfo.icons`), if the app can show it.
    pub fn icon(&self) -> Option<String> {
        self.shared.lock().icon.clone()
    }

    /// Arrives when the server says its commands have changed.
    pub async fn changed(&self) {
        self.shared.list_changed.notified().await;
    }

    /// Arrives when the stream is gone; says why.
    pub async fn ended(&self) -> String {
        self.shared.ended.cancelled().await;
        self.shared.gone()
    }

    /// `initialize`, with the latest version, as over Streamable HTTP: a server negotiates
    /// it down. Some refuse it instead, and say which one they support (in `data.supported`,
    /// or only in the message, like Affinity): then once more with that one.
    pub async fn initialize(&self) -> Result<(), McpError> {
        let result = match self.initialize_with(PROTOCOL_VERSION).await {
            Err(error) => match supported(&error).filter(|v| v != PROTOCOL_VERSION) {
                Some(version) => self.initialize_with(&version).await?,
                None => return Err(error),
            },
            Ok(result) => result,
        };
        {
            let mut state = self.shared.lock();
            state.instructions = wire::str(result.get("instructions"))
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            state.server_name =
                wire::str(result.get("serverInfo").and_then(|s| s.get("name"))).map(str::to_owned);
            state.icon = crate::mcp::declared_icon(result.get("serverInfo"));
        }
        self.poster
            .post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await
    }

    async fn initialize_with(&self, version: &str) -> Result<Map<String, Value>, McpError> {
        let capabilities = capabilities(self.can_elicit);
        self.request(
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": capabilities,
                "clientInfo": { "name": "clak", "version": env!("CARGO_PKG_VERSION") },
            }),
            None,
        )
        .await
    }

    /// All the commands, page after page.
    pub async fn list_tools(&self) -> Result<Vec<Map<String, Value>>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let mut parameters = Map::new();
            if let Some(cursor) = &cursor {
                parameters.insert("cursor".into(), Value::String(cursor.clone()));
            }
            let result = self
                .request("tools/list", Value::Object(parameters), None)
                .await?;
            if let Some(Value::Array(list)) = result.get("tools") {
                tools.extend(list.iter().filter_map(|t| t.as_object().cloned()));
            }
            cursor = wire::str(result.get("nextCursor")).map(str::to_owned);
            if cursor.as_deref().is_none_or(str::is_empty) {
                break;
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
        elicit: Option<&Elicit>,
    ) -> Result<Map<String, Value>, McpError> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
            elicit,
        )
        .await
    }

    async fn request(
        &self,
        method: &str,
        parameters: Value,
        elicit: Option<&Elicit>,
    ) -> Result<Map<String, Value>, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (answer, receiver) = oneshot::channel();
        {
            let mut state = self.shared.lock();
            if let Some(reason) = &state.ended {
                return Err(McpError::Transport(reason.clone()));
            }
            state.pending.insert(id, answer);
            if let Some(elicit) = elicit {
                state.elicits.push((id, elicit.clone()));
            }
        }
        let mut waiting = Waiting {
            client: self,
            id,
            method_cancels: method != "initialize",
            done: false,
        };
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": parameters });
        if let Err(e) = self.poster.post(&message).await {
            // Never arrived: there is nothing to cancel.
            waiting.done = true;
            return Err(e);
        }
        let result = receiver
            .await
            .unwrap_or_else(|_| Err(McpError::Transport(self.shared.gone())));
        waiting.done = true;
        result
    }
}

/// A request waiting for an answer: if whoever was waiting leaves first, the server knows.
struct Waiting<'a> {
    client: &'a McpSse,
    id: i64,
    method_cancels: bool,
    done: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let abandoned = {
            let mut state = self.client.shared.lock();
            state.elicits.retain(|(id, _)| *id != self.id);
            state.pending.remove(&self.id).is_some()
        };
        if abandoned
            && !self.done
            && self.method_cancels
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let poster = self.client.poster.clone();
            let id = self.id;
            runtime.spawn(async move {
                let _ = poster
                    .post(&json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/cancelled",
                        "params": { "requestId": id, "reason": "the caller stopped waiting" },
                    }))
                    .await;
            });
        }
    }
}

/// Reads the stream to its end: first the endpoint, then the server's messages.
struct Reader {
    http: Client,
    url: Url,
    token: Option<Arc<dyn AccessToken>>,
    shared: Arc<Shared>,
    can_elicit: bool,
    /// Who waits for the endpoint, until it arrives.
    found: Option<oneshot::Sender<Result<Url, String>>>,
    poster: Option<Poster>,
}

impl Reader {
    /// To the end of the stream; then whoever was waiting knows why.
    async fn run(mut self, mut response: Response) {
        let mut decoder = Decoder::default();
        let reason = loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break "the server closed the stream".to_owned(),
                Err(e) => break format!("the stream broke ({})", network_message(&e)),
            };
            let mut events = Vec::new();
            if let Err(why) = decoder.feed(&chunk, &mut events) {
                break why;
            }
            for (name, data) in events {
                if let Err(why) = self.event(&name, data) {
                    if let Some(found) = self.found.take() {
                        let _ = found.send(Err(why.clone()));
                    }
                    self.shared.end(why);
                    return;
                }
            }
        };
        self.shared.end(reason);
    }

    fn event(&mut self, name: &str, data: String) -> Result<(), String> {
        match name {
            "endpoint" => {
                // Only the first one counts: a later one could only move the messages elsewhere.
                if self.poster.is_some() {
                    return Ok(());
                }
                let endpoint = self
                    .url
                    .join(data.trim())
                    .map_err(|_| "the server named an endpoint that isn't an address".to_owned())?;
                if !uri::same_server(&endpoint, &self.url) {
                    return Err(
                        "the server sends its messages to another address: not followed".into(),
                    );
                }
                self.poster = Some(Poster {
                    http: self.http.clone(),
                    endpoint: endpoint.clone(),
                    token: self.token.clone(),
                });
                if let Some(found) = self.found.take() {
                    let _ = found.send(Ok(endpoint));
                }
                Ok(())
            }
            "message" => {
                // A message that isn't JSON is skipped, as on stdio.
                if let Ok(Value::Object(message)) = wire::parse(data.as_bytes()) {
                    self.message(message);
                }
                Ok(())
            }
            // Anything else (a heartbeat, a server's own event) doesn't concern MCP.
            _ => Ok(()),
        }
    }

    fn message(&self, message: Map<String, Value>) {
        let method = wire::str(message.get("method")).map(str::to_owned);
        let id = message.get("id").filter(|v| !v.is_null()).cloned();
        match (method, id) {
            (None, Some(id)) => answer(&self.shared, &message, &id),
            (Some(method), Some(id)) => {
                let Some(poster) = self.poster.clone() else {
                    return;
                };
                let parameters = message
                    .get("params")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let (elicit, others) = {
                    let state = self.shared.lock();
                    let elicit = state.elicits.last().map(|(_, e)| e.clone());
                    let others: Vec<String> = state
                        .elicits
                        .iter()
                        .map(|(_, e)| e.call.clone())
                        .filter(|call| elicit.as_ref().is_some_and(|e| e.call != *call))
                        .collect();
                    (elicit.filter(|_| self.can_elicit), others)
                };
                // In a task: a question waits for the user, and meanwhile the answers arrive.
                tokio::spawn(async move {
                    let reply =
                        server_request(id, &method, parameters, elicit.as_ref(), others).await;
                    let _ = poster.post(&reply).await;
                });
            }
            (Some(method), None) => {
                if method == "notifications/tools/list_changed" {
                    self.shared.list_changed.notify_one();
                }
                let _ = self.shared.notifications.send(message);
            }
            _ => {}
        }
    }
}

fn answer(shared: &Shared, message: &Map<String, Value>, id: &Value) {
    let Some(id) = id.as_i64() else { return };
    let Some(answer) = shared.lock().pending.remove(&id) else {
        return;
    };
    let result = match message.get("error") {
        Some(Value::Object(error)) => Err(rpc_error(error)),
        _ => Ok(message
            .get("result")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()),
    };
    let _ = answer.send(result);
}

/// The events of an SSE stream, as they arrive in pieces: their name (`message` if the
/// stream doesn't say) and data. Lines end with `\n`, `\r\n` or `\r`.
#[derive(Default)]
struct Decoder {
    line: Vec<u8>,
    after_cr: bool,
    name: String,
    data: String,
    has_data: bool,
}

impl Decoder {
    fn feed(&mut self, bytes: &[u8], events: &mut Vec<(String, String)>) -> Result<(), String> {
        for &b in bytes {
            if self.after_cr {
                self.after_cr = false;
                if b == b'\n' {
                    continue;
                }
            }
            if b == b'\n' || b == b'\r' {
                self.after_cr = b == b'\r';
                let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                self.line_done(&line, events)?;
            } else {
                self.line.push(b);
                if self.line.len() > MAX_MESSAGE_BYTES + 16 {
                    return Err("event too large".into());
                }
            }
        }
        Ok(())
    }

    fn line_done(&mut self, line: &str, events: &mut Vec<(String, String)>) -> Result<(), String> {
        if line.is_empty() {
            let name = std::mem::take(&mut self.name);
            let data = std::mem::take(&mut self.data);
            if std::mem::take(&mut self.has_data) {
                let name = if name.is_empty() {
                    "message".to_owned()
                } else {
                    name
                };
                events.push((name, data));
            }
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(()); // a comment: the server keeping the connection alive
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.name = value.to_owned(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
                if self.data.len() > MAX_MESSAGE_BYTES {
                    return Err("event too large".into());
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// The version the server says it supports, in an `initialize` error's data: a string, or
/// the last of a list.
fn supported(error: &McpError) -> Option<String> {
    let McpError::Rpc { message, data, .. } = error else {
        return None;
    };
    let declared = match data.as_ref().and_then(|d| d.get("supported")) {
        Some(Value::String(version)) => Some(version.clone()),
        Some(Value::Array(versions)) => versions
            .iter()
            .rev()
            .find_map(Value::as_str)
            .map(str::to_owned),
        _ => None,
    };
    declared
        .or_else(|| {
            // "Unsupported protocol version: 2024-11-05, server supports: 2025-11-25"
            // After the last "support…" that isn't "unsupported".
            let lower = message.to_ascii_lowercase();
            let at = lower
                .rmatch_indices("support")
                .map(|(at, _)| at)
                .find(|&at| !lower[..at].ends_with("un"))?;
            let after = &lower[at..];
            after
                .split(|c: char| !(c.is_ascii_digit() || c == '-'))
                .rev()
                .find(|word| is_version(word))
                .map(str::to_owned)
        })
        .filter(|v| is_version(v))
}

/// A protocol version: a date, `YYYY-MM-DD`.
fn is_version(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
}

async fn current(token: &Option<Arc<dyn AccessToken>>) -> Result<Option<String>, McpError> {
    match token {
        Some(token) => token.current().await,
        None => Ok(None),
    }
}

async fn get(http: &Client, url: &Url, bearer: Option<&str>) -> Result<Response, McpError> {
    let mut request = http.get(url.clone()).header(ACCEPT, "text/event-stream");
    if let Some(bearer) = bearer.filter(|b| !b.is_empty()) {
        request = request.header(AUTHORIZATION, format!("Bearer {bearer}"));
    }
    request
        .send()
        .await
        .map_err(|e| McpError::Transport(network_message(&e)))
}

/// A 401 or 403, as [`McpHttp`](crate::McpHttp) reports it.
fn auth_error(response: &Response, token: &Option<Arc<dyn AccessToken>>) -> Option<McpError> {
    let status = response.status();
    if status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN {
        return None;
    }
    let code = status.as_u16();
    let sign_in = token.as_ref().is_some_and(|t| t.interactive());
    Some(McpError::Auth {
        message: if sign_in {
            format!("sign-in expired or revoked (HTTP {code})")
        } else {
            format!("token rejected (HTTP {code})")
        },
        challenge: OAuthChallenge::from_headers(response.headers()),
        sign_in,
    })
}

fn status_text(status: StatusCode) -> String {
    format!(
        "HTTP {} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
    .trim_end()
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(pieces: &[&str]) -> Vec<(String, String)> {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        for piece in pieces {
            decoder.feed(piece.as_bytes(), &mut events).unwrap();
        }
        events
    }

    #[test]
    fn events_are_named_and_split_across_pieces() {
        let events = decode(&[
            "event: endpoint\r\ndata: /message?session_id=1\r\n\r\n: keep-alive\n\n",
            "data: {\"a\":",
            "1}\n\nevent: ping\ndata:\n\nevent: message\rdata: x\rdata: y\r\r",
        ]);
        assert_eq!(
            events,
            vec![
                ("endpoint".into(), "/message?session_id=1".into()),
                ("message".into(), "{\"a\":1}".into()),
                ("ping".into(), String::new()),
                ("message".into(), "x\ny".into()),
            ]
        );
    }

    #[test]
    fn an_event_without_data_is_not_an_event() {
        assert!(decode(&["event: endpoint\n\n", ": comment\n\n"]).is_empty());
    }

    #[test]
    fn the_supported_version_is_read_from_the_error() {
        let error = |message: &str, data: Option<Value>| McpError::Rpc {
            code: -32602,
            message: message.into(),
            data,
        };
        let cases = [
            (
                error(
                    "Unsupported protocol version",
                    Some(json!({"supported": ["2024-11-05", "2025-03-26"]})),
                ),
                Some("2025-03-26"),
            ),
            (
                error("x", Some(json!({"supported": "2024-11-05"}))),
                Some("2024-11-05"),
            ),
            (
                error(
                    "Unsupported protocol version: 2024-11-05, server supports: 2025-11-25",
                    None,
                ),
                Some("2025-11-25"),
            ),
            (
                error("Unsupported protocol version: 2024-11-05", None),
                None,
            ),
            (error("x", Some(json!({"supported": ""}))), None),
            (McpError::Transport("supports: 2025-11-25".into()), None),
        ];
        for (error, expected) in cases {
            assert_eq!(supported(&error).as_deref(), expected, "{error:?}");
        }
    }
}
