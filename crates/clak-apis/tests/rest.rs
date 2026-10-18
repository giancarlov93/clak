//! Calls: a command becomes an HTTP request to a fake service, and the response becomes
//! what the agent receives.

mod common;

use clak_apis::model::{
    ApiAuth, ApiEndpoint, ApiEntry, ApiParameter, AuthKind, BodyKind, ParamIn, ParamType,
};
use clak_apis::openapi;
use clak_apis::rest::{self, AccessToken, StaticToken};
use common::{FakeOAuth, FakeRest, Reply, args, content, is_error};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn entry(name: &str, base_url: &str, auth: ApiAuth, endpoints: Vec<ApiEndpoint>) -> ApiEntry {
    ApiEntry {
        id: "api_1".into(),
        name: name.into(),
        base_url: base_url.into(),
        enabled: true,
        since: 0.0,
        auth,
        endpoints,
        command: None,
        source: None,
    }
}

#[tokio::test]
async fn an_imported_api_answers_agents_with_its_access() {
    let rest = FakeRest::start().await;
    rest.set_document(common::PETSTORE);
    let http = rest::new_http_client().unwrap();
    let import = openapi::load(&http, &format!("{}/openapi.json", rest.origin))
        .await
        .unwrap();
    assert_eq!(import.base_url, Some(format!("{}/v1", rest.origin)));
    let api = entry(
        &import.name,
        import.base_url.as_deref().unwrap(),
        import.auth.clone(),
        import.endpoints.clone(),
    );
    let key = StaticToken::new("k123");
    let token: Option<&dyn AccessToken> = Some(&key);

    let answer = rest::call(
        &api,
        "listPets",
        &args(json!({"limit": 5, "tags": ["a", "b c"]})),
        token,
        &http,
        None,
    )
    .await;
    assert!(!is_error(&answer));
    assert_eq!(content(&answer), "GET /v1/pets?limit=5&tags=a&tags=b%20c");
    assert_eq!(rest.last().headers["x-api-key"], "k123");
    assert_eq!(rest.last().headers["accept"], "application/json, */*;q=0.8");
    assert_eq!(rest.last().headers["user-agent"], "clak");

    // The default value is sent even if the agent doesn't give it.
    rest::call(
        &api,
        "listPets",
        &args(json!({"X-Trace": "t-1"})),
        token,
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().query, "?limit=20");
    assert_eq!(rest.last().headers["x-trace"], "t-1");

    let answer = rest::call(
        &api,
        "create_pet",
        &args(json!({"name": "Rex", "tag": "dog"})),
        token,
        &http,
        None,
    )
    .await;
    assert!(!is_error(&answer));
    assert_eq!(rest.last().method, "POST");
    assert_eq!(rest.last().body, r#"{"name":"Rex","tag":"dog"}"#);
    assert!(
        rest.last()
            .content_type
            .unwrap()
            .starts_with("application/json")
    );

    // The path is encoded: the slash stays inside its segment.
    rest::call(
        &api,
        "getPet",
        &args(json!({"petId": "a/b c"})),
        token,
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().path, "/v1/pets/a%2Fb%20c");

    // The whole body, from the command line as text: if it's JSON, it's sent as JSON.
    rest::call(
        &api,
        "setNotes",
        &args(json!({"petId": "7", "body": r#"["one","two"]"#})),
        token,
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().body, r#"["one","two"]"#);
    assert_eq!(rest.last().method, "PUT");
    rest::call(
        &api,
        "setNotes",
        &args(json!({"petId": "7", "body": "not json"})),
        token,
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().body, r#""not json""#);

    // A missing required argument isn't even sent.
    let before = rest.requests().len();
    let answer = rest::call(
        &api,
        "create_pet",
        &args(json!({"tag": "cat"})),
        token,
        &http,
        None,
    )
    .await;
    assert!(is_error(&answer));
    assert_eq!(content(&answer), "Missing --name.");
    assert_eq!(rest.requests().len(), before);

    // A command that doesn't exist.
    let answer = rest::call(&api, "nope", &args(json!({})), token, &http, None).await;
    assert_eq!(content(&answer), "Petstore has no command “nope”.");

    // A service error is an error, with its beginning.
    rest.respond(|_| Reply::new(404, r#"{"error":"no such pet"}"#));
    let answer = rest::call(
        &api,
        "getPet",
        &args(json!({"petId": "9"})),
        token,
        &http,
        None,
    )
    .await;
    assert!(is_error(&answer));
    assert_eq!(
        content(&answer),
        "HTTP 404 Not Found\n{\"error\":\"no such pet\"}"
    );
    rest.respond(|_| Reply::new(500, ""));
    assert_eq!(
        content(
            &rest::call(
                &api,
                "getPet",
                &args(json!({"petId": "9"})),
                token,
                &http,
                None
            )
            .await
        ),
        "HTTP 500 Internal Server Error"
    );
}

#[tokio::test]
async fn a_hand_made_api_with_bearer_basic_and_a_form() {
    let rest = FakeRest::start().await;
    let http = rest::new_http_client().unwrap();
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
    let api = entry(
        "My Service",
        &format!("{}/", rest.origin),
        ApiAuth::new(AuthKind::Bearer),
        endpoints.clone(),
    );
    let tok = StaticToken::new("tok");

    rest::call(
        &api,
        "search",
        &args(json!({"q": "café déjà"})),
        Some(&tok),
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().path, "/search");
    assert_eq!(rest.last().query, "?q=caf%C3%A9%20d%C3%A9j%C3%A0");
    assert_eq!(rest.last().headers["authorization"], "Bearer tok");

    rest::call(
        &api,
        "send",
        &args(json!({"kind": "note", "text": "a&b c", "urgent": true})),
        Some(&tok),
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().path, "/forms/note");
    assert_eq!(rest.last().body, "text=a%26b+c&urgent=true");
    assert_eq!(
        rest.last().content_type.as_deref(),
        Some("application/x-www-form-urlencoded")
    );

    // Basic: user in plain text, password from the secret.
    let basic = entry(
        "My Service",
        &rest.origin,
        ApiAuth::named(AuthKind::Basic, "ada"),
        endpoints.clone(),
    );
    rest::call(
        &basic,
        "search",
        &args(json!({"q": "x"})),
        Some(&StaticToken::new("pw")),
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().headers["authorization"], "Basic YWRhOnB3");

    // A key in the query, and one in a header that replaces the agent's.
    let query = entry(
        "Q",
        &rest.origin,
        ApiAuth::named(AuthKind::Query, "api key"),
        endpoints.clone(),
    );
    rest::call(
        &query,
        "search",
        &args(json!({"q": "x"})),
        Some(&StaticToken::new("s&1")),
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().query, "?q=x&api%20key=s%261");
    let header_endpoints = vec![ApiEndpoint::new(
        "ping",
        "GET",
        "/ping",
        None,
        vec![ApiParameter::new("X-Key", ParamIn::Header)],
    )];
    let header = entry(
        "H",
        &rest.origin,
        ApiAuth::named(AuthKind::Header, "X-Key"),
        header_endpoints,
    );
    rest::call(
        &header,
        "ping",
        &args(json!({"X-Key": "mine"})),
        Some(&StaticToken::new("s3cret")),
        &http,
        None,
    )
    .await;
    assert_eq!(rest.last().headers["x-key"], "s3cret");
    // Without a secret, no auth header.
    rest::call(&api, "search", &args(json!({"q": "x"})), None, &http, None).await;
    assert!(!rest.last().headers.contains_key("authorization"));
}

#[tokio::test]
async fn another_host_does_not_get_the_secret() {
    let rest = FakeRest::start().await;
    let other = FakeRest::start().await;
    let http = rest::new_http_client().unwrap();
    let api = entry(
        "Ping",
        &rest.origin,
        ApiAuth::named(AuthKind::Header, "X-Key"),
        vec![
            ApiEndpoint::new(
                "ping",
                "POST",
                "/ping",
                None,
                vec![ApiParameter::new("n", ParamIn::Body)],
            )
            .with_body(BodyKind::Json),
        ],
    );
    let key = StaticToken::new("s3cret");

    // A redirect to another host isn't followed: the secret would go along with it.
    let target = format!("{}/steal?x=1", other.origin);
    rest.respond(move |_| Reply::redirect(302, &target));
    let answer = rest::call(&api, "ping", &args(json!({})), Some(&key), &http, None).await;
    assert!(is_error(&answer));
    assert_eq!(
        content(&answer),
        format!(
            "HTTP 302: Ping redirects to {}/steal, another host. Not followed, so the credentials don't go with it.",
            other.origin
        )
    );
    assert!(other.requests().is_empty());

    // On the same origin it is: a 301 becomes GET without a body, a 307 keeps method and body.
    rest.respond(|seen| {
        if seen.path == "/ping" {
            Reply::redirect(301, "/moved")
        } else {
            Reply::new(200, "here")
        }
    });
    let answer = rest::call(
        &api,
        "ping",
        &args(json!({"n": "1"})),
        Some(&key),
        &http,
        None,
    )
    .await;
    assert_eq!(content(&answer), "here");
    assert_eq!(
        (
            rest.last().method.as_str(),
            rest.last().path.as_str(),
            rest.last().body.as_str()
        ),
        ("GET", "/moved", "")
    );
    assert_eq!(rest.last().headers["x-key"], "s3cret");
    rest.respond(|seen| {
        if seen.path == "/ping" {
            Reply::redirect(307, "/moved")
        } else {
            Reply::new(200, "here")
        }
    });
    rest::call(
        &api,
        "ping",
        &args(json!({"n": "1"})),
        Some(&key),
        &http,
        None,
    )
    .await;
    assert_eq!(
        (rest.last().method.as_str(), rest.last().body.as_str()),
        ("POST", r#"{"n":"1"}"#)
    );

    // An endless loop stops after five hops, and the 3xx arrives as an error.
    rest.respond(|_| Reply::redirect(302, "/again"));
    let before = rest.requests().len();
    let answer = rest::call(&api, "ping", &args(json!({})), Some(&key), &http, None).await;
    assert!(is_error(&answer));
    assert!(content(&answer).starts_with("HTTP 302 Found"));
    assert_eq!(rest.requests().len() - before, 6);
}

#[tokio::test]
async fn an_image_or_a_file_arrives_as_an_attachment() {
    let rest = FakeRest::start().await;
    let http = rest::new_http_client().unwrap();
    let png = vec![0x89, 0x50, 0x4E, 0x47, 1, 2, 3];
    let bytes = png.clone();
    rest.respond(move |seen| {
        if seen.path == "/logo.png" {
            Reply::new(200, "").typed("image/png").bytes(bytes.clone())
        } else {
            Reply::new(200, "")
                .typed("application/pdf")
                .bytes(vec![b'%', b'P', b'D', b'F'])
        }
    });
    let api = entry(
        "Pics",
        &rest.origin,
        ApiAuth::default(),
        vec![
            ApiEndpoint::new("logo", "GET", "/logo.png", None, vec![]),
            ApiEndpoint::new("doc", "GET", "/doc.pdf", None, vec![]),
        ],
    );
    let answer = rest::call(&api, "logo", &args(json!({})), None, &http, None).await;
    let blocks = answer["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "image");
    assert_eq!(blocks[0]["mimeType"], "image/png");
    use base64::Engine as _;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(blocks[0]["data"].as_str().unwrap())
            .unwrap(),
        png
    );

    let answer = rest::call(&api, "doc", &args(json!({})), None, &http, None).await;
    assert_eq!(answer["content"][0]["type"], "resource");
    assert_eq!(
        answer["content"][0]["resource"]["uri"],
        format!("{}/doc.pdf", rest.origin)
    );
    assert_eq!(answer["content"][0]["resource"]["blob"], "JVBERg==");
    assert_eq!(answer["is_error"], false);
}

#[tokio::test]
async fn a_long_text_is_truncated_and_says_so() {
    let rest = FakeRest::start().await;
    let http = rest::new_http_client().unwrap();
    let api = entry(
        "Big",
        &rest.origin,
        ApiAuth::default(),
        vec![ApiEndpoint::new("big", "GET", "/big", None, vec![])],
    );
    rest.respond(|_| {
        Reply::new(200, &"x".repeat(rest::MAX_RESPONSE + 10)).typed("text/plain; charset=utf-8")
    });
    let text = content(&rest::call(&api, "big", &args(json!({})), None, &http, None).await);
    assert!(text.ends_with("\n[… truncated at 200 KB]"));
    assert_eq!(
        text.len(),
        rest::MAX_RESPONSE + "\n[… truncated at 200 KB]".len()
    );

    rest.respond(|_| Reply::new(400, &"e".repeat(5000)));
    let answer = rest::call(&api, "big", &args(json!({})), None, &http, None).await;
    assert!(is_error(&answer));
    assert!(content(&answer).starts_with("HTTP 400 Bad Request\neeee"));
    assert!(content(&answer).ends_with("e […]"));

    // Latin-1 is read as such; an empty body gives the status.
    rest.respond(|_| {
        Reply::new(200, "")
            .typed("text/plain; charset=ISO-8859-1")
            .bytes(vec![b'c', b'a', b'f', 0xE9])
    });
    assert_eq!(
        content(&rest::call(&api, "big", &args(json!({})), None, &http, None).await),
        "café"
    );
    rest.respond(|_| Reply::new(204, ""));
    assert_eq!(
        content(&rest::call(&api, "big", &args(json!({})), None, &http, None).await),
        "HTTP 204 No Content"
    );
}

#[tokio::test]
async fn the_next_page_arrives_as_command_arguments() {
    let rest = FakeRest::start().await;
    let http = rest::new_http_client().unwrap();
    let endpoints = vec![
        ApiEndpoint::new(
            "items",
            "GET",
            "/items",
            None,
            vec![
                ApiParameter::new("page", ParamIn::Query).of_type(ParamType::Integer),
                ApiParameter::new("per_page", ParamIn::Query).of_type(ParamType::Integer),
            ],
        ),
        ApiEndpoint::new("other", "GET", "/other", None, vec![]),
    ];
    let api = entry("Shop", &rest.origin, ApiAuth::default(), endpoints);

    let origin = rest.origin.clone();
    rest.respond(move |_| {
        Reply::new(200, "[1,2]")
            .header("Link", &format!(r#"<{origin}/items?page=3&per_page=2>; rel="next", <{origin}/items?page=21&per_page=2>; rel="last""#))
            .header("X-Total-Count", "42")
    });
    let answer = rest::call(
        &api,
        "items",
        &args(json!({"page": 2, "per_page": 2})),
        None,
        &http,
        None,
    )
    .await;
    let blocks = answer["content"].as_array().unwrap();
    assert_eq!(blocks[0]["text"], "[1,2]"); // JSON stays JSON
    assert_eq!(blocks[1]["text"], "[Next page: --page 3]\n[Total: 42]");

    // A URL the command can't request: say so, without inventing arguments.
    let origin = rest.origin.clone();
    rest.respond(move |_| {
        Reply::new(200, "[]").header(
            "Link",
            &format!(r#"<{origin}/other?cursor=abc>; rel="next""#),
        )
    });
    assert!(
        content(&rest::call(&api, "other", &args(json!({})), None, &http, None).await)
            .contains("can't ask for it")
    );

    // A value that needs quotes, and a rel with several names.
    rest.respond(|_| {
        Reply::new(200, "[]").header("link", r#"</items?page=a%20b>; rel="prefetch next""#)
    });
    let text =
        content(&rest::call(&api, "items", &args(json!({"page": 1})), None, &http, None).await);
    assert!(text.ends_with(r#"[Next page: --page "a b"]"#), "{text}");
}

#[tokio::test]
async fn an_oauth_api_renews_after_a_401_then_asks_the_user() {
    let rest = FakeRest::start().await;
    rest.require_token();
    let http = rest::new_http_client().unwrap();
    let auth = ApiAuth::oauth(
        AuthKind::OAuth,
        Some("https://id.test/a"),
        Some("https://id.test/t"),
        Some("read"),
        Some("app-1"),
    );
    let api = entry(
        "Hub",
        &rest.origin,
        auth,
        vec![ApiEndpoint::new("me", "GET", "/me", None, vec![])],
    );
    let oauth = FakeOAuth::new(&rest);
    let lost = Arc::new(AtomicUsize::new(0));
    let counter = lost.clone();
    let signal = move || {
        counter.fetch_add(1, Ordering::SeqCst);
    };

    assert_eq!(
        content(
            &rest::call(
                &api,
                "me",
                &args(json!({})),
                Some(&oauth),
                &http,
                Some(&signal)
            )
            .await
        ),
        "GET /me"
    );
    assert_eq!(rest.last().headers["authorization"], "Bearer at-1");

    // The token expires earlier than expected: a 401, a renewal, the same request again.
    rest.expire_tokens();
    assert_eq!(
        content(
            &rest::call(
                &api,
                "me",
                &args(json!({})),
                Some(&oauth),
                &http,
                Some(&signal)
            )
            .await
        ),
        "GET /me"
    );
    assert_eq!(oauth.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(rest.last().headers["authorization"], "Bearer at-2");

    // The sign-in service doesn't answer: try again later.
    rest.expire_tokens();
    oauth.unreachable.store(true, Ordering::SeqCst);
    let answer = rest::call(
        &api,
        "me",
        &args(json!({})),
        Some(&oauth),
        &http,
        Some(&signal),
    )
    .await;
    assert_eq!(
        content(&answer),
        "Hub: the sign-in service isn't reachable (connection refused): try again shortly."
    );
    oauth.unreachable.store(false, Ordering::SeqCst);

    // The service no longer renews: the agent learns the user is needed, and so does the panel.
    oauth.refuse.store(true, Ordering::SeqCst);
    let answer = rest::call(
        &api,
        "me",
        &args(json!({})),
        Some(&oauth),
        &http,
        Some(&signal),
    )
    .await;
    assert!(is_error(&answer));
    assert_eq!(
        content(&answer),
        "Hub needs the user to sign in: ask them to open Clak, APIs tab, and choose Sign in on Hub."
    );
    assert_eq!(lost.load(Ordering::SeqCst), 1);
    // From here on nothing is sent, until the user signs in again.
    let before = rest.requests().len();
    assert!(
        content(
            &rest::call(
                &api,
                "me",
                &args(json!({})),
                Some(&oauth),
                &http,
                Some(&signal)
            )
            .await
        )
        .contains("needs the user to sign in")
    );
    assert_eq!(rest.requests().len(), before);

    // Without a saved OAuth access, the same; for "app only" the text mentions the client.
    assert!(
        content(&rest::call(&api, "me", &args(json!({})), None, &http, None).await)
            .contains("needs the user to sign in")
    );
    let mut app = api.clone();
    app.auth.kind = AuthKind::OAuthApp;
    assert_eq!(
        rest::sign_in_text(&app),
        "Hub refused the app's access: ask the user to check its OAuth client in Clak, APIs tab, Edit on Hub."
    );
}

#[tokio::test]
async fn a_slow_or_down_service_says_so() {
    let rest = FakeRest::start().await;
    let http = rest::new_http_client().unwrap();
    let api = entry(
        "Slow",
        &rest.origin,
        ApiAuth::default(),
        vec![ApiEndpoint::new("wait", "GET", "/wait", None, vec![])],
    );
    rest.respond(|_| Reply::new(200, "late").after(Duration::from_secs(5)));
    let answer = rest::call_within(
        &api,
        "wait",
        &args(json!({})),
        None,
        &http,
        None,
        Duration::from_millis(300),
    )
    .await;
    assert!(is_error(&answer));
    assert_eq!(content(&answer), "Slow didn't answer within 0 seconds.");

    let gone = entry(
        "Gone",
        "http://127.0.0.1:1",
        ApiAuth::default(),
        vec![ApiEndpoint::new("x", "GET", "/x", None, vec![])],
    );
    let answer = rest::call(&gone, "x", &args(json!({})), None, &http, None).await;
    assert!(is_error(&answer));
    assert!(
        content(&answer).starts_with("Gone is not reachable: "),
        "{}",
        content(&answer)
    );
    assert!(!content(&answer).contains("127.0.0.1:1/x"));
}
