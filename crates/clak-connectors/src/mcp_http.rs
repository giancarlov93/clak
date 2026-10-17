//! An MCP client over the "Streamable HTTP" transport: every request is a POST, and the
//! response is JSON or an SSE stream, inside which the server can in turn ask for
//! something (a confirmation) before answering.
//!
//! No redirects: they would carry the token to another host. No response beyond
//! [`MAX_MESSAGE_BYTES`]. After a 401 a new token is asked from whoever provides it
//! ([`AccessToken`]: with OAuth, the refresh) and the request is retried once.
//!
//! Canceling a request means dropping its future (with `tokio::time::timeout`, or
//! because whoever was waiting is gone): the connection closes with it.

use crate::BoxFuture;
use crate::oauth::OAuthChallenge;
use clak_wire::json::{self as wire, MAX_MESSAGE_BYTES};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// How a request to an MCP server can go wrong.
#[derive(Debug, Clone, thiserror::Error)]
pub enum McpError {
    /// The server rejected the token (401 or 403): retrying with the same one is noise.
    /// `challenge` is what the server said it wants (with OAuth, where to sign in);
    /// `sign_in`: the token came from an OAuth sign-in, and it couldn't be refreshed: a new sign-in is needed.
    #[error("{message}")]
    Auth {
        message: String,
        challenge: Option<OAuthChallenge>,
        sign_in: bool,
    },

    /// The server no longer recognizes the session (404 with `Mcp-Session-Id`): open another one.
    #[error("session expired")]
    SessionLost,

    /// A response that can't be understood, or an HTTP status that isn't a success, or no network.
    #[error("{0}")]
    Transport(String),

    /// A JSON-RPC error from the server: the request arrived, and was rejected.
    #[error("{message}")]
    Rpc { code: i32, message: String },
}

/// A request from the server in the middle of a call (`elicitation/create`): receives the
/// parameters and returns the `result` to send back.
pub type Elicit =
    Arc<dyn Fn(Map<String, Value>) -> BoxFuture<'static, Map<String, Value>> + Send + Sync>;

/// The token a connector presents: the current one, and a way to get another after a 401.
pub trait AccessToken: Send + Sync {
    /// An OAuth sign-in: when it's no longer valid, the user redoes it from the panel.
    fn interactive(&self) -> bool;

    fn current(&self) -> BoxFuture<'_, Result<Option<String>, McpError>>;

    /// After a 401 with `rejected`: true if there is now a different token to try.
    fn renew(&self, rejected: Option<String>) -> BoxFuture<'_, Result<bool, McpError>>;

    /// True if a refresh was rejected: a new sign-in by the user is needed.
    fn dead(&self) -> bool {
        false
    }

    /// Where the OAuth sign-in was done, for the panel; `None` with a token.
    fn host(&self) -> Option<String> {
        None
    }

    /// A new sign-in took this one's place: its refreshes are no longer saved.
    fn retire(&self) {}
}

/// A token pasted by the user: it doesn't refresh.
pub struct StaticToken(String);

impl StaticToken {
    pub fn new(token: impl Into<String>) -> StaticToken {
        StaticToken(token.into())
    }
}

impl AccessToken for StaticToken {
    fn interactive(&self) -> bool {
        false
    }

    fn current(&self) -> BoxFuture<'_, Result<Option<String>, McpError>> {
        Box::pin(async move { Ok(Some(self.0.clone())) })
    }

    fn renew(&self, _rejected: Option<String>) -> BoxFuture<'_, Result<bool, McpError>> {
        Box::pin(async { Ok(false) })
    }
}

/// A suitable HTTP client: no redirects, no cookies, a time limit for connecting.
/// No time limit for the response: a command can take a while; the caller sets it.
pub fn new_http_client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("the HTTP client can't start")
}

/// The message of a network failure, without the address (the context already says it):
/// the innermost cause, the one that really says what happened.
pub(crate) fn network_message(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "timed out".into();
    }
    let mut inner: &dyn std::error::Error = error;
    while let Some(next) = inner.source() {
        inner = next;
    }
    let text = inner.to_string();
    if text.is_empty() {
        error.to_string()
    } else {
        text
    }
}

#[derive(Default)]
struct State {
    session: Option<String>,
    protocol: Option<String>,
    instructions: Option<String>,
    server_name: Option<String>,
}

/// A server's MCP client. Used by several requests at once: the session and the
/// protocol version are under a lock, never held across an await.
pub struct McpHttp {
    http: Client,
    url: Url,
    token: Option<Arc<dyn AccessToken>>,
    can_elicit: bool,
    state: Mutex<State>,
    next_id: AtomicI64,
}

impl McpHttp {
    pub fn new(
        http: Client,
        url: Url,
        token: Option<Arc<dyn AccessToken>>,
        can_elicit: bool,
    ) -> McpHttp {
        McpHttp {
            http,
            url,
            token,
            can_elicit,
            state: Mutex::new(State::default()),
            next_id: AtomicI64::new(0),
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The instructions for how the server wants to be used, if it has any.
    pub fn instructions(&self) -> Option<String> {
        self.lock().instructions.clone()
    }

    pub fn server_name(&self) -> Option<String> {
        self.lock().server_name.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub async fn initialize(&self) -> Result<(), McpError> {
        self.lock().session = None;
        let mut capabilities = Map::new();
        if self.can_elicit {
            capabilities.insert("elicitation".into(), json!({}));
        }
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": capabilities,
                    "clientInfo": { "name": "clak", "version": env!("CARGO_PKG_VERSION") },
                }),
                None,
            )
            .await?;
        {
            let mut state = self.lock();
            state.protocol = Some(
                wire::str(result.get("protocolVersion"))
                    .unwrap_or(PROTOCOL_VERSION)
                    .to_owned(),
            );
            state.instructions = wire::str(result.get("instructions"))
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            state.server_name =
                wire::str(result.get("serverInfo").and_then(|s| s.get("name"))).map(str::to_owned);
        }
        self.notify("notifications/initialized").await
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

    async fn notify(&self, method: &str) -> Result<(), McpError> {
        let response = self
            .send(&json!({ "jsonrpc": "2.0", "method": method }))
            .await?;
        self.check(&response)
    }

    async fn request(
        &self,
        method: &str,
        parameters: Value,
        elicit: Option<&Elicit>,
    ) -> Result<Map<String, Value>, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let response = self
            .send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": parameters }))
            .await?;
        self.check(&response)?;
        if method == "initialize"
            && let Some(session) = response
                .headers()
                .get("Mcp-Session-Id")
                .and_then(|v| v.to_str().ok())
        {
            self.lock().session = Some(session.to_owned());
        }

        let media = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or("").trim().to_owned())
            .unwrap_or_default();
        if media.eq_ignore_ascii_case("text/event-stream") {
            return self.read_events(response, id, elicit).await;
        }
        let message = parse(&read_all(response).await?)?;
        answer(&message, id)?
            .ok_or_else(|| McpError::Transport("the answer is not for this request".into()))
    }

    /// The SSE stream up to the expected response, serving the server's requests meanwhile.
    /// Lines end with `\n`, `\r\n` or `\r`, as `StreamReader.ReadLine` reads them.
    async fn read_events(
        &self,
        mut response: Response,
        id: i64,
        elicit: Option<&Elicit>,
    ) -> Result<Map<String, Value>, McpError> {
        let mut line: Vec<u8> = Vec::new();
        let mut data = String::new();
        let mut after_cr = false;
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|e| McpError::Transport(network_message(&e)))?;
            let Some(chunk) = chunk else { break };
            for &b in chunk.iter() {
                if after_cr {
                    after_cr = false;
                    if b == b'\n' {
                        continue;
                    }
                }
                if b == b'\n' || b == b'\r' {
                    after_cr = b == b'\r';
                    let text = String::from_utf8_lossy(&std::mem::take(&mut line)).into_owned();
                    if let Some(answer) = self.event_line(&text, &mut data, id, elicit).await? {
                        return Ok(answer);
                    }
                } else {
                    line.push(b);
                    if line.len() > MAX_MESSAGE_BYTES + 16 {
                        return Err(McpError::Transport("event too large".into()));
                    }
                }
            }
        }
        if !line.is_empty() {
            let text = String::from_utf8_lossy(&line).into_owned();
            if let Some(answer) = self.event_line(&text, &mut data, id, elicit).await? {
                return Ok(answer);
            }
        }
        Err(McpError::Transport(
            "the server closed the stream without answering".into(),
        ))
    }

    /// A line of the stream: a piece of `data:`, or the empty line that closes an event.
    async fn event_line(
        &self,
        line: &str,
        data: &mut String,
        id: i64,
        elicit: Option<&Elicit>,
    ) -> Result<Option<Map<String, Value>>, McpError> {
        if line.is_empty() {
            if data.is_empty() {
                return Ok(None);
            }
            let message = parse(data.as_bytes())?;
            data.clear();
            if let Some(answer) = answer(&message, id)? {
                return Ok(Some(answer));
            }
            if let (Some(method), Some(request_id)) = (
                wire::str(message.get("method")),
                message.get("id").filter(|v| !v.is_null()),
            ) {
                let parameters = message
                    .get("params")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                self.reply(request_id.clone(), method, parameters, elicit)
                    .await?;
            }
            return Ok(None);
        }
        if let Some(chunk) = line.strip_prefix("data:") {
            let chunk = chunk.strip_prefix(' ').unwrap_or(chunk);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(chunk);
            if data.len() > MAX_MESSAGE_BYTES {
                return Err(McpError::Transport("event too large".into()));
            }
        }
        Ok(None)
    }

    /// A request from the server inside the stream: answered with another POST.
    async fn reply(
        &self,
        request_id: Value,
        method: &str,
        parameters: Map<String, Value>,
        elicit: Option<&Elicit>,
    ) -> Result<(), McpError> {
        let mut reply = Map::new();
        reply.insert("jsonrpc".into(), json!("2.0"));
        reply.insert("id".into(), request_id);
        match (method, elicit) {
            ("ping", _) => {
                reply.insert("result".into(), json!({}));
            }
            ("elicitation/create", Some(elicit)) => {
                reply.insert("result".into(), Value::Object(elicit(parameters).await));
            }
            _ => {
                reply.insert(
                    "error".into(),
                    json!({ "code": -32601, "message": format!("the client doesn't support '{method}'") }),
                );
            }
        }
        let response = self.send(&Value::Object(reply)).await?;
        self.check(&response)
    }

    /// The POST, and after a 401 a second attempt with a refreshed token, if there is one.
    async fn send(&self, message: &Value) -> Result<Response, McpError> {
        let used = match &self.token {
            Some(token) => token.current().await?,
            None => None,
        };
        let response = self.post(message, used.as_deref()).await?;
        let Some(token) = &self.token else {
            return Ok(response);
        };
        if response.status() != StatusCode::UNAUTHORIZED || !token.renew(used).await? {
            return Ok(response);
        }
        drop(response);
        let fresh = token.current().await?;
        self.post(message, fresh.as_deref()).await
    }

    async fn post(&self, message: &Value, bearer: Option<&str>) -> Result<Response, McpError> {
        let (session, protocol) = {
            let state = self.lock();
            (
                state.session.clone(),
                state
                    .protocol
                    .clone()
                    .unwrap_or_else(|| PROTOCOL_VERSION.to_owned()),
            )
        };
        let mut request = self
            .http
            .post(self.url.clone())
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(wire::encode(message));
        if let Some(bearer) = bearer.filter(|b| !b.is_empty()) {
            request = request.header(AUTHORIZATION, format!("Bearer {bearer}"));
        }
        if wire::str(message.get("method")) != Some("initialize") {
            request = request.header("MCP-Protocol-Version", protocol);
        }
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        request
            .send()
            .await
            .map_err(|e| McpError::Transport(network_message(&e)))
    }

    fn check(&self, response: &Response) -> Result<(), McpError> {
        let status = response.status();
        let code = status.as_u16();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            let sign_in = self.token.as_ref().is_some_and(|t| t.interactive());
            return Err(McpError::Auth {
                message: if sign_in {
                    format!("sign-in expired or revoked (HTTP {code})")
                } else {
                    format!("token rejected (HTTP {code})")
                },
                challenge: OAuthChallenge::from_headers(response.headers()),
                sign_in,
            });
        }
        if status == StatusCode::NOT_FOUND {
            let mut state = self.lock();
            if state.session.is_some() {
                state.session = None;
                return Err(McpError::SessionLost);
            }
        }
        if (300..400).contains(&code) {
            return Err(McpError::Transport(format!(
                "the server redirects elsewhere (HTTP {code}): redirects are not followed"
            )));
        }
        if !status.is_success() {
            return Err(McpError::Transport(
                format!("HTTP {code} {}", status.canonical_reason().unwrap_or(""))
                    .trim_end()
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

/// The response to request `id`, or `None` if the message is another one.
fn answer(message: &Map<String, Value>, id: i64) -> Result<Option<Map<String, Value>>, McpError> {
    if message.get("id").and_then(Value::as_i64) != Some(id) {
        return Ok(None);
    }
    if let Some(Value::Object(error)) = message.get("error") {
        return Err(McpError::Rpc {
            code: error
                .get("code")
                .and_then(Value::as_i64)
                .and_then(|c| i32::try_from(c).ok())
                .unwrap_or(0),
            message: wire::str(error.get("message"))
                .unwrap_or("server error")
                .to_owned(),
        });
    }
    Ok(Some(
        message
            .get("result")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    ))
}

fn parse(utf8: &[u8]) -> Result<Map<String, Value>, McpError> {
    match wire::parse(utf8) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(McpError::Transport(
            "the answer is not a JSON object".into(),
        )),
        Err(_) => Err(McpError::Transport("the answer is not JSON".into())),
    }
}

async fn read_all(mut response: Response) -> Result<Vec<u8>, McpError> {
    let mut buffer = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| McpError::Transport(network_message(&e)))?
    {
        buffer.extend_from_slice(&chunk);
        if buffer.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::Transport("answer too large".into()));
        }
    }
    Ok(buffer)
}

/// The `WWW-Authenticate` headers of a response, for whoever (OAuth) reads them.
pub(crate) fn www_authenticate(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
}
