//! A fake MCP server, in Streamable HTTP on 127.0.0.1: what a connector reaches. It
//! answers in JSON or SSE, splits the command list into pages, can ask for a token,
//! go down, forget the session and ask for confirmations in the middle of a command.
//! With `oauth` it's also its own sign-in service: metadata, dynamic registration,
//! PKCE, rotating refresh tokens, revocation; and, for the APIs, the "app only" sign-in.

#![allow(dead_code)]

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Asks the client for a confirmation and returns its answer (the `result`).
pub type Ask = Arc<dyn Fn(String, Value) -> BoxFuture<Map<String, Value>> + Send + Sync>;

/// A command of the server.
pub type Handler = Arc<dyn Fn(Map<String, Value>, Ask) -> BoxFuture<String> + Send + Sync>;

#[derive(Clone)]
struct Tool {
    name: String,
    description: String,
    schema: Value,
    run: Handler,
}

/// The fake server's state: the public fields are changed with [`FakeMcp::with`].
pub struct Inner {
    pub origin: String,
    pub url: String,
    pub token: Option<String>,
    pub instructions: Option<String>,
    pub sse: bool,
    pub down: bool,
    pub page_size: usize,
    /// The Authorization headers received, in order.
    pub authorizations: Vec<String>,
    /// The capabilities declared by the client in the last `initialize`.
    pub client_capabilities: Option<Value>,

    /// The server asks for an OAuth sign-in: without one of its access tokens it answers 401 with the challenge.
    pub oauth: bool,
    /// The service allows dynamic registration (RFC 7591).
    pub allow_registration: bool,
    /// The user, in the browser, says no.
    pub deny_sign_in: bool,
    /// The sign-in response declares another service (wrong iss).
    pub wrong_issuer: bool,
    /// The lifetime of access tokens, in seconds.
    pub access_lifetime: i64,
    /// The code and its request must carry `resource` (MCP); without it, they must not (API).
    pub require_resource: bool,
    /// The parameters of the last sign-in page opened.
    pub last_authorize: HashMap<String, String>,
    /// The tokens revoked by the client (`/revoke`).
    pub revoked: Vec<String>,
    pub refreshes: usize,
    /// The forms received by `/token`, in order.
    pub token_forms: Vec<HashMap<String, String>>,

    tools: Vec<Tool>,
    session: String,
    clients: HashMap<String, String>,
    codes: HashMap<String, (String, String, String, String)>,
    access: HashMap<String, String>,
    refresh: HashMap<String, String>,
}

struct Shared {
    inner: Mutex<Inner>,
    asked: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    requests: AtomicUsize,
    lists: AtomicUsize,
    asks: AtomicUsize,
    issued: AtomicUsize,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }
}

pub struct FakeMcp {
    pub url: String,
    pub origin: String,
    shared: Arc<Shared>,
    serving: tokio::task::JoinHandle<()>,
}

fn new_session() -> String {
    clak_wire::json::random_hex(16)
}

impl FakeMcp {
    pub async fn start() -> FakeMcp {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/mcp");
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                origin: origin.clone(),
                url: url.clone(),
                token: None,
                instructions: None,
                sse: false,
                down: false,
                page_size: 100,
                authorizations: Vec::new(),
                client_capabilities: None,
                oauth: false,
                allow_registration: true,
                deny_sign_in: false,
                wrong_issuer: false,
                access_lifetime: 3600,
                require_resource: true,
                last_authorize: HashMap::new(),
                revoked: Vec::new(),
                refreshes: 0,
                token_forms: Vec::new(),
                tools: Vec::new(),
                session: new_session(),
                clients: HashMap::new(),
                codes: HashMap::new(),
                access: HashMap::new(),
                refresh: HashMap::new(),
            }),
            asked: Mutex::new(HashMap::new()),
            requests: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            asks: AtomicUsize::new(0),
            issued: AtomicUsize::new(0),
        });
        let router = Router::new()
            .route("/mcp", post(handle))
            .route("/moved", post(moved))
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                get(resource_metadata),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                get(server_metadata),
            )
            .route("/register", post(register))
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .route("/revoke", post(revoke))
            .with_state(shared.clone());
        let serving = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        FakeMcp {
            url,
            origin,
            shared,
            serving,
        }
    }

    pub fn with<R>(&self, change: impl FnOnce(&mut Inner) -> R) -> R {
        change(&mut self.shared.lock())
    }

    pub fn requests(&self) -> usize {
        self.shared.requests.load(Ordering::SeqCst)
    }

    pub fn lists(&self) -> usize {
        self.shared.lists.load(Ordering::SeqCst)
    }

    pub fn registrations(&self) -> usize {
        self.with(|s| s.clients.keys().filter(|k| *k != "manual-client").count())
    }

    /// A client registered by hand in the service's settings.
    pub fn add_manual_client(&self) {
        self.with(|s| s.clients.insert("manual-client".into(), "*".into()));
    }

    /// All access tokens expire: the client must refresh them.
    pub fn expire_access_tokens(&self) {
        self.with(|s| s.access.clear());
    }

    /// The user removed the sign-in from the service: access and refresh tokens are no longer valid.
    pub fn revoke_everything(&self) {
        self.with(|s| {
            s.access.clear();
            s.refresh.clear();
        });
    }

    /// The server forgets the open sessions: the next request gets 404.
    pub fn expire_session(&self) {
        self.with(|s| s.session = new_session());
    }

    pub fn add(
        &self,
        name: &str,
        description: &str,
        run: Handler,
        schema: Option<Value>,
    ) -> &FakeMcp {
        self.with(|s| {
            s.tools.retain(|t| t.name != name);
            s.tools.push(Tool {
                name: name.into(),
                description: description.into(),
                schema: schema.unwrap_or_else(|| json!({"type": "object"})),
                run,
            });
        });
        self
    }

    pub fn add_fn(
        &self,
        name: &str,
        description: &str,
        run: impl Fn(&Map<String, Value>) -> String + Send + Sync + 'static,
    ) -> &FakeMcp {
        let run = Arc::new(run);
        self.add(
            name,
            description,
            Arc::new(move |arguments, _ask| {
                let text = run(&arguments);
                Box::pin(async move { text })
            }),
            None,
        )
    }

    /// The server shuts down: the port no longer answers.
    pub fn stop(&self) {
        self.serving.abort();
    }
}

impl Drop for FakeMcp {
    fn drop(&mut self) {
        self.serving.abort();
        self.shared.asked.lock().unwrap().clear();
    }
}

// =========================================================================
// The sign-in service
// =========================================================================

fn json_response(status: StatusCode, value: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

fn redirect(location: String) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

// Without OAuth the server declares no sign-in: there is no metadata.
async fn resource_metadata(State(s): State<Arc<Shared>>) -> Response {
    let s = s.lock();
    if !s.oauth {
        return StatusCode::NOT_FOUND.into_response();
    }
    json_response(
        StatusCode::OK,
        json!({ "resource": s.url, "authorization_servers": [s.origin], "scopes_supported": ["read", "write"] }),
    )
}

async fn server_metadata(State(s): State<Arc<Shared>>) -> Response {
    let s = s.lock();
    if !s.oauth {
        return StatusCode::NOT_FOUND.into_response();
    }
    let origin = &s.origin;
    let mut meta = json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/authorize"),
        "token_endpoint": format!("{origin}/token"),
        "revocation_endpoint": format!("{origin}/revoke"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "authorization_response_iss_parameter_supported": true,
    });
    if s.allow_registration {
        meta["registration_endpoint"] = json!(format!("{origin}/register"));
    }
    json_response(StatusCode::OK, meta)
}

async fn register(State(s): State<Arc<Shared>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let redirect = body["redirect_uris"][0].as_str().unwrap_or("").to_owned();
    if !redirect.starts_with("http://127.0.0.1:")
        || body["token_endpoint_auth_method"].as_str() != Some("none")
    {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "invalid_client_metadata"}),
        );
    }
    let id = format!("client-{}", s.issued.fetch_add(1, Ordering::SeqCst) + 1);
    s.lock().clients.insert(id.clone(), redirect);
    json_response(
        StatusCode::CREATED,
        json!({"client_id": id, "token_endpoint_auth_method": "none"}),
    )
}

async fn authorize(
    State(s): State<Arc<Shared>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let n = s.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let mut s = s.lock();
    s.last_authorize = q.clone();
    let get = |key: &str| q.get(key).cloned().unwrap_or_default();
    let redirect_uri = get("redirect_uri");
    let registered = s.clients.get(&get("client_id")).cloned();
    if registered.is_none_or(|r| r != "*" && r != redirect_uri)
        || get("code_challenge_method") != "S256"
        || get("response_type") != "code"
    {
        return (StatusCode::BAD_REQUEST, "bad authorization request").into_response();
    }
    let state = urlencode(&get("state"));
    let iss = urlencode(if s.wrong_issuer {
        "https://other.example"
    } else {
        &s.origin
    });
    if s.deny_sign_in {
        return redirect(format!(
            "{redirect_uri}?error=access_denied&state={state}&iss={iss}"
        ));
    }
    let code = format!("code-{n}");
    s.codes.insert(
        code.clone(),
        (
            get("client_id"),
            redirect_uri.clone(),
            get("code_challenge"),
            get("resource"),
        ),
    );
    redirect(format!(
        "{redirect_uri}?code={code}&state={state}&iss={iss}"
    ))
}

fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn token(
    State(shared): State<Arc<Shared>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let f = |key: &str| form.get(key).cloned().unwrap_or_default();
    let error = |e: &str| json_response(StatusCode::BAD_REQUEST, json!({"error": e}));
    let n = shared.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let mut s = shared.lock();
    s.token_forms.push(form.clone());
    let client;
    let mut rotates = true;
    match f("grant_type").as_str() {
        "authorization_code" => {
            let Some(code) = s.codes.remove(&f("code")) else {
                return error("invalid_grant");
            };
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(f("code_verifier").as_bytes()));
            let resource_ok = if s.require_resource {
                code.3 == s.url && f("resource") == s.url
            } else {
                code.3.is_empty() && !form.contains_key("resource")
            };
            if code.0 != f("client_id")
                || code.1 != f("redirect_uri")
                || code.2 != challenge
                || !resource_ok
            {
                return error("invalid_grant");
            }
            client = code.0;
        }
        "refresh_token" => {
            match s.refresh.remove(&f("refresh_token")) {
                Some(owner) if owner == f("client_id") => client = owner,
                _ => return error("invalid_grant"),
            }
            s.refreshes += 1;
        }
        // The "app only" sign-in: a client with its secret, no refresh token.
        "client_credentials" => {
            if f("client_id") != "app-client" || f("client_secret") != "app-secret" {
                return json_response(StatusCode::UNAUTHORIZED, json!({"error": "invalid_client"}));
            }
            client = f("client_id");
            rotates = false;
        }
        _ => return error("unsupported_grant_type"),
    }
    s.access.insert(format!("at-{n}"), client.clone());
    let mut answer = json!({
        "access_token": format!("at-{n}"),
        "token_type": "Bearer",
        "expires_in": s.access_lifetime,
        "scope": "read write",
    });
    if rotates {
        s.refresh.insert(format!("rt-{n}"), client); // rotates: the previous one is no longer valid
        answer["refresh_token"] = json!(format!("rt-{n}"));
    }
    json_response(StatusCode::OK, answer)
}

async fn revoke(
    State(s): State<Arc<Shared>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let token = form.get("token").cloned().unwrap_or_default();
    let mut s = s.lock();
    s.revoked.push(token.clone());
    s.refresh.remove(&token);
    s.access.remove(&token);
    StatusCode::OK.into_response()
}

async fn moved(State(s): State<Arc<Shared>>) -> Response {
    let url = s.lock().url.clone();
    (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, url)]).into_response()
}

// =========================================================================
// The MCP server
// =========================================================================

enum Out {
    Event(Value),
    Final(Value),
}

fn sse_frame(message: &Value) -> Bytes {
    Bytes::from(format!("event: message\ndata: {message}\n\n"))
}

async fn handle(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    shared.requests.fetch_add(1, Ordering::SeqCst);
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let (down, token, oauth, known, origin, session, sse) = {
        let mut s = shared.lock();
        s.authorizations.push(presented.clone());
        let known = presented
            .strip_prefix("Bearer ")
            .is_some_and(|t| s.access.contains_key(t));
        (
            s.down,
            s.token.clone(),
            s.oauth,
            known,
            s.origin.clone(),
            s.session.clone(),
            s.sse,
        )
    };
    if down {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if token.is_some_and(|t| presented != format!("Bearer {t}")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if oauth && !known {
        let challenge = format!(
            "Bearer realm=\"OAuth\", resource_metadata=\"{origin}/.well-known/oauth-protected-resource/mcp\", scope=\"read\""
        );
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, challenge)],
        )
            .into_response();
    }

    let body: Value = serde_json::from_slice(&body).unwrap();
    let Some(method) = body["method"].as_str().map(str::to_owned) else {
        // The client's answer to one of our questions.
        if let Some(id) = body["id"].as_str()
            && let Some(waiting) = shared.asked.lock().unwrap().remove(id)
        {
            let _ = waiting.send(body.clone());
        }
        return StatusCode::ACCEPTED.into_response();
    };
    let presented_session = headers
        .get("Mcp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if method != "initialize" && presented_session != session {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(request_id) = body.get("id").filter(|v| !v.is_null()).cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let parameters = body["params"].as_object().cloned().unwrap_or_default();

    let reply = |result: Result<Value, (i64, String)>| match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": request_id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}})
        }
    };

    let mut extra_session = false;
    let (first, rest) = match method.as_str() {
        "initialize" => {
            extra_session = true;
            (
                Out::Final(reply(Ok(initialize(&shared, &parameters)))),
                None,
            )
        }
        "tools/list" => (Out::Final(reply(Ok(list(&shared, &parameters)))), None),
        "ping" => (Out::Final(reply(Ok(json!({})))), None),
        "tools/call" => {
            let (events, mut outs) = mpsc::unbounded_channel::<Out>();
            let call = call(shared.clone(), parameters, events.clone());
            let id = request_id.clone();
            tokio::spawn(async move {
                let result = call.await;
                let final_reply = match result {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err((code, message)) => {
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                    }
                };
                let _ = events.send(Out::Final(final_reply));
            });
            let first = outs.recv().await.expect("the call always answers");
            (first, Some(outs))
        }
        _ => (
            Out::Final(reply(Err((-32601, format!("unknown method: {method}"))))),
            None,
        ),
    };

    let mut response = match (first, rest) {
        (Out::Final(message), _) if !sse => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            message.to_string(),
        )
            .into_response(),
        (Out::Final(message), _) => event_stream(vec![sse_frame(&message)], None),
        (Out::Event(message), rest) => event_stream(vec![sse_frame(&message)], rest),
    };
    if extra_session {
        response
            .headers_mut()
            .insert("Mcp-Session-Id", session.parse().unwrap());
    }
    response
}

/// An SSE stream: the messages already ready, then those that arrive until the response.
fn event_stream(ready: Vec<Bytes>, rest: Option<mpsc::UnboundedReceiver<Out>>) -> Response {
    let stream = futures_util::stream::unfold(
        (ready.into_iter(), rest, false),
        |(mut ready, mut rest, done)| async move {
            if let Some(frame) = ready.next() {
                return Some((Ok::<Bytes, Infallible>(frame), (ready, rest, done)));
            }
            if done {
                return None;
            }
            match rest.as_mut()?.recv().await? {
                Out::Event(message) => Some((Ok(sse_frame(&message)), (ready, rest, false))),
                Out::Final(message) => Some((Ok(sse_frame(&message)), (ready, rest, true))),
            }
        },
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

fn initialize(shared: &Shared, parameters: &Map<String, Value>) -> Value {
    let mut s = shared.lock();
    s.client_capabilities = parameters.get("capabilities").cloned();
    let mut result = json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "fake", "version": "1.0", "icons": [
            {"src": "https://fake.test/icon-16.png", "mimeType": "image/png", "sizes": ["16x16"]},
            {"src": "https://fake.test/icon-128.png", "mimeType": "image/png", "sizes": ["128x128"]},
        ]},
    });
    if let Some(instructions) = &s.instructions {
        result["instructions"] = json!(instructions);
    }
    result
}

fn list(shared: &Shared, parameters: &Map<String, Value>) -> Value {
    shared.lists.fetch_add(1, Ordering::SeqCst);
    let from: usize = parameters
        .get("cursor")
        .and_then(Value::as_str)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let s = shared.lock();
    let page: Vec<Value> = s
        .tools
        .iter()
        .skip(from)
        .take(s.page_size)
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.schema}))
        .collect();
    let mut result = json!({ "tools": page });
    if from + s.page_size < s.tools.len() {
        result["nextCursor"] = json!((from + s.page_size).to_string());
    }
    result
}

async fn call(
    shared: Arc<Shared>,
    parameters: Map<String, Value>,
    events: mpsc::UnboundedSender<Out>,
) -> Result<Value, (i64, String)> {
    let name = parameters
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let tool = shared.lock().tools.iter().find(|t| t.name == name).cloned();
    let Some(tool) = tool else {
        return Err((-32602, format!("unknown tool: {name}")));
    };
    let asker = shared.clone();
    let ask: Ask = Arc::new(move |message, schema| {
        let shared = asker.clone();
        let events = events.clone();
        Box::pin(async move {
            let id = format!("e{}", shared.asks.fetch_add(1, Ordering::SeqCst) + 1);
            let (answer, got) = oneshot::channel();
            shared.asked.lock().unwrap().insert(id.clone(), answer);
            let _ = events.send(Out::Event(json!({
                "jsonrpc": "2.0", "id": id, "method": "elicitation/create",
                "params": {"message": message, "requestedSchema": schema},
            })));
            match got.await {
                Ok(got) => got["result"].as_object().cloned().unwrap_or_else(|| {
                    let mut error = Map::new();
                    error.insert("action".into(), json!("error"));
                    error
                }),
                Err(_) => Map::new(),
            }
        })
    });
    let arguments = parameters
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let text = (tool.run)(arguments, ask).await;
    Ok(json!({"content": [{"type": "text", "text": text}], "isError": false}))
}

// =========================================================================
// The test secret store
// =========================================================================

/// An "encryption" for the tests: reversible, and the plain text doesn't appear in the file.
pub struct Xor;

impl clak_platform::BlobCipher for Xor {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, clak_platform::SecretError> {
        Ok(plain.iter().map(|b| b ^ 0x5A).rev().collect())
    }

    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, clak_platform::SecretError> {
        Ok(sealed.iter().rev().map(|b| b ^ 0x5A).collect())
    }
}

/// Another user's cipher: opens nothing.
pub struct OtherUser;

impl clak_platform::BlobCipher for OtherUser {
    fn protect(&self, _plain: &[u8]) -> Result<Vec<u8>, clak_platform::SecretError> {
        Err(clak_platform::SecretError("not this user".into()))
    }

    fn unprotect(&self, _sealed: &[u8]) -> Result<Vec<u8>, clak_platform::SecretError> {
        Err(clak_platform::SecretError("not this user".into()))
    }
}

/// `connectors.secrets` in the folder, as on Windows, with the test encryption.
pub fn test_box(home: &std::path::Path) -> Arc<dyn clak_platform::SecretBox> {
    Arc::new(clak_platform::FileBox::new(
        home.join("connectors.secrets"),
        Xor,
    ))
}

pub fn other_user_box(home: &std::path::Path) -> Arc<dyn clak_platform::SecretBox> {
    Arc::new(clak_platform::FileBox::new(
        home.join("connectors.secrets"),
        OtherUser,
    ))
}
