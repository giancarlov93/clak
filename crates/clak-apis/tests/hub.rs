//! The APIs inside the real hub: an OpenAPI (or the user, by hand) gives the endpoints, and
//! to agents the API looks like a connected program, one command per endpoint, with the
//! same permissions.

mod common;
mod hubtest;

use clak_apis::model::{
    ApiAuth, ApiDraft, ApiEndpoint, ApiParameter, AuthKind, BodyKind, ParamIn, ParamType,
};
use clak_connectors::OAuthClient;
use clak_hub::ConsentKind;
use common::{CLIENT_ID, CLIENT_SECRET, FakeRest, Reply};
use hubtest::{Setup, TestHub, browser, code, is_error, text};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Duration;

fn draft(
    name: &str,
    base_url: &str,
    auth: ApiAuth,
    secret: Option<&str>,
    endpoints: Vec<ApiEndpoint>,
) -> ApiDraft {
    ApiDraft {
        name: name.into(),
        base_url: base_url.into(),
        auth,
        secret: secret.map(str::to_owned),
        endpoints,
        ..ApiDraft::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn un_api_importata_risponde_agli_agenti_con_il_suo_accesso() {
    let rest = FakeRest::start().await;
    rest.set_document(common::PETSTORE);
    let hub = TestHub::start().await;
    let import = hub
        .apis
        .import(&format!("{}/openapi.json", rest.origin))
        .await
        .unwrap();
    assert_eq!(import.base_url, Some(format!("{}/v1", rest.origin)));

    let mut new = draft(
        &import.name,
        import.base_url.as_deref().unwrap(),
        import.auth.clone(),
        Some("k123"),
        import.endpoints.clone(),
    );
    new.source = Some(format!("{}/openapi.json", rest.origin));
    let entry = hub.apis.add(new).unwrap();
    let seen = hub.api(&entry.id, |a| a["cli"] == "petstore").await;
    assert_eq!(seen["has_secret"], true);
    assert_eq!(seen["state"], "active");
    assert_eq!(seen["auth_label"], "key in header X-API-Key");
    assert_eq!(seen["endpoints"], 5);
    assert!(
        !std::fs::read_to_string(hub.home.join("apis.json"))
            .unwrap()
            .contains("k123")
    );
    assert_eq!(
        hub.secrets.all().get(&entry.id).map(String::as_str),
        Some("k123")
    );
    assert!(
        !clak_wire::json::text(&serde_json::Value::Object(hub.snapshot().await)).contains("k123")
    );
    // For agents it's a program, with its commands.
    let resource = hub.snapshot().await["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "petstore")
        .cloned()
        .unwrap();
    assert_eq!(resource["origin"]["protocol"], "rest");
    assert_eq!(resource["tools"].as_array().unwrap().len(), 5);
    assert!(
        hub.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("API added: Petstore ("))
    );

    // The first use asks for permission, and the service is described for what it is.
    let call = {
        let hub = &hub;
        async move {
            hub.call(
                "claude",
                "petstore",
                "listPets",
                json!({"limit": 5, "tags": ["a", "b c"]}),
            )
            .await
        }
    };
    let question = async {
        let question = hub.prompt.next(ConsentKind::Use).await;
        assert!(
            question.request.what.iter().any(|l| l.label == "Service"
                && l.value
                    .starts_with(&format!("REST API, {}/v1", rest.origin))),
            "{:?}",
            question.request.what
        );
        question.allow();
    };
    let (answer, ()) = tokio::join!(call, question);
    assert!(!is_error(&answer), "{answer:?}");
    assert_eq!(text(&answer), "GET /v1/pets?limit=5&tags=a&tags=b%20c");
    assert_eq!(rest.last().headers["x-api-key"], "k123");

    hub.prompt.set_allow_all(true);
    let answer = hub
        .call(
            "claude",
            "petstore",
            "create_pet",
            json!({"name": "Rex", "tag": "dog"}),
        )
        .await;
    assert!(!is_error(&answer));
    assert_eq!(rest.last().method, "POST");
    assert_eq!(rest.last().body, r#"{"name":"Rex","tag":"dog"}"#);

    hub.call("claude", "petstore", "getPet", json!({"petId": "a/b c"}))
        .await;
    assert_eq!(rest.last().path, "/v1/pets/a%2Fb%20c");

    hub.call(
        "claude",
        "petstore",
        "setNotes",
        json!({"petId": "7", "body": r#"["uno","due"]"#}),
    )
    .await;
    assert_eq!(rest.last().body, r#"["uno","due"]"#);

    // A missing required argument isn't even sent.
    let before = rest.requests().len();
    let answer = hub
        .call("claude", "petstore", "create_pet", json!({"tag": "cat"}))
        .await;
    assert!(is_error(&answer));
    assert!(text(&answer).contains("--name"));
    assert_eq!(rest.requests().len(), before);

    // A service error is an error, with its beginning.
    rest.respond(|_| Reply::new(404, r#"{"error":"no such pet"}"#));
    let answer = hub
        .call("claude", "petstore", "getPet", json!({"petId": "9"}))
        .await;
    assert!(is_error(&answer));
    assert!(text(&answer).starts_with("HTTP 404"));
    assert!(text(&answer).contains("no such pet"));
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn un_api_a_mano_con_bearer_basic_e_un_modulo() {
    let rest = FakeRest::start().await;
    let hub = TestHub::start().await;
    hub.prompt.set_allow_all(true);
    let endpoints = vec![
        ApiEndpoint::new(
            "search",
            "GET",
            "/search",
            Some("Search."),
            vec![ApiParameter::new("q", ParamIn::Query).required()],
        ),
        ApiEndpoint::new(
            "send",
            "POST",
            "/forms/{kind}",
            None,
            vec![
                ApiParameter::new("kind", ParamIn::Path),
                ApiParameter::new("text", ParamIn::Body),
                ApiParameter::new("urgent", ParamIn::Body).of_type(ParamType::Boolean),
            ],
        )
        .with_body(BodyKind::Form),
    ];
    let mut new = draft(
        "My Service",
        &format!("{}/", rest.origin),
        ApiAuth::new(AuthKind::Bearer),
        Some("tok"),
        endpoints.clone(),
    );
    new.command = Some("svc".into());
    let entry = hub.apis.add(new).unwrap();
    assert_eq!(entry.base_url, rest.origin);
    hub.api(&entry.id, |a| a["cli"] == "svc").await;

    hub.call("claude", "svc", "search", json!({"q": "è così"}))
        .await;
    assert_eq!(rest.last().query, "?q=%C3%A8%20cos%C3%AC");
    assert_eq!(rest.last().headers["authorization"], "Bearer tok");

    hub.call(
        "claude",
        "svc",
        "send",
        json!({"kind": "note", "text": "a&b", "urgent": true}),
    )
    .await;
    assert_eq!(rest.last().path, "/forms/note");
    assert_eq!(rest.last().body, "text=a%26b&urgent=true");

    // Basic: user in plain text, password encrypted. The new access makes permissions start over.
    hub.prompt.set_allow_all(false);
    let mut basic = draft(
        "My Service",
        &rest.origin,
        ApiAuth::named(AuthKind::Basic, "ada"),
        Some("pw"),
        endpoints,
    );
    basic.command = Some("svc".into());
    hub.apis.edit(&entry.id, basic).await.unwrap();
    hub.api(&entry.id, |a| a["cli"] == "svc" && a["auth"] == "basic")
        .await;
    let call = hub.call("claude", "svc", "search", json!({"q": "x"}));
    let allow = async { hub.prompt.next(ConsentKind::Use).await.allow() };
    tokio::join!(call, allow);
    assert_eq!(rest.last().headers["authorization"], "Basic YWRhOnB3");
    assert!(
        hub.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "API My Service: Basic, user ada, secret changed")
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rinominare_un_api_tiene_i_permessi_e_un_altro_host_non_riceve_il_segreto() {
    let rest = FakeRest::start().await;
    let other = FakeRest::start().await;
    let hub = TestHub::start().await;
    let mut endpoints = vec![ApiEndpoint::new("ping", "GET", "/ping", None, vec![])];
    let auth = ApiAuth::named(AuthKind::Header, "X-Key");
    let entry = hub
        .apis
        .add(draft(
            "Ping",
            &rest.origin,
            auth.clone(),
            Some("s3cret"),
            endpoints.clone(),
        ))
        .unwrap();
    hub.api(&entry.id, |a| a["cli"] == "ping").await;
    hub.prompt.set_allow_all(true);
    hub.call("claude", "ping", "ping", json!({})).await;
    hub.prompt.set_allow_all(false);
    let asked = hub.prompt.count();

    // Name, command and one more endpoint: same permissions; the new command is asked separately.
    endpoints.push(ApiEndpoint::new("pong", "GET", "/pong", None, vec![]));
    let mut renamed = draft(
        "Ping Pong",
        &rest.origin,
        auth.clone(),
        None,
        endpoints.clone(),
    );
    renamed.command = Some("pp".into());
    hub.apis.edit(&entry.id, renamed.clone()).await.unwrap();
    hub.api(&entry.id, |a| a["cli"] == "pp").await;
    assert!(!is_error(
        &hub.call("claude", "pp", "ping", json!({})).await
    ));
    assert_eq!(hub.prompt.count(), asked);
    assert_eq!(rest.last().headers["x-key"], "s3cret");
    assert!(
        hub.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "API Ping: renamed “Ping Pong”, command “pp”, 2 endpoints")
    );
    let call = hub.call("claude", "pp", "pong", json!({}));
    let allow = async { hub.prompt.next(ConsentKind::Tool).await.allow() };
    tokio::join!(call, allow);

    // A redirect to another host isn't followed: the secret would go along with it.
    let target = format!("{}/steal", other.origin);
    rest.respond(move |_| Reply::redirect(302, &target));
    let answer = hub.call("claude", "pp", "ping", json!({})).await;
    assert!(is_error(&answer));
    assert!(text(&answer).contains("another host"));
    assert!(other.requests().is_empty());

    // On the same origin it is.
    rest.respond(|seen| {
        if seen.path == "/ping" {
            Reply::redirect(301, "/moved")
        } else {
            Reply::new(200, "here")
        }
    });
    assert_eq!(
        text(&hub.call("claude", "pp", "ping", json!({})).await),
        "here"
    );

    // Another URL: the secret stays behind, and permissions start over.
    let mut moved = renamed.clone();
    moved.base_url = other.origin.clone();
    hub.apis.edit(&entry.id, moved).await.unwrap();
    hub.api(&entry.id, |a| {
        a["base_url"] == other.origin.as_str() && a["cli"] == "pp"
    })
    .await;
    assert!(!hub.apis.has_secret(&entry.id));
    let call = hub.call("claude", "pp", "ping", json!({}));
    let allow = async { hub.prompt.next(ConsentKind::Use).await.allow() };
    tokio::join!(call, allow);
    assert!(!other.last().headers.contains_key("x-key"));

    hub.apis.set_enabled(&entry.id, false).await.unwrap();
    let off = hub.api(&entry.id, |a| a["cli"].is_null()).await;
    assert_eq!(off["state"], "off");
    assert_eq!(
        code(&hub.call("claude", "pp", "ping", json!({})).await),
        Some("not_found")
    );
    hub.apis.remove(&entry.id).await.unwrap();
    assert!(hub.snapshot().await["apis"].as_array().unwrap().is_empty());
    assert!(hub.apis.entries().is_empty());
    assert!(
        hub.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "API removed: Ping Pong")
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn un_comando_abbandonato_si_ferma_anche_nell_api() {
    let rest = FakeRest::start().await;
    let hub = TestHub::start_with(Setup {
        call_timeout: Some(Duration::from_millis(500)),
    })
    .await;
    hub.prompt.set_allow_all(true);
    rest.respond(|_| Reply::new(200, "late").after(Duration::from_secs(5)));
    let entry = hub
        .apis
        .add(draft(
            "Slow",
            &rest.origin,
            ApiAuth::default(),
            None,
            vec![ApiEndpoint::new("wait", "GET", "/wait", None, vec![])],
        ))
        .unwrap();
    hub.api(&entry.id, |a| a["cli"] == "slow").await;

    let call = hub.call("claude", "slow", "wait", json!({}));
    let watch = async {
        for _ in 0..40 {
            if hub.apis.calls_in_flight(&entry.id) == 1 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    };
    let (answer, seen) = tokio::join!(call, watch);
    assert!(seen, "the command never started");
    assert_eq!(code(&answer), Some("app_timeout"));
    // The hub sent `cancel`: the HTTP request no longer waits.
    for _ in 0..50 {
        if hub.apis.calls_in_flight(&entry.id) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(hub.apis.calls_in_flight(&entry.id), 0);
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn le_api_ripartono_con_l_hub_e_un_api_sbagliata_non_si_salva() {
    let rest = FakeRest::start().await;
    let hub = TestHub::start().await;
    let ping = vec![ApiEndpoint::new("ping", "GET", "/ping", None, vec![])];
    let on = hub
        .apis
        .add(draft(
            "On",
            &rest.origin,
            ApiAuth::default(),
            None,
            ping.clone(),
        ))
        .unwrap();
    let off = hub
        .apis
        .add(draft(
            "Off",
            &rest.origin,
            ApiAuth::default(),
            None,
            ping.clone(),
        ))
        .unwrap();
    hub.apis.set_enabled(&off.id, false).await.unwrap();
    hub.apis.stop().await;
    hub.api(&on.id, |a| a["cli"].is_null()).await;

    let again = hub.reopen();
    again.start_all();
    let seen = hub.api(&on.id, |a| a["cli"] == "on").await;
    assert_eq!(seen["state"], "active");
    assert_eq!(hub.api(&off.id, |_| true).await["state"], "off");
    again.stop().await;

    assert_eq!(
        hub.apis
            .add(draft(
                "",
                &rest.origin,
                ApiAuth::default(),
                None,
                ping.clone()
            ))
            .unwrap_err(),
        "A name is required."
    );
    assert_eq!(
        hub.apis
            .add(draft(
                "X",
                "ftp://x",
                ApiAuth::default(),
                None,
                ping.clone()
            ))
            .unwrap_err(),
        "The base URL must start with http:// or https://."
    );
    assert_eq!(
        hub.apis
            .edit(
                "api_none",
                draft("X", &rest.origin, ApiAuth::default(), None, ping)
            )
            .await
            .unwrap_err(),
        "This API no longer exists."
    );
    hub.stop().await;
}

// --- OAuth --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn un_api_con_oauth_si_aggiunge_con_l_accesso_nel_browser_e_si_rinnova_da_sola() {
    let rest = FakeRest::start().await;
    rest.require_token();
    let hub = TestHub::start().await;
    hub.apis.set_open_browser(browser());
    hub.apis.set_sign_in_timeout(Duration::from_secs(10));
    let auth = ApiAuth::oauth(
        AuthKind::OAuth,
        Some(&format!("{}/oauth/authorize", rest.origin)),
        Some(&format!("{}/oauth/token", rest.origin)),
        Some("read"),
        Some(CLIENT_ID),
    );
    let me = vec![ApiEndpoint::new("me", "GET", "/me", None, vec![])];

    let grant = hub.apis.sign_in(&auth, None, None, None).await.unwrap();
    {
        let last = rest.state.last_authorize.lock().unwrap();
        assert_eq!(last["scope"], "read");
        assert_eq!(last["code_challenge_method"], "S256");
        assert!(!last.contains_key("resource")); // not sent outside MCP
    }
    let mut new = draft("Hub", &rest.origin, auth.clone(), None, me.clone());
    new.grant = Some(grant.clone());
    let entry = hub.apis.add(new).unwrap();
    let seen = hub.api(&entry.id, |a| a["cli"] == "hub").await;
    assert_eq!(seen["state"], "active");
    assert_eq!(seen["signin_host"], "127.0.0.1");
    assert!(
        !clak_wire::json::text(&serde_json::Value::Object(hub.snapshot().await))
            .contains(&grant.access_token)
    );

    hub.prompt.set_allow_all(true);
    assert_eq!(
        text(&hub.call("claude", "hub", "me", json!({})).await),
        "GET /me"
    );
    assert_eq!(
        rest.last().headers["authorization"],
        format!("Bearer {}", grant.access_token)
    );

    // The token expires earlier than expected: a 401, a renewal, the same request again.
    rest.expire_tokens();
    assert_eq!(
        text(&hub.call("claude", "hub", "me", json!({})).await),
        "GET /me"
    );
    assert_eq!(rest.state.refreshes.load(Ordering::SeqCst), 1);
    assert_ne!(
        rest.last().headers["authorization"],
        format!("Bearer {}", grant.access_token)
    );
    // The renewal was saved.
    assert!(!hub.secrets.all()[&entry.id].contains(&format!("\"{}\"", grant.access_token)));

    // The service no longer renews: the agent learns the user is needed, and the panel shows it.
    rest.state.refuse_refresh.store(true, Ordering::SeqCst);
    rest.expire_tokens();
    let answer = hub.call("claude", "hub", "me", json!({})).await;
    assert!(is_error(&answer));
    assert!(text(&answer).contains("needs the user to sign in"));
    hub.api(&entry.id, |a| a["state"] == "sign-in required")
        .await;
    assert!(
        hub.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "Hub: sign-in required (the service no longer renews the access)")
    );

    // Signed in again, with the same client: active again.
    rest.state.refuse_refresh.store(false, Ordering::SeqCst);
    let again = hub
        .apis
        .sign_in(&auth, None, Some(&entry.id), None)
        .await
        .unwrap();
    let mut renewed = draft("Hub", &rest.origin, auth, None, me);
    renewed.grant = Some(again);
    hub.apis.edit(&entry.id, renewed).await.unwrap();
    hub.api(&entry.id, |a| a["state"] == "active" && !a["cli"].is_null())
        .await;
    assert!(!is_error(&hub.call("claude", "hub", "me", json!({})).await));
    assert!(
        hub.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "API Hub: signed in at 127.0.0.1")
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn un_api_solo_app_prende_il_token_con_il_segreto_del_client() {
    let rest = FakeRest::start().await;
    rest.require_token();
    let hub = TestHub::start().await;
    let auth = ApiAuth::oauth(
        AuthKind::OAuthApp,
        None,
        Some(&format!("{}/oauth/token", rest.origin)),
        None,
        Some(CLIENT_ID),
    );
    assert!(hub.apis.sign_in(&auth, None, None, None).await.is_err()); // not without a secret
    assert_eq!(
        hub.apis
            .sign_in(&ApiAuth::new(AuthKind::Bearer), None, None, None)
            .await
            .unwrap_err()
            .message,
        "This API doesn't use OAuth."
    );

    let lines = std::sync::Mutex::new(Vec::new());
    let status = |line: &str| lines.lock().unwrap().push(line.to_owned());
    let grant = hub
        .apis
        .sign_in(
            &auth,
            Some(OAuthClient::new(CLIENT_ID, Some(CLIENT_SECRET.into()))),
            None,
            Some(&status),
        )
        .await
        .unwrap();
    assert_eq!(
        lines.lock().unwrap().as_slice(),
        ["Asking 127.0.0.1 for a token…"]
    );
    let mut new = draft(
        "Daemon",
        &rest.origin,
        auth,
        None,
        vec![ApiEndpoint::new("me", "GET", "/me", None, vec![])],
    );
    new.grant = Some(grant);
    let entry = hub.apis.add(new).unwrap();
    hub.api(&entry.id, |a| a["cli"] == "daemon").await;
    hub.prompt.set_allow_all(true);
    assert!(!is_error(
        &hub.call("claude", "daemon", "me", json!({})).await
    ));

    // Expired: a new token, requested again with the secret, with nobody in the browser.
    rest.expire_tokens();
    assert!(!is_error(
        &hub.call("claude", "daemon", "me", json!({})).await
    ));
    assert_eq!(rest.state.client_tokens.load(Ordering::SeqCst), 2);

    // Switching to a key removes the OAuth access.
    let key = draft(
        "Daemon",
        &rest.origin,
        ApiAuth::new(AuthKind::Bearer),
        None,
        vec![ApiEndpoint::new("me", "GET", "/me", None, vec![])],
    );
    hub.apis.edit(&entry.id, key).await.unwrap();
    assert!(!hub.apis.has_secret(&entry.id));
    hub.stop().await;
}
