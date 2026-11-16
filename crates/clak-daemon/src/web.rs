//! The hub's questions on a web page, for `--web`: on a server or in a container, where
//! nobody watches the daemon's terminal.
//!
//! The hub gives an agent the link of its question at once ([`clak_hub::HubOptions::question_link`]),
//! the agent passes it on, and the user answers from a browser: through an SSH tunnel or
//! Tailscale, because the page listens on loopback only. The page also shows what is allowed
//! now, and takes it away.
//!
//! The agent runs as the same user and can reach the page as well as the user can: what it
//! lacks is the password, which only the user knows (see [`crate::password`]). So every
//! question, every answer and every revocation needs a session opened with it. Besides:
//!
//! - the session is a random token in an HttpOnly, SameSite=Strict cookie, kept in memory
//!   only, for [`SESSION`];
//! - a wrong password makes the next attempt wait, twice as long each time, up to a minute;
//! - every change is a POST with the `X-Clak` header, which a page of another site can't
//!   send without asking (CORS, which this server never allows), and an `Origin`, when the
//!   browser sends one, of this very host;
//! - the pages run only their own script (Content-Security-Policy) and can't be framed;
//!   what programs and agents say about themselves is shown as text, never as markup;
//! - Allow and Deny are enabled a moment after a question appears, as in the app's windows.
//!
//! An MCP server's question halfway through a command is answered here too: its form, checked
//! against it before it goes; its page, a link that opens in a tab of its own, and for a
//! command that waits for it, "Continue" once the user is done there.

use crate::password;
use axum::Router;
use axum::extract::{Json, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use clak_hub::consent::BoxFuture;
use clak_hub::{
    Asks, Broker, Choices, ConsentPrompt, ConsentRequest, Elicited, Outbox, Reply, Scope,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Where the page listens by default, on 127.0.0.1.
pub const PORT: u16 = 7780;

/// How long a session lasts without asking for the password again.
pub const SESSION: Duration = Duration::from_secs(12 * 3600);

/// The longest wait after wrong passwords.
const MOST_WAIT: Duration = Duration::from_secs(60);

const COOKIE: &str = "clak_session";

const INDEX: &str = include_str!("../web/index.html");
const SCRIPT: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/app.css");
/// The app's own drawing of a server's form and page.
const FORM: &str = include_str!("../../../ui/form.js");

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// --- the questions -----------------------------------------------------------------

/// The hub's questions, open until the user answers on the page or the hub withdraws them.
#[derive(Clone)]
pub struct WebPrompt(Arc<Questions>);

struct Questions {
    open: Mutex<Vec<Open>>,
    /// The hub's time for an answer: for the page to say how long is left.
    timeout: Duration,
}

struct Open {
    request: ConsentRequest,
    asked: f64,
    answer: Option<Answer>,
    /// A server's page, opened: the command waits for the user to finish there.
    opened: bool,
}

/// Where the answer goes: a permission's reply, or a server question's.
enum Answer {
    Reply(oneshot::Sender<Reply>),
    Elicited(oneshot::Sender<Elicited>),
}

/// What became of an answer to a server's question.
#[derive(Debug, PartialEq)]
pub enum Filled {
    /// The question is no longer open.
    Gone,
    /// Answered.
    Done,
    /// The page is open: the command waits for "Continue".
    Waiting,
}

/// Takes a question off the page when the hub no longer waits for it, however that goes.
struct Asked {
    questions: Arc<Questions>,
    id: String,
}

impl Drop for Asked {
    fn drop(&mut self) {
        let mut open = self
            .questions
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        open.retain(|q| q.request.id != self.id);
    }
}

impl WebPrompt {
    pub fn new(timeout: Duration) -> WebPrompt {
        WebPrompt(Arc::new(Questions {
            open: Mutex::new(Vec::new()),
            timeout,
        }))
    }

    /// The open questions, oldest first, as the page shows them.
    pub fn list(&self) -> Vec<Value> {
        let open = self.0.open.lock().unwrap_or_else(|e| e.into_inner());
        open.iter()
            .map(|q| {
                let mut item = q.request.to_json();
                item["asked"] = json!(q.asked);
                item["opened"] = json!(q.opened);
                item["expires"] = json!(q.asked + self.0.timeout.as_secs_f64());
                item
            })
            .collect()
    }

    /// The user's answer. False if the question is no longer there (answered elsewhere,
    /// withdrawn, expired). Only what the question offers counts: a scope, "always" or a
    /// block only for a command's question, and "always" only where it can be made.
    pub fn answer(&self, id: &str, reply: Reply) -> bool {
        let sender = {
            let mut open = self.0.open.lock().unwrap_or_else(|e| e.into_inner());
            let Some(question) = open.iter_mut().find(|q| q.request.id == id) else {
                return false;
            };
            let reply = Choices::allowed(question.request.choices.as_ref(), reply);
            match question.answer.take() {
                Some(Answer::Reply(sender)) => Some((sender, reply)),
                // A server's question has its own answers.
                other => {
                    question.answer = other;
                    None
                }
            }
        };
        sender.is_some_and(|(sender, reply)| sender.send(reply).is_ok())
    }

    /// The answer to a server's question: `accept` with the form's content (checked against
    /// it: the error is for the user), `open` for its page, `decline`, `cancel`. For a page,
    /// `accept` is "Continue", once it was opened.
    pub fn fill(
        &self,
        id: &str,
        action: &str,
        content: &Map<String, Value>,
    ) -> Result<Filled, String> {
        let mut open = self.0.open.lock().unwrap_or_else(|e| e.into_inner());
        let Some(question) = open.iter_mut().find(|q| q.request.id == id) else {
            return Ok(Filled::Gone);
        };
        if !matches!(question.answer, Some(Answer::Elicited(_))) {
            return Err("This question isn't a server's.".into());
        }
        let answer = match (action, &question.request.asks) {
            ("open", Some(Asks::Page(page))) if page.wait => {
                question.opened = true;
                return Ok(Filled::Waiting);
            }
            ("open", Some(Asks::Page(_))) => Elicited::Accepted(Map::new()),
            ("open", _) => return Err("This question has no page.".into()),
            ("accept", Some(Asks::Form(form))) => Elicited::Accepted(form.check(content)?),
            ("accept", Some(Asks::Page(_))) if question.opened => Elicited::Accepted(Map::new()),
            ("accept", _) => return Err("Open the page first.".into()),
            ("decline", _) => Elicited::Declined,
            _ => Elicited::Cancelled,
        };
        let Some(Answer::Elicited(sender)) = question.answer.take() else {
            return Ok(Filled::Gone);
        };
        Ok(if sender.send(answer).is_ok() {
            Filled::Done
        } else {
            Filled::Gone
        })
    }

    fn push(&self, request: ConsentRequest, answer: Answer) -> Asked {
        let gone = Asked {
            questions: self.0.clone(),
            id: request.id.clone(),
        };
        self.0
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Open {
                request,
                asked: now(),
                answer: Some(answer),
                opened: false,
            });
        gone
    }
}

impl ConsentPrompt for WebPrompt {
    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<Reply, String>> {
        let (sender, receiver) = oneshot::channel();
        let gone = self.push(request, Answer::Reply(sender));
        // The guard goes with the future: dropped unanswered, even before it runs, the
        // question leaves the page.
        Box::pin(async move {
            let _gone = gone;
            tokio::select! {
                answer = receiver => Ok(answer.unwrap_or(Reply::Deny)),
                _ = cancel.cancelled() => Ok(Reply::Deny),
            }
        })
    }

    fn elicit(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<Elicited, String>> {
        let (sender, receiver) = oneshot::channel();
        let gone = self.push(request, Answer::Elicited(sender));
        Box::pin(async move {
            let _gone = gone;
            tokio::select! {
                answer = receiver => Ok(answer.unwrap_or(Elicited::Cancelled)),
                _ = cancel.cancelled() => Ok(Elicited::Cancelled),
            }
        })
    }
}

// --- the server ----------------------------------------------------------------------

/// What the page needs: the questions, the hub, the password's hash and who has logged in.
pub struct Page {
    prompt: WebPrompt,
    broker: Broker,
    hash: String,
    sessions: Mutex<HashMap<String, Instant>>,
    throttle: Mutex<Throttle>,
    /// The first wait after a wrong password (a second; shorter in tests).
    first_wait: Duration,
}

#[derive(Default)]
struct Throttle {
    failures: u32,
    until: Option<Instant>,
}

impl Page {
    pub fn new(prompt: WebPrompt, broker: Broker, hash: String) -> Arc<Page> {
        Page::with_wait(prompt, broker, hash, Duration::from_secs(1))
    }

    fn with_wait(
        prompt: WebPrompt,
        broker: Broker,
        hash: String,
        first_wait: Duration,
    ) -> Arc<Page> {
        Arc::new(Page {
            prompt,
            broker,
            hash,
            sessions: Mutex::new(HashMap::new()),
            throttle: Mutex::new(Throttle::default()),
            first_wait,
        })
    }

    fn logged_in(&self, headers: &HeaderMap) -> bool {
        let Some(token) = cookie(headers) else {
            return false;
        };
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        sessions.retain(|_, until| *until > now);
        sessions.contains_key(&token)
    }

    /// How long until the next attempt, if one has to wait.
    fn wait(&self) -> Option<Duration> {
        let throttle = self.throttle.lock().unwrap_or_else(|e| e.into_inner());
        throttle
            .until
            .and_then(|until| until.checked_duration_since(Instant::now()))
            .filter(|left| !left.is_zero())
    }

    fn failed(&self) {
        let mut throttle = self.throttle.lock().unwrap_or_else(|e| e.into_inner());
        throttle.failures += 1;
        let wait = self
            .first_wait
            .saturating_mul(1 << (throttle.failures - 1).min(16))
            .min(MOST_WAIT);
        throttle.until = Some(Instant::now() + wait);
    }

    fn open_session(&self) -> String {
        *self.throttle.lock().unwrap_or_else(|e| e.into_inner()) = Throttle::default();
        let token = clak_wire::json::random_hex(32);
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token.clone(), Instant::now() + SESSION);
        token
    }
}

/// The page's routes. Every response carries the headers that keep it to itself.
pub fn router(page: Arc<Page>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/q/{id}", get(index))
        .route("/app.js", get(script))
        .route("/app.css", get(style))
        .route("/form.js", get(form))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/state", get(state))
        .route("/api/answer", post(answer))
        .route("/api/revoke", post(revoke))
        .route("/api/commands", post(commands))
        .layer(axum::middleware::map_response(guarded))
        .with_state(page)
}

/// Serves the page until `stop`.
pub async fn serve(listener: tokio::net::TcpListener, page: Arc<Page>, stop: CancellationToken) {
    let served = axum::serve(listener, router(page))
        .with_graceful_shutdown(async move { stop.cancelled().await })
        .await;
    if let Err(e) = served {
        eprintln!("the web page stopped: {e}");
    }
}

async fn guarded(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'",
        ),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

fn text(kind: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, kind)], body).into_response()
}

async fn index() -> Response {
    text("text/html; charset=utf-8", INDEX)
}

async fn script() -> Response {
    text("text/javascript; charset=utf-8", SCRIPT)
}

async fn form() -> Response {
    text("text/javascript; charset=utf-8", FORM)
}

async fn style() -> Response {
    text("text/css; charset=utf-8", STYLE)
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// The session cookie's token, if the request has one.
fn cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            pair.trim()
                .strip_prefix(&format!("{COOKIE}="))
                .map(str::to_owned)
        })
        .filter(|token| !token.is_empty())
}

/// A change can come only from this page: with the header a page of another site can't
/// send, and from this very origin when the browser says which.
fn from_the_page(headers: &HeaderMap) -> bool {
    if headers.get("x-clak").and_then(|v| v.to_str().ok()) != Some("1") {
        return false;
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let authority = origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://"))
        .map(|(_, rest)| rest.trim_end_matches('/'));
    host.is_some() && authority == host
}

/// The checks of a change: from the page, and logged in.
fn checked(page: &Page, headers: &HeaderMap) -> Option<Response> {
    if !from_the_page(headers) {
        return Some(refuse(StatusCode::FORBIDDEN, "Not from Clak's page."));
    }
    if !page.logged_in(headers) {
        return Some(refuse(StatusCode::UNAUTHORIZED, "Log in first."));
    }
    None
}

async fn login(
    State(page): State<Arc<Page>>,
    headers: HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> Response {
    if !from_the_page(&headers) {
        return refuse(StatusCode::FORBIDDEN, "Not from Clak's page.");
    }
    if let Some(left) = page.wait() {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "Too many wrong passwords: try again in {} s.",
                left.as_secs().max(1)
            ),
        );
    }
    let given = body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let hash = page.hash.clone();
    let right = tokio::task::spawn_blocking(move || password::verify(&hash, &given))
        .await
        .unwrap_or(false);
    if !right {
        page.failed();
        return refuse(StatusCode::UNAUTHORIZED, "Wrong password.");
    }
    let token = page.open_session();
    let cookie = format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION.as_secs()
    );
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

async fn logout(State(page): State<Arc<Page>>, headers: HeaderMap) -> Response {
    if !from_the_page(&headers) {
        return refuse(StatusCode::FORBIDDEN, "Not from Clak's page.");
    }
    if let Some(token) = cookie(&headers) {
        page.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&token);
    }
    (
        [(
            header::SET_COOKIE,
            format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
        )],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

async fn state(State(page): State<Arc<Page>>, headers: HeaderMap) -> Response {
    if !page.logged_in(&headers) {
        return refuse(StatusCode::UNAUTHORIZED, "Log in first.");
    }
    let snapshot = page.broker.run(|c| c.snapshot());
    let mut body = permissions(&snapshot);
    body["questions"] = Value::Array(page.prompt.list());
    body["now"] = json!(now());
    Json(body).into_response()
}

async fn answer(
    State(page): State<Arc<Page>>,
    headers: HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> Response {
    if let Some(refused) = checked(&page, &headers) {
        return refused;
    }
    let id = body.get("id").and_then(Value::as_str).unwrap_or_default();
    // A server's question: {action, content}.
    if let Some(action) = body.get("action").and_then(Value::as_str) {
        let content = body
            .get("content")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        return match page.prompt.fill(id, action, &content) {
            Ok(Filled::Gone) => refuse(
                StatusCode::NOT_FOUND,
                "This question is no longer open: answered, withdrawn or expired.",
            ),
            Ok(Filled::Done) => Json(json!({ "ok": true })).into_response(),
            Ok(Filled::Waiting) => Json(json!({ "ok": true, "waits": true })).into_response(),
            Err(problem) => refuse(StatusCode::BAD_REQUEST, &problem),
        };
    }
    let flag = |key: &str| body.get(key).and_then(Value::as_bool) == Some(true);
    // {allow, scope, always} or {block}: the question decides what of it counts.
    let reply = if flag("block") {
        Reply::Block
    } else if flag("allow") {
        Reply::Allow {
            scope: body
                .get("scope")
                .and_then(Value::as_str)
                .and_then(Scope::parse)
                .unwrap_or(Scope::Once),
            always: flag("always"),
        }
    } else {
        Reply::Deny
    };
    if !page.prompt.answer(id, reply) {
        return refuse(
            StatusCode::NOT_FOUND,
            "This question is no longer open: answered, withdrawn or expired.",
        );
    }
    Json(json!({ "ok": true })).into_response()
}

/// A program's commands as Clak keeps them (`{key}`), or what the user decides about them
/// (`{key, action, name, digest}`: approve, ask, off, approve_all with `commands`, reset,
/// forget_missing). The page is the user's, behind the password: it may approve, as the app
/// hosting the hub may; an approval names the version on screen.
async fn commands(
    State(page): State<Arc<Page>>,
    headers: HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> Response {
    if let Some(refused) = checked(&page, &headers) {
        return refused;
    }
    let key = body.get("key").and_then(Value::as_str);
    let name = body.get("name").and_then(Value::as_str);
    let digest = body.get("digest").and_then(Value::as_str);
    let message = match body.get("action").and_then(Value::as_str) {
        None => None,
        Some("approve") => Some(json!({
            "type": "command_state", "key": key, "name": name, "state": "approved", "digest": digest,
        })),
        Some(state @ ("ask" | "off")) => {
            Some(json!({ "type": "command_state", "key": key, "name": name, "state": state }))
        }
        Some("approve_all") => {
            Some(json!({ "type": "approve_all", "key": key, "commands": body.get("commands") }))
        }
        Some("reset") => Some(json!({ "type": "reset_commands", "key": key })),
        Some("forget_missing") => Some(json!({ "type": "forget_missing", "key": key })),
        Some(_) => return refuse(StatusCode::BAD_REQUEST, "Nothing to do of that kind."),
    };
    if let Some(Value::Object(message)) = message {
        let (outbox, _replies) = Outbox::channel();
        clak_hub::control_channel::panel_message(&page.broker, &message, &outbox, true);
    }
    Json(page.broker.run(|c| c.panel_commands(key))).into_response()
}

/// What can be taken away, as the panel does: a permission, a connected agent, an "Always
/// allow", an allowed application; and a block lifted, which only lets the agent ask again.
/// Never given: that happens only in the questions, and in the commands review.
async fn revoke(
    State(page): State<Arc<Page>>,
    headers: HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> Response {
    if let Some(refused) = checked(&page, &headers) {
        return refused;
    }
    let id = body.get("id").and_then(Value::as_str).unwrap_or_default();
    let message = match body.get("kind").and_then(Value::as_str) {
        Some("grant") => json!({ "type": "revoke", "grant_id": id }),
        Some("agent") => json!({ "type": "forget_session", "session_id": id }),
        Some("rule") => json!({ "type": "forget_rule", "id": id }),
        Some("app") => json!({ "type": "forget_app", "key": id }),
        Some("block") => json!({ "type": "unblock", "id": id }),
        _ => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "Nothing of that kind to take away.",
            );
        }
    };
    let Value::Object(message) = message else {
        unreachable!("an object above")
    };
    // Behind the password the page is the user's: it may lift a block, as the app hosting
    // the hub may (on the channel an agent could lift its own).
    let inside = message.get("type").and_then(Value::as_str) == Some("unblock");
    let (outbox, _replies) = Outbox::channel();
    clak_hub::control_channel::panel_message(&page.broker, &message, &outbox, inside);
    Json(json!({ "ok": true })).into_response()
}

/// What is allowed now, from the hub's state: each permission with its agent and its
/// program by name.
fn permissions(snapshot: &Value) -> Value {
    let list = |key: &str| snapshot[key].as_array().cloned().unwrap_or_default();
    let sessions = list("sessions");
    let resources = list("resources");
    let find = |items: &[Value], id: &Value, field: &str| -> Value {
        items
            .iter()
            .find(|item| &item["id"] == id)
            .map(|item| item[field].clone())
            .unwrap_or(Value::Null)
    };
    let grants: Vec<Value> = list("grants")
        .iter()
        .map(|g| {
            json!({
                "id": g["id"],
                "agent": find(&sessions, &g["session_id"], "label"),
                "program": find(&resources, &g["resource_id"], "name"),
                "since": g["since"],
                "tools": g["tools"],
                "calls": g["calls"],
            })
        })
        .collect();
    // Shown, not lifted: only the app hosting the hub lifts a block.
    let blocks: Vec<Value> = list("blocks")
        .iter()
        .map(|b| {
            json!({
                "id": b["id"], "agent": b["agent"], "program": b["program"], "since": b["since"],
            })
        })
        .collect();
    let agents: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s["id"], "label": s["label"], "exe": s["exe"], "cwd": s["cwd"],
                "via": s["via"], "since": s["since"],
            })
        })
        .collect();
    json!({
        "grants": grants,
        "blocks": blocks,
        "commands": snapshot["commands"],
        "agents": agents,
        "rules": list("rules"),
        "apps": list("apps"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clak_hub::{AgentIdentity, ConsentKind, ConsentLine, Hub, HubOptions, Platform};
    use std::collections::HashSet;

    struct NoSystem;

    impl Platform for NoSystem {
        fn agent_of(
            &self,
            _pid: Option<u32>,
            _request: &Map<String, Value>,
            _authorized: &HashSet<String>,
        ) -> Option<AgentIdentity> {
            None
        }
        fn is_alive(&self, _agent: &AgentIdentity) -> bool {
            true
        }
        fn attest_loopback(&self, _peer_port: u16, _local_port: u16) -> Value {
            json!({})
        }
        fn executable_hash(&self, _path: &str) -> Option<String> {
            None
        }
    }

    struct Served {
        base: String,
        host: String,
        prompt: WebPrompt,
        hub: Option<Hub>,
        stop: CancellationToken,
        home: std::path::PathBuf,
        http: reqwest::Client,
    }

    impl Served {
        async fn start() -> Served {
            let home =
                std::env::temp_dir().join(format!("clak-web-{}", clak_wire::json::random_hex(6)));
            std::fs::create_dir_all(&home).unwrap();
            let prompt = WebPrompt::new(Duration::from_secs(600));
            let mut options = HubOptions::new(Arc::new(NoSystem));
            options.consent = Some(Arc::new(prompt.clone()));
            options.settings = false;
            let hub = Hub::try_start(&home, options, None).await.unwrap().unwrap();
            let page = Page::with_wait(
                prompt.clone(),
                hub.broker().clone(),
                password::hash("correct horse battery").unwrap(),
                Duration::from_millis(200),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let host = listener.local_addr().unwrap().to_string();
            let stop = CancellationToken::new();
            tokio::spawn(serve(listener, page, stop.clone()));
            Served {
                base: format!("http://{host}"),
                host,
                prompt,
                hub: Some(hub),
                stop,
                home,
                http: reqwest::Client::new(),
            }
        }

        fn post(&self, path: &str, body: Value) -> reqwest::RequestBuilder {
            self.http
                .post(format!("{}{path}", self.base))
                .header("x-clak", "1")
                .header("content-type", "application/json")
                .body(body.to_string())
        }

        async fn login(&self, password: &str) -> reqwest::Response {
            self.post("/api/login", json!({ "password": password }))
                .send()
                .await
                .unwrap()
        }

        async fn session(&self) -> String {
            let response = self.login("correct horse battery").await;
            assert_eq!(response.status(), 200);
            let cookie = response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .to_owned();
            assert!(
                cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"),
                "{cookie}"
            );
            cookie.split(';').next().unwrap().to_owned()
        }

        async fn state(&self, cookie: &str) -> Value {
            let response = self
                .http
                .get(format!("{}/api/state", self.base))
                .header("cookie", cookie)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            serde_json::from_str(&response.text().await.unwrap()).unwrap()
        }

        async fn stop(mut self) {
            self.stop.cancel();
            self.hub.take().unwrap().stop().await;
            std::fs::remove_dir_all(&self.home).ok();
        }
    }

    /// A command's question; with `always`, it can be made standing.
    fn question(id: &str, always: bool) -> ConsentRequest {
        ConsentRequest {
            asks: None,
            id: id.into(),
            kind: ConsentKind::Call,
            title: "Claude Code wants to run “read” in <b>notes</b>".into(),
            question: "Allow it?".into(),
            who: vec![ConsentLine::new("Agent", "Claude Code")],
            what: vec![ConsentLine::new("Program", "notes")],
            tools: vec![],
            warning: None,
            note: "Until the agent exits.".into(),
            arguments: vec![ConsentLine::new("line", "2")],
            choices: Some(Choices {
                scopes: vec![
                    (Scope::Once, "Only this call".into()),
                    (Scope::Session, "“read” for this agent's session".into()),
                    (Scope::Approve, "Approve “read”, as it is, for notes".into()),
                ],
                always: always.then(|| "Always allow Claude Code to use notes".into()),
                block: Some("Block notes for Claude Code".into()),
            }),
        }
    }

    fn server_question(id: &str, asks: Asks) -> ConsentRequest {
        ConsentRequest {
            asks: Some(asks),
            kind: ConsentKind::Confirm,
            choices: None,
            arguments: vec![],
            ..question(id, false)
        }
    }

    #[tokio::test]
    async fn a_server_s_form_and_page_are_answered_on_the_page() {
        let served = Served::start().await;
        let cookie = served.session().await;
        let answer = |body: Value| {
            served
                .post("/api/answer", body)
                .header("cookie", &cookie)
                .send()
        };

        let form = clak_hub::Form::parse(Some(&json!({
            "properties": {"name": {"type": "string", "title": "Name"}},
            "required": ["name"],
        })))
        .unwrap();
        let asked = tokio::spawn(served.prompt.elicit(
            server_question("ask_2", Asks::Form(form)),
            CancellationToken::new(),
        ));
        let state = served.state(&cookie).await;
        assert_eq!(state["questions"][0]["form"]["fields"][0]["name"], "name");
        // A form that doesn't fit stays, with what's wrong.
        let wrong = answer(json!({"id": "ask_2", "action": "accept", "content": {}}))
            .await
            .unwrap();
        assert_eq!(wrong.status(), 400);
        assert_eq!(
            wrong.json::<Value>().await.unwrap()["error"],
            "“Name” is required."
        );
        let sent =
            answer(json!({"id": "ask_2", "action": "accept", "content": {"name": "Ada", "x": 1}}))
                .await
                .unwrap();
        assert_eq!(sent.status(), 200);
        assert_eq!(
            asked.await.unwrap(),
            Ok(Elicited::Accepted(
                json!({"name": "Ada"}).as_object().unwrap().clone()
            ))
        );

        // A page the command waits for: opened, then "Continue".
        let page = clak_hub::Page::parse("https://example.com/connect", true).unwrap();
        let asked = tokio::spawn(served.prompt.elicit(
            server_question("ask_3", Asks::Page(page)),
            CancellationToken::new(),
        ));
        let state = served.state(&cookie).await;
        assert_eq!(state["questions"][0]["page"]["host"], "example.com");
        let early = answer(json!({"id": "ask_3", "action": "accept"}))
            .await
            .unwrap();
        assert_eq!(early.status(), 400);
        let opened = answer(json!({"id": "ask_3", "action": "open"}))
            .await
            .unwrap();
        assert_eq!(
            opened.json::<Value>().await.unwrap(),
            json!({"ok": true, "waits": true})
        );
        assert_eq!(served.state(&cookie).await["questions"][0]["opened"], true);
        let done = answer(json!({"id": "ask_3", "action": "accept"}))
            .await
            .unwrap();
        assert_eq!(done.status(), 200);
        assert_eq!(asked.await.unwrap(), Ok(Elicited::Accepted(Map::new())));

        // The page's own script draws them.
        let script = served
            .http
            .get(format!("{}/form.js", served.base))
            .send()
            .await
            .unwrap();
        assert_eq!(script.status(), 200);
        assert!(script.text().await.unwrap().contains("const ClakForm"));
    }

    #[tokio::test]
    async fn nothing_without_the_password_and_wrong_ones_wait_longer() {
        let served = Served::start().await;
        // The page itself holds nothing: anyone may load it, with its guarding headers.
        let page = served
            .http
            .get(format!("{}/q/ask_1", served.base))
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), 200);
        let policy = page.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(policy.contains("script-src 'self'") && policy.contains("frame-ancestors 'none'"));
        assert_eq!(page.headers()[header::X_FRAME_OPTIONS], "DENY");

        let state = served
            .http
            .get(format!("{}/api/state", served.base))
            .send()
            .await
            .unwrap();
        assert_eq!(state.status(), 401);
        let forged = served
            .http
            .get(format!("{}/api/state", served.base))
            .header("cookie", "clak_session=0123")
            .send()
            .await
            .unwrap();
        assert_eq!(forged.status(), 401);

        // Without the header, or from another site: not from the page.
        let bare = served
            .http
            .post(format!("{}/api/login", served.base))
            .header("content-type", "application/json")
            .body(json!({ "password": "correct horse battery" }).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(bare.status(), 403);
        let elsewhere = served
            .post("/api/login", json!({ "password": "correct horse battery" }))
            .header("origin", "http://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(elsewhere.status(), 403);
        let same = served
            .post("/api/logout", json!({}))
            .header("origin", format!("http://{}", served.host))
            .send()
            .await
            .unwrap();
        assert_eq!(same.status(), 200);

        // A wrong password, then even the right one waits; after the wait it goes.
        assert_eq!(served.login("wrong").await.status(), 401);
        let early = served.login("correct horse battery").await;
        assert_eq!(early.status(), 429);
        assert!(early.text().await.unwrap().contains("try again in"));
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(served.login("wrong").await.status(), 401);
        // The second wait is twice the first.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(served.login("correct horse battery").await.status(), 429);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let cookie = served.session().await;
        served.state(&cookie).await;

        // Logged out, the session is over.
        let out = served
            .post("/api/logout", json!({}))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(out.status(), 200);
        let after = served
            .http
            .get(format!("{}/api/state", served.base))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(after.status(), 401);
        served.stop().await;
    }

    #[tokio::test]
    async fn a_question_is_answered_on_the_page_and_leaves_it() {
        let served = Served::start().await;
        let cookie = served.session().await;
        assert_eq!(served.state(&cookie).await["questions"], json!([]));

        let asked = tokio::spawn(
            served
                .prompt
                .ask(question("ask_1", true), CancellationToken::new()),
        );
        let state = served.state(&cookie).await;
        let questions = state["questions"].as_array().unwrap();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0]["id"], "ask_1");
        // As text: the page puts it in the DOM as such.
        assert_eq!(
            questions[0]["title"],
            "Claude Code wants to run “read” in <b>notes</b>"
        );
        assert_eq!(questions[0]["arguments"][0]["value"], "2");
        assert_eq!(
            questions[0]["choices"]["block"],
            "Block notes for Claude Code"
        );
        assert_eq!(questions[0]["choices"]["scopes"][1]["value"], "session");
        let left = questions[0]["expires"].as_f64().unwrap() - state["now"].as_f64().unwrap();
        assert!((590.0..=600.0).contains(&left), "{left}");

        // Not logged in, no answer.
        let stranger = served
            .post("/api/answer", json!({ "id": "ask_1", "allow": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(stranger.status(), 401);

        let yes = served
            .post(
                "/api/answer",
                json!({ "id": "ask_1", "allow": true, "scope": "session", "always": true }),
            )
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(yes.status(), 200);
        assert_eq!(
            asked.await.unwrap(),
            Ok(Reply::Allow {
                scope: Scope::Session,
                always: true
            })
        );
        assert_eq!(served.state(&cookie).await["questions"], json!([]));

        // Answered already: nothing to answer.
        let again = served
            .post("/api/answer", json!({ "id": "ask_1", "allow": false }))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(again.status(), 404);

        // "Always" only where offered; a no; a question the hub withdraws leaves the page.
        let asked = tokio::spawn(
            served
                .prompt
                .ask(question("ask_2", false), CancellationToken::new()),
        );
        while served.prompt.list().is_empty() {
            tokio::task::yield_now().await;
        }
        assert!(served.prompt.answer(
            "ask_2",
            Reply::Allow {
                scope: Scope::Approve,
                always: true
            }
        ));
        assert_eq!(
            asked.await.unwrap(),
            Ok(Reply::Allow {
                scope: Scope::Approve,
                always: false
            })
        );
        // A block, through the page.
        let asked = tokio::spawn(
            served
                .prompt
                .ask(question("ask_5", false), CancellationToken::new()),
        );
        while served.prompt.list().is_empty() {
            tokio::task::yield_now().await;
        }
        let blocked = served
            .post("/api/answer", json!({ "id": "ask_5", "block": true }))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(blocked.status(), 200);
        assert_eq!(asked.await.unwrap(), Ok(Reply::Block));

        let cancel = CancellationToken::new();
        let asked = tokio::spawn(served.prompt.ask(question("ask_3", false), cancel.clone()));
        assert_eq!(served.prompt.list().len(), 1);
        cancel.cancel();
        assert_eq!(asked.await.unwrap(), Ok(Reply::Deny));
        assert!(served.prompt.list().is_empty());
        // Dropped without ever running: gone too.
        drop(
            served
                .prompt
                .ask(question("ask_4", false), CancellationToken::new()),
        );
        assert!(served.prompt.list().is_empty());
        served.stop().await;
    }

    #[tokio::test]
    async fn the_commands_are_read_and_decided_only_once_logged_in() {
        let served = Served::start().await;
        let stranger = served
            .post("/api/commands", json!({ "key": "app:x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(stranger.status(), 401);
        let cookie = served.session().await;
        let read = served
            .post("/api/commands", json!({ "key": "app:x" }))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(read.status(), 200);
        let body: Value = serde_json::from_str(&read.text().await.unwrap()).unwrap();
        assert_eq!(body["type"], "commands");
        assert_eq!(body["commands"], json!([]));
        for (action, status) in [
            ("forget_missing", 200),
            ("approve_all", 200),
            ("everything", 400),
        ] {
            let response = served
                .post("/api/commands", json!({ "key": "app:x", "action": action }))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{action}");
        }
        assert!(served.state(&cookie).await["commands"].is_array());
        served.stop().await;
    }

    #[tokio::test]
    async fn only_known_kinds_are_taken_away() {
        let served = Served::start().await;
        let cookie = served.session().await;
        let state = served.state(&cookie).await;
        for key in ["grants", "agents", "rules", "apps"] {
            assert_eq!(state[key], json!([]), "{key}");
        }
        for (kind, status) in [
            ("grant", 200),
            ("agent", 200),
            ("rule", 200),
            ("app", 200),
            ("block", 200),
            ("everything", 400),
        ] {
            let response = served
                .post("/api/revoke", json!({ "kind": kind, "id": "x" }))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{kind}");
        }
        let stranger = served
            .post("/api/revoke", json!({ "kind": "grant", "id": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(stranger.status(), 401);
        served.stop().await;
    }

    #[test]
    fn what_is_allowed_names_its_agent_and_program() {
        let snapshot = json!({
            "sessions": [{ "id": "s1", "label": "Claude Code", "exe": "/x/claude", "cwd": "/srv", "via": "cli", "since": 1.0, "pid": 9 }],
            "resources": [{ "id": "r1", "name": "notes" }],
            "grants": [{ "id": "g1", "session_id": "s1", "resource_id": "r1", "since": 2.0, "tools": ["read"], "calls": 3 }],
            "rules": [{ "id": "rule1" }],
            "apps": [{ "key": "k1" }],
        });
        let view = permissions(&snapshot);
        assert_eq!(view["grants"][0]["agent"], "Claude Code");
        assert_eq!(view["grants"][0]["program"], "notes");
        assert_eq!(view["grants"][0]["calls"], 3);
        assert_eq!(view["agents"][0]["cwd"], "/srv");
        assert!(view["agents"][0].get("pid").is_none());
        assert_eq!(view["rules"][0]["id"], "rule1");
        assert_eq!(view["apps"][0]["key"], "k1");
    }
}
