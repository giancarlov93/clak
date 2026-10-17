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
        .add_fn("echo", "Ripete il testo.", |a| {
            format!(
                "eco: {}",
                a.get("text").and_then(Value::as_str).unwrap_or("")
            )
        })
        .add_fn("ora", "Che ore sono.", |_| "mezzogiorno".into())
        .add_fn("somma", "Somma due numeri.", |a| {
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

async fn porta_tutti_i_comandi_e_le_istruzioni(sse: bool) {
    let server = FakeMcp::start().await;
    tools(&server);
    server.with(|s| {
        s.page_size = 2;
        s.sse = sse;
        s.instructions = Some("Chiama prima «ora».".into());
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
    assert_eq!(names, ["echo", "ora", "somma"]);
    assert_eq!(server.lists(), 2); // two pages
    assert_eq!(mcp.instructions().as_deref(), Some("Chiama prima «ora»."));
    assert_eq!(mcp.server_name().as_deref(), Some("finto"));
    assert_eq!(
        content(
            &mcp.call_tool("echo", &args(json!({"text": "ciao"})), None)
                .await
                .unwrap()
        ),
        "eco: ciao"
    );
    assert_eq!(
        content(
            &mcp.call_tool("somma", &args(json!({"a": 2, "b": 3})), None)
                .await
                .unwrap()
        ),
        "5"
    );
}

#[tokio::test]
async fn un_connettore_porta_tutti_i_comandi_e_le_istruzioni_del_server_in_json() {
    porta_tutti_i_comandi_e_le_istruzioni(false).await;
}

#[tokio::test]
async fn un_connettore_porta_tutti_i_comandi_e_le_istruzioni_del_server_in_sse() {
    porta_tutti_i_comandi_e_le_istruzioni(true).await;
}

#[tokio::test]
async fn una_sessione_scaduta_si_dice_e_se_ne_apre_un_altra() {
    let server = FakeMcp::start().await;
    tools(&server);
    let mcp = client(&server, None, false);
    mcp.initialize().await.unwrap();
    assert_eq!(
        content(&mcp.call_tool("ora", &Map::new(), None).await.unwrap()),
        "mezzogiorno"
    );

    server.expire_session();
    let expired = mcp.call_tool("ora", &Map::new(), None).await.unwrap_err();
    assert!(matches!(expired, McpError::SessionLost));
    assert_eq!(expired.to_string(), "session expired");
    mcp.initialize().await.unwrap();
    assert_eq!(
        content(&mcp.call_tool("ora", &Map::new(), None).await.unwrap()),
        "mezzogiorno"
    );
}

#[tokio::test]
async fn un_token_rifiutato_non_si_riprova() {
    let server = FakeMcp::start().await;
    tools(&server);
    server.with(|s| s.token = Some("giusto".into()));
    let error = client(&server, Some("sbagliato"), false)
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

    let mcp = client(&server, Some("giusto"), false);
    mcp.initialize().await.unwrap();
    assert!(server.with(|s| s.authorizations.contains(&"Bearer giusto".to_owned())));
    assert_eq!(
        server.with(|s| s.authorizations.last().cloned()).as_deref(),
        Some("Bearer giusto")
    );
}

#[tokio::test]
async fn un_server_che_cade_e_un_guasto_di_trasporto() {
    let server = FakeMcp::start().await;
    tools(&server);
    let mcp = client(&server, None, false);
    mcp.initialize().await.unwrap();
    server.with(|s| s.down = true);
    let error = mcp.call_tool("ora", &Map::new(), None).await.unwrap_err();
    assert!(
        matches!(&error, McpError::Transport(m) if m == "HTTP 503 Service Unavailable"),
        "{error:?}"
    );
    server.with(|s| s.down = false);
    assert_eq!(
        content(&mcp.call_tool("ora", &Map::new(), None).await.unwrap()),
        "mezzogiorno"
    );
}

#[tokio::test]
async fn un_comando_che_il_server_non_conosce_e_un_errore_rpc() {
    let server = FakeMcp::start().await;
    tools(&server);
    let mcp = client(&server, None, false);
    mcp.initialize().await.unwrap();
    let error = mcp
        .call_tool("nessuno", &Map::new(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, McpError::Rpc { code: -32602, message } if message == "comando sconosciuto: nessuno"),
        "{error:?}"
    );
}

#[tokio::test]
async fn un_redirect_non_si_segue() {
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

fn svuota(server: &FakeMcp) {
    server.add(
        "svuota",
        "Svuota il cestino.",
        Arc::new(|_, ask| {
            Box::pin(async move {
                let schema = json!({"type":"object","properties":{"conferma":{"type":"boolean"},"modo":{"type":"string","enum":["tutto"]},"nota":{"type":"string"}}});
                let answer = ask("Svuoto il cestino? Sono 12 file.".into(), schema).await;
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
async fn una_conferma_chiesta_dal_server_arriva_al_client_comando_per_comando() {
    let server = FakeMcp::start().await;
    tools(&server);
    svuota(&server);
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
        .call_tool("svuota", &Map::new(), Some(&yes))
        .await
        .unwrap();
    assert_eq!(
        content(&answer),
        r#"accept {"conferma":true,"modo":"tutto"}"#
    );
    assert_eq!(*asked.lock().unwrap(), ["Svuoto il cestino? Sono 12 file."]);

    // The answer isn't remembered: the next time it's asked again, and a no is a no.
    let no: Elicit = Arc::new(|_| Box::pin(async { args(json!({"action": "decline"})) }));
    assert_eq!(
        content(
            &mcp.call_tool("svuota", &Map::new(), Some(&no))
                .await
                .unwrap()
        ),
        "decline "
    );

    // With nobody to answer, the client tells the server it can't do it.
    assert_eq!(
        content(&mcp.call_tool("svuota", &Map::new(), None).await.unwrap()),
        "error "
    );
}

#[tokio::test]
async fn senza_interfaccia_il_client_non_promette_conferme() {
    let server = FakeMcp::start().await;
    tools(&server);
    client(&server, None, false).initialize().await.unwrap();
    let capabilities = server.with(|s| s.client_capabilities.clone()).unwrap();
    assert!(capabilities.get("elicitation").is_none());
}

#[tokio::test]
async fn la_prova_dice_se_il_server_c_e_e_che_cosa_offre() {
    let server = FakeMcp::start().await;
    tools(&server);
    server.with(|s| s.instructions = Some("Sii gentile.".into()));
    let found = probe(&server.url, None).await;
    assert!(found.ok);
    assert_eq!(found.tools, 3);
    assert_eq!(found.server.as_deref(), Some("finto"));
    assert_eq!(
        found.message,
        "Reached (finto): 3 commands, with its instructions."
    );

    server.with(|s| s.token = Some("giusto".into()));
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
    assert!(probe(&server.url, Some("giusto")).await.ok);

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
fn il_comando_automatico_viene_dal_nome() {
    let cases = [
        ("Notion (lavoro)", "notion-lavoro"),
        ("Città · Àgenda", "citta-agenda"),
        ("list", "app-list"),
        ("!!!", "app"),
        (
            "Un nome davvero molto più lungo del dovuto",
            "un-nome-davvero-molto-pi",
        ),
    ];
    for (name, command) in cases {
        assert_eq!(cli_names::from(name), command, "{name}");
    }
}

#[test]
fn un_comando_scelto_a_mano_segue_le_stesse_regole() {
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
fn il_registro_tiene_il_token_a_parte_e_cifrato() {
    let home = temporary_home();
    let store = ConnectorStore::new(&home, fake::test_box(&home));
    let entry = store
        .add(
            "Linear",
            "https://mcp.linear.app/mcp",
            Some("abc-segreto"),
            None,
        )
        .unwrap();
    assert!(entry.id.starts_with("cn_"));
    assert_eq!(store.token(&entry.id).as_deref(), Some("abc-segreto"));
    let list = std::fs::read_to_string(home.join("connectors.json")).unwrap();
    assert!(!list.contains("abc-segreto"));
    assert!(
        !std::fs::read_to_string(home.join("connectors.secrets"))
            .unwrap()
            .contains("abc-segreto")
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
fn il_registro_legge_un_file_ritoccato_a_mano() {
    let home = temporary_home();
    std::fs::create_dir_all(&home).unwrap();
    // With the BOM, an invalid command (ignored), an unknown authentication (token).
    std::fs::write(
        home.join("connectors.json"),
        "\u{FEFF}{\r\n  \"connectors\": [\r\n    {\"id\": \"cn_1\", \"name\": \"Notion\", \"url\": \"https://mcp.notion.com/mcp\", \"enabled\": false, \"since\": 1760000000.25, \"auth\": \"oauth\", \"command\": \"notion-work\"},\r\n    {\"id\": \"cn_2\", \"name\": \"Altro\", \"url\": \"https://x.it\", \"auth\": \"boh\", \"command\": \"Non Valido\"},\r\n    {\"name\": \"senza id\", \"url\": \"https://y.it\"}\r\n  ]\r\n}",
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
