//! The MCP client, probing a server and the registry, against a fake MCP server on
//! 127.0.0.1. The parts that go through the broker (permissions, snapshot, retries) come
//! with the hub.

mod fake;

use clak_connectors::{
    AccessToken, ConnectorEntry, ConnectorStore, Elicit, McpError, McpHttp, StaticToken, fill,
    new_http_client, probe,
};
use clak_hub::cli_names;
use fake::FakeMcp;
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;

fn tools(server: &FakeMcp) {
    server
        .add_fn("echo", "Repeats the text.", |a| {
            format!(
                "echo: {}",
                a.get("text").and_then(Value::as_str).unwrap_or("")
            )
        })
        .add_fn("time", "What time it is.", |_| "noon".into())
        .add_fn("sum", "Adds two numbers.", |a| {
            format!(
                "{}",
                a["a"].as_i64().unwrap_or(0) + a["b"].as_i64().unwrap_or(0)
            )
        });
}

fn client(server: &FakeMcp, token: Option<&str>, can_elicit: bool) -> McpHttp {
    let token = token.map(|t| Arc::new(StaticToken::new(t)) as Arc<dyn AccessToken>);
    McpHttp::new(
        new_http_client(),
        Url::parse(&server.url).unwrap(),
        token,
        can_elicit,
    )
}

/// The text of the answer: a connector passes it in blocks, like MCP.
fn content(answer: &Map<String, Value>) -> String {
    answer["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["text"].as_str().unwrap_or("").to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

async fn brings_all_commands_and_instructions(sse: bool) {
    let server = FakeMcp::start().await;
    tools(&server);
    server.with(|s| {
        s.page_size = 2;
        s.sse = sse;
        s.instructions = Some("Call «time» first.".into());
    });
    let mcp = client(&server, None, false);
    mcp.initialize().await.unwrap();
    let names: Vec<String> = mcp
        .list_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["echo", "time", "sum"]);
    assert_eq!(server.lists(), 2); // two pages
    assert_eq!(mcp.instructions().as_deref(), Some("Call «time» first."));
    assert_eq!(mcp.server_name().as_deref(), Some("fake"));
    assert_eq!(
        content(
            &mcp.call_tool("echo", &args(json!({"text": "hello"})), None)
                .await
                .unwrap()
        ),
        "echo: hello"
    );
    assert_eq!(
        content(
            &mcp.call_tool("sum", &args(json!({"a": 2, "b": 3})), None)
                .await
                .unwrap()
        ),
        "5"
    );
}

#[tokio::test]
async fn a_connector_brings_all_the_server_commands_and_instructions_in_json() {
    brings_all_commands_and_instructions(false).await;
}

#[tokio::test]
async fn a_connector_brings_all_the_server_commands_and_instructions_in_sse() {
    brings_all_commands_and_instructions(true).await;
}

#[tokio::test]
async fn an_expired_session_is_reported_and_another_is_opened() {
    let server = FakeMcp::start().await;
    tools(&server);
    let mcp = client(&server, None, false);
    mcp.initialize().await.unwrap();
    assert_eq!(
        content(&mcp.call_tool("time", &Map::new(), None).await.unwrap()),
        "noon"
    );

    server.expire_session();
    let expired = mcp.call_tool("time", &Map::new(), None).await.unwrap_err();
    assert!(matches!(expired, McpError::SessionLost));
    assert_eq!(expired.to_string(), "session expired");
    mcp.initialize().await.unwrap();
    assert_eq!(
        content(&mcp.call_tool("time", &Map::new(), None).await.unwrap()),
        "noon"
    );
}

#[tokio::test]
async fn a_rejected_token_is_not_retried() {
    let server = FakeMcp::start().await;
    tools(&server);
    server.with(|s| s.token = Some("right".into()));
    let error = client(&server, Some("wrong"), false)
        .initialize()
        .await
        .unwrap_err();
    let McpError::Auth {
        message,
        challenge,
        sign_in,
    } = &error
    else {
        panic!("{error:?}")
    };
    assert_eq!(message, "token rejected (HTTP 401)");
    assert!(challenge.is_none());
    assert!(!sign_in);
    assert_eq!(server.requests(), 1); // a pasted token doesn't refresh: no second attempt

    let mcp = client(&server, Some("right"), false);
    mcp.initialize().await.unwrap();
    assert!(server.with(|s| s.authorizations.contains(&"Bearer right".to_owned())));
    assert_eq!(
        server.with(|s| s.authorizations.last().cloned()).as_deref(),
        Some("Bearer right")
    );
}

#[tokio::test]
async fn a_server_going_down_is_a_transport_failure() {
    let server = FakeMcp::start().await;
    tools(&server);
    let mcp = client(&server, None, false);
    mcp.initialize().await.unwrap();
    server.with(|s| s.down = true);
    let error = mcp.call_tool("time", &Map::new(), None).await.unwrap_err();
    assert!(
        matches!(&error, McpError::Transport(m) if m == "HTTP 503 Service Unavailable"),
        "{error:?}"
    );
    server.with(|s| s.down = false);
    assert_eq!(
        content(&mcp.call_tool("time", &Map::new(), None).await.unwrap()),
        "noon"
    );
}

#[tokio::test]
async fn a_command_the_server_does_not_know_is_an_rpc_error() {
    let server = FakeMcp::start().await;
    tools(&server);
    let mcp = client(&server, None, false);
    mcp.initialize().await.unwrap();
    let error = mcp
        .call_tool("nonexistent", &Map::new(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, McpError::Rpc { code: -32602, message } if message == "unknown tool: nonexistent"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let server = FakeMcp::start().await;
    tools(&server);
    let mcp = McpHttp::new(
        new_http_client(),
        Url::parse(&format!("{}/moved", server.origin)).unwrap(),
        None,
        false,
    );
    let error = mcp.initialize().await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "the server redirects elsewhere (HTTP 307): redirects are not followed"
    );
    assert_eq!(server.requests(), 0);
}

fn empty_trash(server: &FakeMcp) {
    server.add(
        "empty_trash",
        "Empties the trash.",
        Arc::new(|_, ask| {
            Box::pin(async move {
                let schema = json!({"type":"object","properties":{"confirm":{"type":"boolean"},"mode":{"type":"string","enum":["all"]},"note":{"type":"string"}}});
                let answer = ask("Empty the trash? It holds 12 files.".into(), schema).await;
                format!(
                    "{} {}",
                    answer.get("action").and_then(Value::as_str).unwrap_or(""),
                    answer.get("content").map(Value::to_string).unwrap_or_default()
                )
            })
        }),
        None,
    );
}

#[tokio::test]
async fn a_confirmation_asked_by_the_server_reaches_the_client_command_by_command() {
    let server = FakeMcp::start().await;
    tools(&server);
    empty_trash(&server);
    let mcp = client(&server, None, true);
    mcp.initialize().await.unwrap();
    assert!(
        server
            .with(|s| s.client_capabilities.clone())
            .unwrap()
            .get("elicitation")
            .is_some()
    );

    let asked: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let seen = asked.clone();
    let yes: Elicit = Arc::new(move |parameters| {
        seen.lock()
            .unwrap()
            .push(parameters["message"].as_str().unwrap().to_owned());
        let content = fill(parameters.get("requestedSchema").and_then(Value::as_object));
        Box::pin(async move { args(json!({"action": "accept", "content": content})) })
    });
    let answer = mcp
        .call_tool("empty_trash", &Map::new(), Some(&yes))
        .await
        .unwrap();
    assert_eq!(content(&answer), r#"accept {"confirm":true,"mode":"all"}"#);
    assert_eq!(
        *asked.lock().unwrap(),
        ["Empty the trash? It holds 12 files."]
    );

    // The answer isn't remembered: the next time it's asked again, and a no is a no.
    let no: Elicit = Arc::new(|_| Box::pin(async { args(json!({"action": "decline"})) }));
    assert_eq!(
        content(
            &mcp.call_tool("empty_trash", &Map::new(), Some(&no))
                .await
                .unwrap()
        ),
        "decline "
    );

    // With nobody to answer, the client tells the server it can't do it.
    assert_eq!(
        content(
            &mcp.call_tool("empty_trash", &Map::new(), None)
                .await
                .unwrap()
        ),
        "error "
    );
}

#[tokio::test]
async fn without_an_interface_the_client_does_not_promise_confirmations() {
    let server = FakeMcp::start().await;
    tools(&server);
    client(&server, None, false).initialize().await.unwrap();
    let capabilities = server.with(|s| s.client_capabilities.clone()).unwrap();
    assert!(capabilities.get("elicitation").is_none());
}

#[tokio::test]
async fn the_probe_tells_whether_the_server_is_there_and_what_it_offers() {
    let server = FakeMcp::start().await;
    tools(&server);
    server.with(|s| s.instructions = Some("Be kind.".into()));
    let found = probe(&server.url, None).await;
    assert!(found.ok);
    assert_eq!(found.tools, 3);
    assert_eq!(found.server.as_deref(), Some("fake"));
    assert_eq!(
        found.message,
        "Reached (fake): 3 commands, with its instructions."
    );

    server.with(|s| s.token = Some("right".into()));
    assert_eq!(
        probe(&server.url, None).await.message,
        "The server asks for a token."
    );
    assert!(
        probe(&server.url, Some("no"))
            .await
            .message
            .starts_with("The server rejected the token")
    );
    assert!(probe(&server.url, Some("right")).await.ok);

    let url = server.url.clone();
    server.stop();
    drop(server);
    let gone = probe(&url, None).await;
    assert!(!gone.ok);
    assert!(gone.message.starts_with("Not reached"), "{}", gone.message);
    assert_eq!(
        probe("ftp://x.it", None).await.message,
        "The address must start with http:// or https://."
    );
}

#[test]
fn the_automatic_command_comes_from_the_name() {
    let cases = [
        ("Notion (work)", "notion-work"),
        ("Café · Résumé", "cafe-resume"),
        ("list", "app-list"),
        ("!!!", "app"),
        (
            "A name that is truly much longer than needed",
            "a-name-that-is-truly-muc",
        ),
    ];
    for (name, command) in cases {
        assert_eq!(cli_names::from(name), command, "{name}");
    }
}

#[test]
fn a_hand_picked_command_follows_the_same_rules() {
    let cases = [
        (None, true),
        (Some(""), true),
        (Some("notion-work"), true),
        (Some("lab2"), true),
        (Some("Notion"), false),
        (Some("notion--work"), false),
        (Some("-notion"), false),
        (Some("notion work"), false),
        (Some("status"), false),
        (Some("abcdefghijklmnopqrstuvwxy"), false),
    ];
    for (command, ok) in cases {
        assert_eq!(cli_names::problem(command).is_none(), ok, "{command:?}");
    }
}

fn temporary_home() -> PathBuf {
    std::env::temp_dir().join(format!("clak-conn-{}", clak_wire::json::random_hex(6)))
}

#[test]
fn the_registry_keeps_the_token_separate_and_encrypted() {
    let home = temporary_home();
    let store = ConnectorStore::new(&home, fake::test_box(&home));
    let entry = store
        .add(
            "Linear",
            "https://mcp.linear.app/mcp",
            Some("abc-secret"),
            None,
        )
        .unwrap();
    assert!(entry.id.starts_with("cn_"));
    assert_eq!(store.token(&entry.id).as_deref(), Some("abc-secret"));
    let list = std::fs::read_to_string(home.join("connectors.json")).unwrap();
    assert!(!list.contains("abc-secret"));
    assert!(
        !std::fs::read_to_string(home.join("connectors.secrets"))
            .unwrap()
            .contains("abc-secret")
    );

    // The file's shape.
    let saved: Value = serde_json::from_str(&list).unwrap();
    let item = &saved["connectors"][0];
    assert_eq!(item["name"], "Linear");
    assert_eq!(item["enabled"], true);
    assert_eq!(item["auth"], "token");
    assert_eq!(item["command"], Value::Null);
    assert!(item["since"].is_f64());
    assert!(list.contains("\n  \"connectors\": ["));

    store.set_token(&entry.id, None).unwrap();
    assert_eq!(store.token(&entry.id), None);
    assert_eq!(
        ConnectorStore::new(&home, fake::test_box(&home)).all(),
        vec![entry.clone()]
    );
    // A file copied from another user: no token, not an error.
    store.set_token(&entry.id, Some("abc")).unwrap();
    assert_eq!(
        ConnectorStore::new(&home, fake::other_user_box(&home)).token(&entry.id),
        None
    );

    store.remove(&entry.id).unwrap();
    assert!(store.all().is_empty());
    assert_eq!(store.token(&entry.id), None);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_registry_reads_a_hand_edited_file() {
    let home = temporary_home();
    std::fs::create_dir_all(&home).unwrap();
    // With the BOM, an invalid command (ignored), an unknown authentication (token).
    std::fs::write(
        home.join("connectors.json"),
        "\u{FEFF}{\r\n  \"connectors\": [\r\n    {\"id\": \"cn_1\", \"name\": \"Notion\", \"url\": \"https://mcp.notion.com/mcp\", \"enabled\": false, \"since\": 1760000000.25, \"auth\": \"oauth\", \"command\": \"notion-work\"},\r\n    {\"id\": \"cn_2\", \"name\": \"Other\", \"url\": \"https://x.it\", \"auth\": \"bogus\", \"command\": \"Not Valid\"},\r\n    {\"name\": \"no id\", \"url\": \"https://y.it\"}\r\n  ]\r\n}",
    )
    .unwrap();
    let store = ConnectorStore::new(&home, fake::test_box(&home));
    let all = store.all();
    assert_eq!(all.len(), 2);
    assert_eq!(
        all[0],
        ConnectorEntry {
            id: "cn_1".into(),
            name: "Notion".into(),
            url: "https://mcp.notion.com/mcp".into(),
            enabled: false,
            since: 1760000000.25,
            auth: "oauth".into(),
            command: Some("notion-work".into()),
            launch: None,
        }
    );
    assert!(all[0].is_oauth());
    assert!(all[1].enabled);
    assert_eq!(all[1].auth, "token");
    assert_eq!(all[1].command, None);
    assert_eq!(all[1].since, 0.0);
    let _ = std::fs::remove_dir_all(&home);
}
