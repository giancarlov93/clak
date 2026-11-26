//! The older HTTP+SSE transport, against a fake server on 127.0.0.1 that speaks only that:
//! a GET at `/sse` opens the stream and names the endpoint, the messages are POSTed there
//! and answered on the stream. The client gets there by itself, after the POST of
//! `initialize` is refused.

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use clak_connectors::{Elicit, McpError, McpHttp, PROTOCOL_VERSION, new_http_client, probe};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use url::Url;

#[derive(Default)]
struct Inner {
    /// The endpoint the stream names; by default `/message?session_id=…`.
    endpoint: Option<String>,
    /// The stream of the open session.
    stream: Option<mpsc::UnboundedSender<String>>,
    session: String,
    /// The protocol versions the client asked for in `initialize`, in order.
    protocols: Vec<String>,
    /// The only version the server speaks: it refuses the others, naming it in the message.
    only_version: Option<String>,
    /// The questions the server asked the client, waiting for an answer.
    asked: HashMap<String, oneshot::Sender<Value>>,
    /// The methods received, in order.
    methods: Vec<String>,
}

type Shared = Arc<Mutex<Inner>>;

struct Fake {
    origin: String,
    shared: Shared,
    serving: tokio::task::JoinHandle<()>,
}

impl Fake {
    async fn start() -> Fake {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let shared: Shared = Arc::default();
        let router = Router::new()
            .route("/sse", get(open))
            .route("/message", post(message))
            .with_state(shared.clone());
        let serving = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Fake {
            origin,
            shared,
            serving,
        }
    }

    fn url(&self) -> String {
        format!("{}/sse", self.origin)
    }

    fn client(&self, can_elicit: bool) -> McpHttp {
        McpHttp::new(
            new_http_client(),
            Url::parse(&self.url()).unwrap(),
            None,
            can_elicit,
        )
    }

    fn with<R>(&self, change: impl FnOnce(&mut Inner) -> R) -> R {
        change(&mut self.shared.lock().unwrap())
    }

    /// The server closes the stream.
    fn close(&self) {
        self.with(|s| s.stream = None);
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

fn frame(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

async fn open(State(shared): State<Shared>) -> Response {
    let (out, queue) = mpsc::unbounded_channel::<String>();
    let first = {
        let mut s = shared.lock().unwrap();
        s.session = clak_wire::json::random_hex(8);
        s.stream = Some(out);
        let endpoint = s
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("/message?session_id={}", s.session));
        // A comment and an unknown event before the endpoint: neither counts.
        format!(
            ": hello\n\n{}{}",
            frame("ping", "{}"),
            frame("endpoint", &endpoint)
        )
    };
    // The first chunk, then whatever the session sends, until the server closes it.
    let frames =
        futures_util::stream::unfold((Some(first), queue), |(first, mut queue)| async move {
            let chunk = match first {
                Some(first) => first,
                None => queue.recv().await?,
            };
            Some((Ok::<_, Infallible>(Bytes::from(chunk)), (None, queue)))
        });
    let body = Body::from_stream(frames);
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap()
}

async fn message(
    State(shared): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let message: Value = serde_json::from_slice(&body).unwrap();
    let (stream, known) = {
        let s = shared.lock().unwrap();
        (
            s.stream.clone(),
            query.get("session_id") == Some(&s.session),
        )
    };
    let Some(stream) = stream.filter(|_| known) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let method = message["method"].as_str().map(str::to_owned);
    let id = message.get("id").cloned().filter(|v| !v.is_null());
    match (method, id) {
        // An answer to a question of the server.
        (None, Some(id)) => {
            let waiting = shared
                .lock()
                .unwrap()
                .asked
                .remove(id.as_str().unwrap_or(""));
            if let Some(waiting) = waiting {
                let _ = waiting.send(message);
            }
        }
        (Some(method), id) => {
            shared.lock().unwrap().methods.push(method.clone());
            if let Some(id) = id {
                tokio::spawn(answer(shared.clone(), stream, id, method, message));
            }
        }
        _ => {}
    }
    StatusCode::ACCEPTED.into_response()
}

async fn answer(
    shared: Shared,
    stream: mpsc::UnboundedSender<String>,
    id: Value,
    method: String,
    message: Value,
) {
    let params = &message["params"];
    let result = match method.as_str() {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("").to_owned();
            let only = {
                let mut s = shared.lock().unwrap();
                s.protocols.push(asked.clone());
                s.only_version.clone()
            };
            if let Some(only) = only.filter(|v| *v != asked) {
                let error = json!({ "jsonrpc": "2.0", "id": id, "error": {
                    "code": -32602,
                    "message": format!("Unsupported protocol version: {asked}, server supports: {only}"),
                }});
                let _ = stream.send(frame("message", &error.to_string()));
                return;
            }
            json!({
                "protocolVersion": asked,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "Legacy", "version": "1" },
                "instructions": "Draw things.",
            })
        }
        "tools/list" => json!({ "tools": [
            { "name": "echo", "description": "Repeats the text.", "inputSchema": { "type": "object" } },
            { "name": "ask", "description": "Asks the user.", "inputSchema": { "type": "object" } },
        ]}),
        "tools/call" => match params["name"].as_str() {
            Some("echo") => {
                let text = params["arguments"]["text"].as_str().unwrap_or("");
                json!({ "content": [{ "type": "text", "text": format!("echo: {text}") }] })
            }
            Some("ask") => {
                let (got, reply) = oneshot::channel();
                shared.lock().unwrap().asked.insert("q1".into(), got);
                let _ = stream.send(frame(
                    "message",
                    &json!({
                        "jsonrpc": "2.0", "id": "q1", "method": "elicitation/create",
                        "params": { "message": "Which colour?", "requestedSchema": { "type": "object" } },
                    })
                    .to_string(),
                ));
                let reply = reply.await.unwrap_or(Value::Null);
                json!({ "content": [{ "type": "text", "text": reply["result"].to_string() }] })
            }
            _ => {
                let error = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": "no such tool" } });
                let _ = stream.send(frame("message", &error.to_string()));
                return;
            }
        },
        _ => json!({}),
    };
    let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    let _ = stream.send(frame("message", &reply.to_string()));
}

fn text(answer: &Map<String, Value>) -> String {
    answer["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[tokio::test]
async fn a_refused_post_falls_back_to_http_sse_and_everything_works_through_it() {
    let server = Fake::start().await;
    let client = server.client(false);
    client.initialize().await.unwrap();
    assert!(client.legacy_sse());
    assert_eq!(client.server_name().as_deref(), Some("Legacy"));
    assert_eq!(client.instructions().as_deref(), Some("Draw things."));
    assert_eq!(
        server.with(|s| s.protocols.clone()),
        vec![PROTOCOL_VERSION.to_owned()]
    );
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 2);
    let answer = client
        .call_tool("echo", &args(json!({"text": "hi"})), None)
        .await
        .unwrap();
    assert_eq!(text(&answer), "echo: hi");
    let error = client
        .call_tool("missing", &Map::new(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, McpError::Rpc { code: -32602, .. }),
        "{error:?}"
    );
    assert!(
        server.with(|s| s.methods.contains(&"notifications/initialized".to_owned())),
        "the client said it was ready"
    );
}

#[tokio::test]
async fn a_server_that_refuses_the_version_is_asked_again_with_the_one_it_names() {
    let server = Fake::start().await;
    server.with(|s| s.only_version = Some("2024-11-05".into()));
    let client = server.client(false);
    client.initialize().await.unwrap();
    assert_eq!(
        server.with(|s| s.protocols.clone()),
        vec![PROTOCOL_VERSION.to_owned(), "2024-11-05".to_owned()]
    );
    assert_eq!(client.list_tools().await.unwrap().len(), 2);
}

#[tokio::test]
async fn several_requests_at_once_get_their_own_answers() {
    let server = Fake::start().await;
    let client = Arc::new(server.client(false));
    client.initialize().await.unwrap();
    let calls = (0..8).map(|n| {
        let client = client.clone();
        tokio::spawn(async move {
            let answer = client
                .call_tool("echo", &args(json!({"text": n.to_string()})), None)
                .await
                .unwrap();
            assert_eq!(text(&answer), format!("echo: {n}"));
        })
    });
    for call in calls {
        call.await.unwrap();
    }
}

#[tokio::test]
async fn the_servers_question_reaches_the_running_command() {
    let server = Fake::start().await;
    let client = server.client(true);
    client.initialize().await.unwrap();
    let elicit = Elicit::new("ask", |parameters, _others| {
        Box::pin(async move {
            assert_eq!(parameters["message"], "Which colour?");
            Ok(args(
                json!({ "action": "accept", "content": { "colour": "red" } }),
            ))
        })
    });
    let answer = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool("ask", &Map::new(), Some(&elicit)),
    )
    .await
    .unwrap()
    .unwrap();
    let result: Value = serde_json::from_str(&text(&answer)).unwrap();
    assert_eq!(result["content"]["colour"], "red");
}

#[tokio::test]
async fn an_endpoint_on_another_server_is_not_followed() {
    let server = Fake::start().await;
    server.with(|s| s.endpoint = Some("http://other.example/message".into()));
    let error = server.client(false).initialize().await.unwrap_err();
    assert!(error.to_string().contains("another address"), "{error}");
}

#[tokio::test]
async fn when_the_stream_closes_the_client_knows_and_requests_fail() {
    let server = Fake::start().await;
    let client = server.client(false);
    client.initialize().await.unwrap();
    server.close();
    let reason = tokio::time::timeout(Duration::from_secs(10), client.ended())
        .await
        .expect("the end of the stream was never noticed");
    assert!(reason.contains("closed the stream"), "{reason}");
    let error = client.list_tools().await.unwrap_err();
    assert!(matches!(error, McpError::Transport(_)), "{error:?}");
}

#[tokio::test]
async fn an_address_that_speaks_neither_transport_keeps_the_first_refusal() {
    let server = Fake::start().await;
    let client = McpHttp::new(
        new_http_client(),
        Url::parse(&format!("{}/nothing", server.origin)).unwrap(),
        None,
        false,
    );
    let error = client.initialize().await.unwrap_err();
    assert!(
        matches!(&error, McpError::Transport(m) if m == "HTTP 404 Not Found"),
        "{error:?}"
    );
    assert!(!client.legacy_sse());
}

#[tokio::test]
async fn probing_an_http_sse_server_counts_its_commands() {
    let server = Fake::start().await;
    let outcome = probe(&server.url(), None).await;
    assert!(outcome.ok, "{}", outcome.message);
    assert_eq!(outcome.tools, 2);
    assert_eq!(outcome.server.as_deref(), Some("Legacy"));
}
