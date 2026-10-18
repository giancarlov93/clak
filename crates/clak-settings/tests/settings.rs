//! The hub settings as a program, `clak settings …`: an agent sees MCP servers and REST
//! APIs and proposes changes, which the user confirms one by one. Secrets don't leak.
//! The port of `SettingsTests.cs`, plus a few more cases.

#[path = "../../clak-connectors/tests/fake/mod.rs"]
mod fake;
#[path = "../../clak-apis/tests/common/mod.rs"]
mod rest_fake;
mod support;

use clak_apis::{ApiAuth, ApiDraft, ApiEndpoint};
use clak_hub::{ConsentKind, cli_names};
use fake::FakeMcp;
use rest_fake::FakeRest;
use serde_json::{Value, json};
use std::time::Duration;
use support::{Question, Setup, TestHub, code, content, is_error, result};

const PETS: &str = r#"
{
  "openapi": "3.0.3",
  "info": { "title": "Pets", "version": "1" },
  "servers": [ { "url": "/v1" } ],
  "security": [ { "key": [] } ],
  "components": { "securitySchemes": { "key": { "type": "apiKey", "in": "header", "name": "X-API-Key" } } },
  "paths": {
    "/pets": { "get": { "operationId": "listPets", "summary": "List the pets." } },
    "/pets/{id}": {
      "get": { "operationId": "getPet", "parameters": [ { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } } ] }
    }
  }
}
"#;

/// The first use of "settings": the user allows it, and the question says what it is.
async fn allow_settings(hub: &TestHub) {
    let using = hub.prompt.next(ConsentKind::Use).await;
    assert_eq!(using.request.title, "claude wants to use Clak's settings");
    assert_eq!(using.what("Service"), Some("Clak itself"));
    using.allow();
}

async fn echo_server() -> FakeMcp {
    let server = FakeMcp::start().await;
    server.add_fn("echo", "Repeats the text.", |a| {
        format!(
            "echo: {}",
            a.get("text").and_then(Value::as_str).unwrap_or("")
        )
    });
    server
}

fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn settings_is_a_reserved_word() {
    assert_eq!(cli_names::from("Settings"), "app-settings");
    assert!(cli_names::problem(Some("settings")).is_some());
    assert!(cli_names::problem(Some("settings-2")).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn no_connector_takes_the_settings_name() {
    let server = echo_server().await;
    let hub = TestHub::start().await;
    assert!(
        hub.connectors
            .add("Fake", &server.url, None, Some("settings"))
            .is_err()
    );
    let entry = hub
        .connectors
        .add("Settings", &server.url, None, None)
        .unwrap();
    let seen = hub.connector(&entry.id, "active").await;
    assert_eq!(seen["cli"], "app-settings");

    hub.prompt.set_allow_all(true);
    let listed = hub.ask("claude", "list", None).await;
    let resources = listed["resources"].as_array().unwrap();
    let settings: Vec<&Value> = resources
        .iter()
        .filter(|r| r["name"] == "settings")
        .collect();
    assert_eq!(settings.len(), 1, "{listed:?}");
    assert_eq!(settings[0]["origin"], "Clak itself");
    let tools = hub
        .ask("claude", "tools", Some(json!({ "resource": "settings" })))
        .await;
    assert!(
        tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "add-api"),
        "{tools:?}"
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_adds_an_mcp_server_and_the_user_confirms_it() {
    let server = echo_server().await;
    server.with(|s| s.token = Some("t0k".into()));
    let hub = TestHub::start().await;

    let (answer, ()) = tokio::join!(
        hub.call_flags(
            "claude",
            "settings",
            "add-server",
            &[("name", "Fake"), ("url", &server.url), ("token", "t0k")]
        ),
        async {
            allow_settings(&hub).await;
            let change = hub.prompt.next(ConsentKind::Change).await;
            assert_eq!(change.request.title, "Add the MCP server “Fake”");
            assert_eq!(
                change.request.question,
                "Allow claude to add this MCP server?"
            );
            assert_eq!(change.what("Address"), Some(server.url.as_str()));
            assert_eq!(change.what("Command"), Some("clak fake"));
            assert_eq!(change.what("Access"), Some("a token, given by the agent"));
            assert_eq!(change.who("Agent"), Some("claude"));
            assert!(change.request.what.iter().all(|l| !l.value.contains("t0k")));
            // On the same machine: no plain-text address warning.
            assert_eq!(change.request.warning, None);
            change.allow();
        }
    );

    let added = result(&answer);
    assert_eq!(added["state"], "active");
    assert_eq!(added["command"], "fake");
    assert_eq!(added["access"], "token");
    assert!(added["next"].as_str().unwrap().starts_with("Ready"));
    assert!(!content(&answer).contains("t0k"));
    // The agent knows what it's waiting for.
    assert!(
        hub.progress_text().contains("to confirm this change"),
        "{}",
        hub.progress_text()
    );
    assert_eq!(
        server.with(|s| s.authorizations.last().cloned()),
        Some("Bearer t0k".to_owned())
    );

    // Reading asks nothing more: permission to use "settings" is enough.
    let servers = result(&hub.call("claude", "settings", "servers", json!({})).await);
    assert_eq!(names(&servers), ["Fake"]);
    assert!(!servers.to_string().contains("t0k"));
    hub.prompt.quiet(100).await;
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_no_leaves_everything_as_it_was() {
    let hub = TestHub::start().await;
    let (answer, ()) = tokio::join!(
        hub.call(
            "claude",
            "settings",
            "add-server",
            json!({ "name": "Other", "url": "https://mcp.example.com/mcp" })
        ),
        async {
            allow_settings(&hub).await;
            hub.prompt.next(ConsentKind::Change).await.deny();
        }
    );
    assert!(is_error(&answer));
    assert!(
        content(&answer).contains("declined"),
        "{}",
        content(&answer)
    );
    assert!(hub.connectors.entries().is_empty());

    // A wrong argument doesn't even bother the user.
    let answer = hub
        .call(
            "claude",
            "settings",
            "add-server",
            json!({ "name": "Other", "url": "ftp://x" }),
        )
        .await;
    assert!(is_error(&answer));
    assert!(content(&answer).contains("http"), "{}", content(&answer));
    hub.prompt.quiet(100).await;
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manual_api_keeps_its_endpoints_and_its_secrets_do_not_leak() {
    let rest = FakeRest::start().await;
    let hub = TestHub::start().await;
    let endpoints = r#"[ { "name": "orders", "method": "get", "path": "/orders", "parameters": [ { "name": "limit", "in": "query", "type": "integer" } ] } ]"#;
    let base = format!("{}/api", rest.origin);
    let (answer, ()) = tokio::join!(
        hub.call_flags(
            "claude",
            "settings",
            "add-api",
            &[
                ("name", "Shop"),
                ("base-url", &base),
                ("auth", "bearer"),
                ("secret", "s3cret"),
                ("endpoints", endpoints),
            ]
        ),
        async {
            allow_settings(&hub).await;
            let change = hub.prompt.next(ConsentKind::Change).await;
            assert_eq!(
                change.what("Access"),
                Some("Bearer token, with a secret given by the agent")
            );
            assert_eq!(change.request.tools.len(), 1);
            assert_eq!(change.request.tools[0].description, "GET /orders");
            assert!(
                change
                    .request
                    .what
                    .iter()
                    .all(|l| !l.value.contains("s3cret"))
            );
            change.allow();
        }
    );
    let added = result(&answer);
    assert_eq!(added["command"], "shop");
    assert_eq!(added["access"], "Bearer token");
    assert!(!content(&answer).contains("s3cret"));

    hub.prompt.set_allow_all(true);
    let called = hub
        .call("claude", "shop", "orders", json!({ "limit": 2 }))
        .await;
    assert!(!is_error(&called), "{}", content(&called));
    assert_eq!(rest.last().headers["authorization"], "Bearer s3cret");
    assert_eq!(rest.last().query, "?limit=2");

    // One more endpoint: the same JSON that show-api returns.
    result(
        &hub.call(
            "claude",
            "settings",
            "set-endpoint",
            json!({
                "api": "shop",
                "endpoint": { "name": "order", "method": "GET", "path": "/orders/{id}", "parameters": [ { "name": "id", "in": "path" } ] },
            }),
        )
        .await,
    );
    let shown = result(
        &hub.call("claude", "settings", "show-api", json!({ "api": "Shop" }))
            .await,
    );
    assert_eq!(names(&shown["endpoints"]), ["orders", "order"]);
    assert_eq!(shown["auth"]["kind"], "bearer");
    assert!(!shown.to_string().contains("s3cret"));
    // The show-api JSON is what set-endpoint accepts: without the nulls.
    assert!(shown["auth"].get("client_id").is_none());
    assert!(shown["endpoints"][0].get("body_required").is_none());
    let called = hub
        .call("claude", "shop", "order", json!({ "id": "7" }))
        .await;
    assert!(!is_error(&called), "{}", content(&called));
    assert_eq!(rest.last().path, "/api/orders/7");

    // A wrong endpoint says why, without asking anything.
    let count = hub.prompt.count();
    let wrong = hub
        .call(
            "claude",
            "settings",
            "set-endpoint",
            json!({ "api": "shop", "endpoint": r#"{"name": "bad", "method": "GET", "path": "/x/{id}"}"# }),
        )
        .await;
    assert!(is_error(&wrong));
    assert!(content(&wrong).contains("{id}"), "{}", content(&wrong));
    assert_eq!(hub.prompt.count(), count);

    // Moved to another host, the secret stays behind.
    let moved = format!("{}/api", rest.origin.replace("127.0.0.1", "localhost"));
    let edited = result(
        &hub.call(
            "claude",
            "settings",
            "edit-api",
            json!({ "api": "shop", "base_url": moved }),
        )
        .await,
    );
    assert_eq!(edited["access"], "Bearer token, no secret saved");
    assert!(
        edited["next"]
            .as_str()
            .unwrap()
            .starts_with("No secret saved")
    );
    let id = hub.apis.entries()[0].id.clone();
    assert!(!hub.apis.has_secret(&id));

    result(
        &hub.call(
            "claude",
            "settings",
            "remove-endpoint",
            json!({ "api": "shop", "name": "orders" }),
        )
        .await,
    );
    let left: Vec<String> = hub.apis.entries()[0]
        .endpoints
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(left, ["order"]);
    let only = hub
        .call(
            "claude",
            "settings",
            "remove-endpoint",
            json!({ "api": "shop", "name": "order" }),
        )
        .await;
    assert!(is_error(&only));
    assert!(
        content(&only).contains("only endpoint"),
        "{}",
        content(&only)
    );

    result(
        &hub.call("claude", "settings", "remove-api", json!({ "api": id }))
            .await,
    );
    assert!(hub.apis.entries().is_empty());
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn from_an_openapi_only_the_chosen_endpoints() {
    let rest = FakeRest::start().await;
    rest.set_document(PETS);
    let hub = TestHub::start().await;
    hub.prompt.set_allow_all(true);
    let document = format!("{}/openapi.json", rest.origin);
    let added = result(
        &hub.call(
            "claude",
            "settings",
            "add-api",
            json!({ "openapi": document, "only": ["listPets"] }),
        )
        .await,
    );
    assert_eq!(added["name"], "Pets");
    assert_eq!(added["base_url"], format!("{}/v1", rest.origin));
    assert_eq!(added["endpoints"], 1);
    assert_eq!(added["access"], "key in header X-API-Key, no secret saved");
    assert_eq!(added["openapi"], document.as_str());

    let unknown = hub
        .call(
            "claude",
            "settings",
            "add-api",
            json!({ "openapi": document, "only": ["nope"] }),
        )
        .await;
    assert!(is_error(&unknown));
    assert!(
        content(&unknown).contains("listPets, getPet"),
        "{}",
        content(&unknown)
    );

    // With OAuth the user signs in: a secret from the agent isn't needed, and isn't taken.
    let oauth = hub
        .call(
            "claude",
            "settings",
            "edit-api",
            json!({
                "api": "pets", "auth": "oauth", "authorization_url": "https://login.example.com/authorize",
                "token_url": "https://login.example.com/token", "client_id": "app-1", "secret": "x",
            }),
        )
        .await;
    assert!(is_error(&oauth));
    assert!(
        content(&oauth).contains("signs in from the panel"),
        "{}",
        content(&oauth)
    );
    let sign_in = result(
        &hub.call(
            "claude",
            "settings",
            "edit-api",
            json!({
                "api": "pets", "auth": "oauth", "authorization_url": "https://login.example.com/authorize",
                "token_url": "https://login.example.com/token", "client_id": "app-1",
            }),
        )
        .await,
    );
    assert_eq!(sign_in["state"], "sign-in required");
    assert!(
        sign_in["next"]
            .as_str()
            .unwrap()
            .contains("Sign in on “Pets”"),
        "{sign_in}"
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_applies_to_what_the_user_saw() {
    let rest = FakeRest::start().await;
    let hub = TestHub::start().await;
    let entry = hub
        .apis
        .add(ApiDraft {
            name: "Shop".into(),
            base_url: rest.origin.clone(),
            auth: ApiAuth::default(),
            endpoints: vec![ApiEndpoint::new("orders", "GET", "/orders", None, vec![])],
            ..ApiDraft::default()
        })
        .unwrap();

    let (answer, ()) = tokio::join!(
        hub.call(
            "claude",
            "settings",
            "edit-api",
            json!({ "api": "shop", "name": "Store" })
        ),
        async {
            allow_settings(&hub).await;
            let change: Question = hub.prompt.next(ConsentKind::Change).await;
            assert_eq!(change.what("Name"), Some("Shop → Store"));
            // While the user decides, the panel changes it: the agent's change doesn't overwrite it.
            hub.apis
                .edit(
                    &entry.id,
                    ApiDraft {
                        name: "Workshop".into(),
                        base_url: rest.origin.clone(),
                        endpoints: entry.endpoints.clone(),
                        ..ApiDraft::default()
                    },
                )
                .await
                .unwrap();
            change.allow();
        }
    );
    assert!(is_error(&answer));
    assert!(
        content(&answer).contains("changed while the user was deciding"),
        "{}",
        content(&answer)
    );
    assert_eq!(hub.apis.get(&entry.id).unwrap().name, "Workshop");

    // Turned off and on again, by the agent.
    hub.prompt.set_allow_all(true);
    let off = result(
        &hub.call(
            "claude",
            "settings",
            "edit-api",
            json!({ "api": "workshop", "enabled": false }),
        )
        .await,
    );
    assert_eq!(off["enabled"], false);
    assert!(off["next"].as_str().unwrap().starts_with("Turned off"));
    assert!(!hub.apis.get(&entry.id).unwrap().enabled);
    let nothing = hub
        .call(
            "claude",
            "settings",
            "edit-api",
            json!({ "api": "workshop", "enabled": false }),
        )
        .await;
    assert!(
        content(&nothing).contains("Nothing to change"),
        "{}",
        content(&nothing)
    );
    let on = result(
        &hub.call(
            "claude",
            "settings",
            "edit-api",
            json!({ "api": "workshop", "enabled": true }),
        )
        .await,
    );
    assert_eq!(on["enabled"], true);
    assert!(hub.apis.get(&entry.id).unwrap().enabled);
    hub.stop().await;
}

// =========================================================================
// Beyond the Windows version
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn a_server_is_edited_and_the_token_does_not_follow_another_host() {
    let server = echo_server().await;
    server.with(|s| s.token = Some("t0k".into()));
    let hub = TestHub::start().await;
    let entry = hub
        .connectors
        .add("Fake", &server.url, Some("t0k"), None)
        .unwrap();
    hub.connector(&entry.id, "active").await;

    // On another host (localhost instead of 127.0.0.1) the token stays behind, and the question says so.
    let moved = server.url.replace("127.0.0.1", "localhost");
    let (answer, ()) = tokio::join!(
        hub.call(
            "claude",
            "settings",
            "edit-server",
            json!({ "server": "fake", "url": moved, "command": "other" })
        ),
        async {
            allow_settings(&hub).await;
            let change = hub.prompt.next(ConsentKind::Change).await;
            assert_eq!(change.request.title, "Change the MCP server “Fake”");
            assert_eq!(change.request.question, "Allow claude to make this change?");
            assert_eq!(change.what("Server"), Some("Fake (clak fake)"));
            assert_eq!(change.what("Command"), Some("clak fake → clak other"));
            assert_eq!(
                change.what("Access"),
                Some("the saved token or sign-in stays behind: it isn't sent to the new host")
            );
            change.allow();
        }
    );
    let edited = result(&answer);
    assert_eq!(edited["command"], "other");
    assert_eq!(edited["access"], "none");
    assert_eq!(edited["state"], "token rejected");
    assert!(
        edited["next"]
            .as_str()
            .unwrap()
            .contains("edit-server --token"),
        "{edited}"
    );
    assert!(!hub.connectors.has_credential(&entry.id));

    // The right token, from the agent: active again.
    hub.prompt.set_allow_all(true);
    let fixed = result(
        &hub.call(
            "claude",
            "settings",
            "edit-server",
            json!({ "server": "other", "token": "t0k" }),
        )
        .await,
    );
    assert_eq!(fixed["state"], "active");
    assert_eq!(fixed["access"], "token");

    // No change: it says so without asking.
    let count = hub.prompt.count();
    let same = hub
        .call(
            "claude",
            "settings",
            "edit-server",
            json!({ "server": entry.id, "name": "Fake" }),
        )
        .await;
    assert!(
        content(&same).contains("Nothing to change"),
        "{}",
        content(&same)
    );
    let both = hub
        .call(
            "claude",
            "settings",
            "edit-server",
            json!({ "server": "other", "token": "x", "remove_token": true }),
        )
        .await;
    assert!(content(&both).contains("choose one"), "{}", content(&both));
    assert_eq!(hub.prompt.count(), count);

    let removed = result(
        &hub.call(
            "claude",
            "settings",
            "remove-server",
            json!({ "server": "Fake" }),
        )
        .await,
    );
    assert_eq!(removed["removed"], "Fake");
    assert!(hub.connectors.entries().is_empty());
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_arguments_and_names_that_do_not_exist() {
    let hub = TestHub::start().await;
    hub.prompt.set_allow_all(true);
    let missing = hub
        .call(
            "claude",
            "settings",
            "show-api",
            json!({ "api": "missing" }),
        )
        .await;
    assert!(is_error(&missing));
    assert_eq!(
        content(&missing),
        "No REST API is called “missing”: `clak settings apis` lists them."
    );
    let typed = hub
        .call(
            "claude",
            "settings",
            "edit-server",
            json!({ "server": "x", "name": 3 }),
        )
        .await;
    // The server is looked up first: it doesn't exist.
    assert!(
        content(&typed).contains("No MCP server is called “x”"),
        "{}",
        content(&typed)
    );
    let bad_bool = hub
        .call(
            "claude",
            "settings",
            "add-server",
            json!({ "name": 3, "url": "https://x" }),
        )
        .await;
    assert_eq!(content(&bad_bool), "--name must be text.");
    let no_base = hub
        .call(
            "claude",
            "settings",
            "add-api",
            json!({ "name": "X", "endpoints": [{ "name": "a", "method": "GET", "path": "/a" }] }),
        )
        .await;
    assert_eq!(content(&no_base), "--base-url is required.");
    let nothing = hub.call("claude", "settings", "add-api", json!({})).await;
    assert!(
        content(&nothing).starts_with("Pass --openapi"),
        "{}",
        content(&nothing)
    );
    let only = hub
        .call(
            "claude",
            "settings",
            "add-api",
            json!({ "only": ["a"], "endpoints": [] }),
        )
        .await;
    assert_eq!(content(&only), "--only goes with --openapi.");
    let auth = hub
        .call(
            "claude",
            "settings",
            "add-api",
            json!({ "name": "X", "base_url": "https://x.example.com", "auth": "magic",
                    "endpoints": [{ "name": "a", "method": "GET", "path": "/a" }] }),
        )
        .await;
    assert_eq!(
        content(&auth),
        "--auth is one of: none, bearer, header, query, basic, oauth, oauth-app."
    );
    let nowhere = hub
        .call(
            "claude",
            "settings",
            "add-api",
            json!({ "name": "X", "base_url": "https://x.example.com", "secret": "s",
                    "endpoints": [{ "name": "a", "method": "GET", "path": "/a" }] }),
        )
        .await;
    assert!(
        content(&nowhere).contains("nowhere to go"),
        "{}",
        content(&nowhere)
    );
    let unknown = hub.call("claude", "settings", "fly", json!({})).await;
    // A command that doesn't exist is stopped by the hub already, before the program.
    assert!(code(&unknown).is_some() || is_error(&unknown));
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_interface_no_change() {
    let hub = TestHub::start_with(Setup { no_prompt: true }).await;
    let answer = hub
        .call(
            "claude",
            "settings",
            "add-server",
            json!({ "name": "Other", "url": "https://mcp.example.com/mcp" }),
        )
        .await;
    // With nobody to ask, the core doesn't even allow using "settings".
    assert!(code(&answer).is_some() || is_error(&answer), "{answer:?}");
    assert!(hub.connectors.entries().is_empty());
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_command_changes_nothing() {
    let hub = TestHub::start().await;
    let call = hub.call(
        "claude",
        "settings",
        "add-server",
        json!({ "name": "Other", "url": "https://mcp.example.com/mcp" }),
    );
    // The agent leaves while the user decides: the question closes by itself.
    let asked = async {
        allow_settings(&hub).await;
        let change = hub.prompt.next(ConsentKind::Change).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        change
    };
    let change = tokio::select! {
        _ = call => panic!("the command shouldn't have finished"),
        change = asked => change,
    };
    // The command (the connection) dropped: the hub cancels the call, and a late yes doesn't count.
    tokio::time::sleep(Duration::from_millis(300)).await;
    change.allow();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(hub.connectors.entries().is_empty());
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopped_settings_disappear() {
    let hub = TestHub::start().await;
    assert!(hub.settings.conn_id().is_some());
    hub.settings.stop("hub closed").await;
    assert!(hub.settings.conn_id().is_none());
    assert!(
        hub.hub()
            .broker()
            .run(|c| c.app(cli_names::SETTINGS).is_none())
    );
    hub.stop().await;
}

/// A program that surely exists: this very test executable, which with `--list` lists the
/// tests and exits (it isn't an MCP server, but the confirmation doesn't know that).
fn this_program() -> String {
    format!("\"{}\" --list", std::env::current_exe().unwrap().display())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_adds_a_stdio_program_and_the_variables_do_not_leak() {
    let secret = "top-secret-key-4711";
    let hub = TestHub::start().await;
    let line = this_program();
    let env = format!(r#"{{"API_KEY": "{secret}", "DEBUG": 1}}"#);
    let (answer, ()) = tokio::join!(
        hub.call_flags(
            "claude",
            "settings",
            "add-server",
            &[("name", "Browser"), ("stdio", &line), ("env", &env)]
        ),
        async {
            allow_settings(&hub).await;
            let change = hub.prompt.next(ConsentKind::Change).await;
            assert_eq!(change.request.title, "Add the MCP server “Browser”");
            let exe = std::env::current_exe().unwrap();
            assert!(change.what("Program").unwrap().ends_with("--list"));
            assert_eq!(
                change.what("File"),
                Some(exe.display().to_string().as_str())
            );
            assert_eq!(
                change.what("Environment"),
                Some("API_KEY, DEBUG (values given by the agent, saved encrypted)")
            );
            assert_eq!(change.what("Command"), Some("clak browser"));
            assert!(
                change
                    .request
                    .warning
                    .as_deref()
                    .unwrap()
                    .contains("every time it starts")
            );
            assert!(
                change
                    .request
                    .what
                    .iter()
                    .all(|l| !l.value.contains(secret))
            );
            change.allow();
        }
    );
    let added = result(&answer);
    assert!(
        added["program"].as_str().unwrap().ends_with("--list"),
        "{added}"
    );
    assert!(added.get("url").is_none());
    assert_eq!(added["access"], "environment variables API_KEY, DEBUG");
    assert!(!content(&answer).contains(secret));
    let id = added["id"].as_str().unwrap().to_owned();
    assert_eq!(hub.connectors.env(&id)["DEBUG"], "1");

    // The variables change all together; an address or a token doesn't apply.
    let (answer, ()) = tokio::join!(
        hub.call(
            "claude",
            "settings",
            "edit-server",
            json!({ "server": "browser", "env": { "API_KEY": "new" } })
        ),
        async {
            let change = hub.prompt.next(ConsentKind::Change).await;
            assert_eq!(
                change.what("Environment"),
                Some(
                    "API_KEY (values given by the agent, saved encrypted) (replacing the saved ones)"
                )
            );
            assert_eq!(change.request.warning, None);
            change.allow();
        }
    );
    assert!(!is_error(&answer), "{}", content(&answer));
    assert_eq!(hub.connectors.env(&id).len(), 1);
    for wrong in [
        json!({ "server": "browser", "url": "https://x.example.com/mcp" }),
        json!({ "server": "browser", "token": "t" }),
    ] {
        let answer = hub.call("claude", "settings", "edit-server", wrong).await;
        assert!(
            content(&answer).contains("runs a program"),
            "{}",
            content(&answer)
        );
    }

    let (answer, ()) = tokio::join!(
        hub.call(
            "claude",
            "settings",
            "remove-server",
            json!({ "server": "browser" })
        ),
        async {
            let change = hub.prompt.next(ConsentKind::Change).await;
            assert!(change.what("Program").unwrap().ends_with("--list"));
            assert_eq!(
                change.what("Environment"),
                Some("its saved variables are deleted")
            );
            change.allow();
        }
    );
    assert!(!is_error(&answer), "{}", content(&answer));
    assert!(hub.connectors.env(&id).is_empty());
    hub.prompt.quiet(100).await;
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_program_does_not_bother_the_user() {
    let hub = TestHub::start().await;
    hub.prompt.set_allow_all(true);
    let cases = [
        (json!({ "name": "B" }), "--url or --stdio is required"),
        (
            json!({ "name": "B", "stdio": "x", "url": "https://x.example.com" }),
            "takes no --url",
        ),
        (
            json!({ "name": "B", "url": "https://x.example.com", "env": { "A": "1" } }),
            "--env is for a program",
        ),
        (json!({ "name": "B", "stdio": "\"unclosed" }), "quote"),
        (
            json!({ "name": "B", "stdio": "clak-program-that-does-not-exist" }),
            "was not found on this computer",
        ),
        (
            json!({ "name": "B", "stdio": "x", "env": ["A"] }),
            "--env is a JSON object",
        ),
        (
            json!({ "name": "B", "stdio": "x", "env": { "NO-PE": "1" } }),
            "not a valid environment variable name",
        ),
    ];
    for (args, expected) in cases {
        let answer = hub
            .call("claude", "settings", "add-server", args.clone())
            .await;
        assert!(is_error(&answer), "{args}");
        assert!(
            content(&answer).contains(expected),
            "{args}: {}",
            content(&answer)
        );
    }
    assert!(hub.connectors.entries().is_empty());

    // A server at an address doesn't become a program.
    let entry = hub
        .connectors
        .add("Web", "https://x.example.com/mcp", None, None)
        .unwrap();
    let answer = hub
        .call(
            "claude",
            "settings",
            "edit-server",
            json!({ "server": entry.id, "stdio": "x" }),
        )
        .await;
    assert!(
        content(&answer).contains("remove it and add it again"),
        "{}",
        content(&answer)
    );
    hub.stop().await;
}
