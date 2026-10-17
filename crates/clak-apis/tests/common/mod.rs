//! A fake REST service on 127.0.0.1: it remembers every request and answers with
//! `respond`, which usually echoes the request. On `/openapi.json` and `/openapi.yaml` it
//! serves the document it's given. With `require_token` it wants one of the valid tokens,
//! or answers 401: the access tests issue them and expire them by hand.

#![allow(dead_code)]

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Response, Uri};
use clak_apis::rest::{AccessToken, McpError};
use clak_connectors::BoxFuture;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    /// The path as it arrived, with its `%XX`.
    pub path: String,
    /// The query with the `?`, or empty.
    pub query: String,
    /// The headers, with lowercase names.
    pub headers: HashMap<String, String>,
    pub body: String,
    pub content_type: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: String,
    pub location: Option<String>,
    pub headers: Vec<(String, String)>,
    pub delay: Option<Duration>,
}

impl Reply {
    pub fn new(status: u16, body: &str) -> Reply {
        Reply {
            status,
            body: body.as_bytes().to_vec(),
            content_type: "application/json".into(),
            location: None,
            headers: Vec::new(),
            delay: None,
        }
    }

    pub fn redirect(status: u16, location: &str) -> Reply {
        Reply {
            location: Some(location.into()),
            ..Reply::new(status, "")
        }
    }

    pub fn typed(mut self, content_type: &str) -> Reply {
        self.content_type = content_type.into();
        self
    }

    pub fn bytes(mut self, bytes: Vec<u8>) -> Reply {
        self.body = bytes;
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn after(mut self, delay: Duration) -> Reply {
        self.delay = Some(delay);
        self
    }
}

type Respond = Arc<dyn Fn(&Seen) -> Reply + Send + Sync>;

pub struct State {
    pub document: Mutex<Option<String>>,
    pub requests: Mutex<Vec<Seen>>,
    respond: Mutex<Respond>,
    pub require_token: AtomicBool,
    pub tokens: Mutex<HashSet<String>>,
    /// The OAuth service without discovery: the issued codes, the valid refresh tokens, the counters.
    codes: Mutex<HashMap<String, (String, String)>>,
    refresh: Mutex<HashSet<String>>,
    pub refuse_refresh: AtomicBool,
    pub refreshes: AtomicUsize,
    pub client_tokens: AtomicUsize,
    issued: AtomicUsize,
    pub last_authorize: Mutex<HashMap<String, String>>,
}

pub struct FakeRest {
    pub origin: String,
    pub state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeRest {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeRest {
    pub async fn start() -> FakeRest {
        let state = Arc::new(State {
            document: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            respond: Mutex::new(echo()),
            require_token: AtomicBool::new(false),
            tokens: Mutex::new(HashSet::new()),
            codes: Mutex::new(HashMap::new()),
            refresh: Mutex::new(HashSet::new()),
            refuse_refresh: AtomicBool::new(false),
            refreshes: AtomicUsize::new(0),
            client_tokens: AtomicUsize::new(0),
            issued: AtomicUsize::new(0),
            last_authorize: Mutex::new(HashMap::new()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let shared = state.clone();
        let app = axum::Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let state = shared.clone();
                async move { handle(state, method, uri, headers, body).await }
            },
        );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeRest {
            origin,
            state,
            task,
        }
    }

    pub fn set_document(&self, text: &str) {
        *self.state.document.lock().unwrap() = Some(text.into());
    }

    pub fn respond(&self, f: impl Fn(&Seen) -> Reply + Send + Sync + 'static) {
        *self.state.respond.lock().unwrap() = Arc::new(f);
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.state.requests.lock().unwrap().clone()
    }

    pub fn last(&self) -> Seen {
        self.requests().last().cloned().expect("no request yet")
    }

    pub fn require_token(&self) {
        self.state.require_token.store(true, Ordering::SeqCst);
    }

    /// All access tokens expire: the client must renew them.
    pub fn expire_tokens(&self) {
        self.state.tokens.lock().unwrap().clear();
    }
}

fn echo() -> Respond {
    Arc::new(|seen: &Seen| Reply::new(200, &format!("{} {}{}", seen.method, seen.path, seen.query)))
}

async fn handle(
    state: Arc<State>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let path = uri.path().to_owned();
    if path == "/openapi.json" || path == "/openapi.yaml" {
        let document = state.document.lock().unwrap().clone();
        if let Some(document) = document {
            let kind = if path.ends_with("yaml") {
                "application/yaml"
            } else {
                "application/json"
            };
            return Response::builder()
                .header("content-type", kind)
                .body(Body::from(document))
                .unwrap();
        }
    }
    if path == "/oauth/authorize" {
        return authorize(&state, uri.query().unwrap_or(""));
    }
    if path == "/oauth/token" {
        return token(&state, &body);
    }
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if state.require_token.load(Ordering::SeqCst) {
        let valid = auth
            .strip_prefix("Bearer ")
            .is_some_and(|t| state.tokens.lock().unwrap().contains(t));
        if !valid {
            return Response::builder().status(401).body(Body::empty()).unwrap();
        }
    }
    let seen = Seen {
        method: method.to_string(),
        path,
        query: uri.query().map(|q| format!("?{q}")).unwrap_or_default(),
        headers: headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
        content_type: headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    };
    state.requests.lock().unwrap().push(seen.clone());
    let respond = state.respond.lock().unwrap().clone();
    let reply = respond(&seen);
    if let Some(delay) = reply.delay {
        tokio::time::sleep(delay).await;
    }
    let mut response = Response::builder()
        .status(reply.status)
        .header("content-type", reply.content_type.as_str());
    if let Some(location) = &reply.location {
        response = response.header("location", location.as_str());
    }
    for (name, value) in &reply.headers {
        response = response.header(name.as_str(), value.as_str());
    }
    response.body(Body::from(reply.body)).unwrap()
}

/// A fake OAuth access: the token is valid for the fake service until it expires; a
/// renewal gives a new one, unless the service refuses.
pub struct FakeOAuth {
    pub state: Arc<State>,
    pub token: Mutex<String>,
    pub refuse: AtomicBool,
    pub refreshes: AtomicUsize,
    pub dead: AtomicBool,
    pub unreachable: AtomicBool,
}

impl FakeOAuth {
    pub fn new(rest: &FakeRest) -> FakeOAuth {
        let first = "at-1".to_owned();
        rest.state.tokens.lock().unwrap().insert(first.clone());
        FakeOAuth {
            state: rest.state.clone(),
            token: Mutex::new(first),
            refuse: AtomicBool::new(false),
            refreshes: AtomicUsize::new(0),
            dead: AtomicBool::new(false),
            unreachable: AtomicBool::new(false),
        }
    }
}

impl AccessToken for FakeOAuth {
    fn interactive(&self) -> bool {
        true
    }

    fn current(&self) -> BoxFuture<'_, Result<Option<String>, McpError>> {
        Box::pin(async move { Ok(Some(self.token.lock().unwrap().clone())) })
    }

    fn renew(&self, _rejected: Option<String>) -> BoxFuture<'_, Result<bool, McpError>> {
        Box::pin(async move {
            if self.unreachable.load(Ordering::SeqCst) {
                return Err(McpError::Transport("connection refused".into()));
            }
            if self.refuse.load(Ordering::SeqCst) {
                self.dead.store(true, Ordering::SeqCst);
                return Ok(false);
            }
            let n = self.refreshes.fetch_add(1, Ordering::SeqCst) + 2;
            let fresh = format!("at-{n}");
            self.state.tokens.lock().unwrap().insert(fresh.clone());
            *self.token.lock().unwrap() = fresh;
            Ok(true)
        })
    }

    fn dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }
}

/// The text of all the blocks of a response, one per line.
pub fn content(response: &Value) -> String {
    response["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .map(|b| b["text"].as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub fn is_error(response: &Value) -> bool {
    response["is_error"].as_bool() == Some(true)
}

/// The arguments of a command from an inline JSON.
pub fn args(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// A Petstore for the tests: references, allOf, cycles, a file upload.
pub const PETSTORE: &str = r##"
{
  "openapi": "3.0.3",
  "info": { "title": "Petstore", "version": "1" },
  "servers": [ { "url": "/{version}", "variables": { "version": { "default": "v1" } } } ],
  "security": [ { "key": [] } ],
  "components": {
    "securitySchemes": {
      "oauth": { "type": "oauth2", "flows": {} },
      "key": { "type": "apiKey", "in": "header", "name": "X-API-Key" }
    },
    "parameters": {
      "PetId": { "name": "petId", "in": "path", "required": true, "schema": { "type": "string" } }
    },
    "schemas": {
      "Pet": {
        "allOf": [
          { "$ref": "#/components/schemas/Base" },
          { "type": "object", "required": ["name"], "properties": {
              "name": { "type": "string", "description": "What it's called." },
              "tag": { "type": "string", "enum": ["cat", "dog"] },
              "owner": { "$ref": "#/components/schemas/Owner" } } }
        ]
      },
      "Base": { "type": "object", "properties": { "id": { "type": "integer", "readOnly": true } } },
      "Owner": { "type": "object", "properties": { "name": { "type": "string" }, "pets": { "type": "array", "items": { "$ref": "#/components/schemas/Pet" } } } }
    }
  },
  "paths": {
    "/pets": {
      "get": {
        "operationId": "listPets", "summary": "List the pets.",
        "parameters": [
          { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20 }, "description": "How many." },
          { "name": "tags", "in": "query", "schema": { "type": "array", "items": { "type": "string" } } },
          { "name": "X-Trace", "in": "header", "schema": { "type": "string" } },
          { "name": "Accept", "in": "header", "schema": { "type": "string" } }
        ]
      },
      "post": {
        "operationId": "create pet!", "summary": "Add a pet.",
        "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Pet" } } } }
      }
    },
    "/pets/{petId}": {
      "parameters": [ { "$ref": "#/components/parameters/PetId" } ],
      "get": { "operationId": "getPet", "summary": "One pet." },
      "delete": { "summary": "Remove a pet.", "deprecated": true }
    },
    "/pets/{petId}/photo": {
      "post": {
        "operationId": "uploadPhoto",
        "requestBody": { "required": true, "content": { "multipart/form-data": { "schema": { "type": "object" } } } }
      }
    },
    "/pets/{petId}/notes": {
      "put": {
        "operationId": "setNotes",
        "parameters": [ { "$ref": "#/components/parameters/PetId" } ],
        "requestBody": { "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
      }
    }
  }
}
"##;

// ---- the fake OAuth service: a single client, registered by hand ----

pub const CLIENT_ID: &str = "app-1";
pub const CLIENT_SECRET: &str = "shh";

fn pairs(text: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(text.as_bytes())
        .into_owned()
        .collect()
}

fn authorize(state: &State, query: &str) -> Response<Body> {
    let query = pairs(query);
    *state.last_authorize.lock().unwrap() = query.clone();
    if query.get("client_id").map(String::as_str) != Some(CLIENT_ID) {
        return Response::builder().status(400).body(Body::empty()).unwrap();
    }
    let code = clak_wire::json::random_hex(16);
    let redirect = query.get("redirect_uri").cloned().unwrap_or_default();
    let challenge = query.get("code_challenge").cloned().unwrap_or_default();
    state
        .codes
        .lock()
        .unwrap()
        .insert(code.clone(), (redirect.clone(), challenge));
    let back = format!(
        "{redirect}?code={code}&state={}",
        url::form_urlencoded::byte_serialize(
            query.get("state").map_or("", String::as_str).as_bytes()
        )
        .collect::<String>()
    );
    Response::builder()
        .status(302)
        .header("location", back)
        .body(Body::empty())
        .unwrap()
}

fn token(state: &State, body: &[u8]) -> Response<Body> {
    let form = pairs(&String::from_utf8_lossy(body));
    let refuse = |error: &str| {
        Response::builder()
            .status(400)
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"error":"{error}"}}"#)))
            .unwrap()
    };
    let field = |key: &str| form.get(key).cloned().unwrap_or_default();
    if field("client_id") != CLIENT_ID {
        return refuse("invalid_client");
    }
    let mut refresh = true;
    match field("grant_type").as_str() {
        "authorization_code" => {
            use base64::Engine as _;
            use sha2::Digest as _;
            let issued = state.codes.lock().unwrap().remove(&field("code"));
            let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(sha2::Sha256::digest(field("code_verifier").as_bytes()));
            match issued {
                Some((redirect, challenge))
                    if redirect == field("redirect_uri") && challenge == verifier => {}
                _ => return refuse("invalid_grant"),
            }
        }
        "refresh_token" => {
            if state.refuse_refresh.load(Ordering::SeqCst)
                || !state
                    .refresh
                    .lock()
                    .unwrap()
                    .remove(&field("refresh_token"))
            {
                return refuse("invalid_grant");
            }
            state.refreshes.fetch_add(1, Ordering::SeqCst);
        }
        "client_credentials" => {
            if field("client_secret") != CLIENT_SECRET {
                return refuse("invalid_client");
            }
            state.client_tokens.fetch_add(1, Ordering::SeqCst);
            refresh = false;
        }
        _ => return refuse("unsupported_grant_type"),
    }
    let n = state.issued.fetch_add(1, Ordering::SeqCst) + 1;
    state.tokens.lock().unwrap().insert(format!("at-{n}"));
    let body = if refresh {
        state.refresh.lock().unwrap().insert(format!("rt-{n}"));
        format!(
            r#"{{"access_token":"at-{n}","refresh_token":"rt-{n}","token_type":"Bearer","expires_in":3600}}"#
        )
    } else {
        format!(r#"{{"access_token":"at-{n}","token_type":"bearer","expires_in":3600}}"#)
    };
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}
