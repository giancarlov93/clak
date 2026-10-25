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

use crate::password;
use axum::Router;
use axum::extract::{Json, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use clak_hub::consent::BoxFuture;
use clak_hub::{Broker, ConsentPrompt, ConsentRequest, Outbox};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
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
    /// Answered yes with "Always allow" ticked, until the hub reads it.
    always: Mutex<HashSet<String>>,
    /// The hub's time for an answer: for the page to say how long is left.
    timeout: Duration,
}

struct Open {
    request: ConsentRequest,
    asked: f64,
    answer: Option<oneshot::Sender<bool>>,
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
            always: Mutex::new(HashSet::new()),
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
                item["expires"] = json!(q.asked + self.0.timeout.as_secs_f64());
                item
            })
            .collect()
    }

    /// The user's answer. False if the question is no longer there (answered elsewhere,
    /// withdrawn, expired).
    pub fn answer(&self, id: &str, allow: bool, always: bool) -> bool {
        let sender = {
            let mut open = self.0.open.lock().unwrap_or_else(|e| e.into_inner());
            let Some(question) = open.iter_mut().find(|q| q.request.id == id) else {
                return false;
            };
            // Only where the question offers it.
            if allow && always && question.request.always.is_some() {
                self.0
                    .always
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.to_owned());
            }
            question.answer.take()
        };
        sender.is_some_and(|sender| sender.send(allow).is_ok())
    }
}

impl ConsentPrompt for WebPrompt {
    fn always(&self, id: &str) -> bool {
        self.0
            .always
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
    }

    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<bool, String>> {
        let (sender, receiver) = oneshot::channel();
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
                answer: Some(sender),
            });
        // The guard goes with the future: dropped unanswered, even before it runs, the
        // question leaves the page.
        Box::pin(async move {
            let _gone = gone;
            tokio::select! {
                answer = receiver => Ok(answer.unwrap_or(false)),
                _ = cancel.cancelled() => Ok(false),
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
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/state", get(state))
        .route("/api/answer", post(answer))
        .route("/api/revoke", post(revoke))
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
    let allow = body.get("allow").and_then(Value::as_bool) == Some(true);
    let always = body.get("always").and_then(Value::as_bool) == Some(true);
    if !page.prompt.answer(id, allow, always) {
        return refuse(
            StatusCode::NOT_FOUND,
            "This question is no longer open: answered, withdrawn or expired.",
        );
    }
    Json(json!({ "ok": true })).into_response()
}

/// What can be taken away, as the panel does: a permission, a connected agent, an "Always
/// allow", an allowed application. Never given: that happens only in the questions.
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
    let (outbox, _replies) = Outbox::channel();
    clak_hub::control_channel::panel_message(&page.broker, &message, &outbox, false);
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
        "agents": agents,
        "rules": list("rules"),
        "apps": list("apps"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clak_hub::{AgentIdentity, ConsentKind, ConsentLine, Hub, HubOptions, Platform};

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

    fn question(id: &str, always: bool) -> ConsentRequest {
        ConsentRequest {
            id: id.into(),
            kind: ConsentKind::Use,
            title: "Claude Code wants to use <b>notes</b>".into(),
            question: "Allow it?".into(),
            who: vec![ConsentLine::new("Agent", "Claude Code")],
            what: vec![ConsentLine::new("Program", "notes")],
            tools: vec![],
            warning: None,
            note: "Until the agent exits.".into(),
            always: always.then(|| "Always allow Claude Code to use notes".into()),
        }
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
            "Claude Code wants to use <b>notes</b>"
        );
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
                json!({ "id": "ask_1", "allow": true, "always": true }),
            )
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(yes.status(), 200);
        assert_eq!(asked.await.unwrap(), Ok(true));
        assert!(served.prompt.always("ask_1"));
        assert!(!served.prompt.always("ask_1"));
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
        assert!(served.prompt.answer("ask_2", true, true));
        assert_eq!(asked.await.unwrap(), Ok(true));
        assert!(!served.prompt.always("ask_2"));

        let cancel = CancellationToken::new();
        let asked = tokio::spawn(served.prompt.ask(question("ask_3", false), cancel.clone()));
        assert_eq!(served.prompt.list().len(), 1);
        cancel.cancel();
        assert_eq!(asked.await.unwrap(), Ok(false));
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
