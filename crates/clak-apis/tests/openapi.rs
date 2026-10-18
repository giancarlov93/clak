//! The OpenAPI reader: a 3.x or 2.0 document, in JSON or YAML, becomes name, URL, access
//! and endpoints.

mod common;

use clak_apis::model::{ApiAuth, AuthKind, BodyKind, ParamType};
use clak_apis::{openapi, rest};
use common::FakeRest;
use url::Url;

const SWAGGER: &str = r#"swagger: "2.0"
info:
  title: Old Shop
host: shop.example.com
basePath: /api
schemes: [http, https]
securityDefinitions:
  basic:
    type: basic
paths:
  /orders:
    post:
      operationId: addOrder
      consumes: [application/json]
      parameters:
        - in: body
          name: order
          required: true
          schema:
            type: object
            required: [item]
            properties:
              item: { type: string }
              quantity: { type: integer, default: 1 }
  /login:
    post:
      operationId: login
      parameters:
        - { in: formData, name: user, type: string, required: true }
        - { in: formData, name: remember, type: boolean }
  /files:
    post:
      parameters:
        - { in: formData, name: file, type: file }
"#;

fn names<T: AsRef<str>>(items: impl IntoIterator<Item = T>) -> Vec<String> {
    items.into_iter().map(|s| s.as_ref().to_owned()).collect()
}

#[test]
fn an_openapi_3_gives_name_url_access_and_endpoints() {
    let source = Url::parse("https://pets.example.com/openapi.json").unwrap();
    let import = openapi::parse(common::PETSTORE, Some(&source)).unwrap();
    assert_eq!(import.name, "Petstore");
    assert_eq!(
        import.base_url.as_deref(),
        Some("https://pets.example.com/v1")
    );
    assert_eq!(import.auth, ApiAuth::named(AuthKind::Header, "X-API-Key")); // the one the document requires, not the first
    assert_eq!(
        names(import.endpoints.iter().map(|e| &e.name)),
        [
            "listPets",
            "create_pet",
            "getPet",
            "delete_pets_petid",
            "setNotes"
        ]
    );
    assert!(
        import
            .skipped
            .iter()
            .any(|s| s.starts_with("POST /pets/{petId}/photo") && s.contains("uploads a file"))
    );
    assert_eq!(import.version, "OpenAPI 3.0.3");

    let list = &import.endpoints[0];
    assert_eq!(
        names(list.parameters.iter().map(|p| &p.name)),
        ["limit", "tags", "X-Trace"]
    ); // the hub sets Accept
    assert_eq!(list.parameters[0].default.as_deref(), Some("20"));
    assert_eq!(list.parameters[1].kind, ParamType::List);

    // The body is an object: its fields become arguments, except the read-only ones.
    let create = &import.endpoints[1];
    assert_eq!(create.body, Some(BodyKind::Json));
    assert_eq!(
        names(create.parameters.iter().map(|p| &p.name)),
        ["name", "tag", "owner"]
    );
    assert!(create.parameters[0].required);
    assert_eq!(
        create.parameters[0].description.as_deref(),
        Some("What it's called.")
    );
    assert_eq!(
        create.parameters[1].choices.as_deref(),
        Some(&["cat".to_owned(), "dog".to_owned()][..])
    );
    assert_eq!(create.parameters[2].kind, ParamType::Object);
    assert!(create.parameters[2].schema.is_some()); // a circular reference (Owner → Pet → Owner) doesn't recurse forever

    let get = &import.endpoints[2];
    assert_eq!(get.parameters.len(), 1);
    assert!(get.parameters[0].required);
    assert!(
        import.endpoints[3]
            .description
            .as_deref()
            .unwrap()
            .starts_with("Deprecated.")
    );

    // A body that isn't an object stays whole.
    let notes = &import.endpoints[4];
    assert!(notes.whole_body());
    assert_eq!(notes.body_schema.as_ref().unwrap()["type"], "array");

    let tool = create.to_tool();
    assert!(
        tool["description"]
            .as_str()
            .unwrap()
            .starts_with("Add a pet.\n\nPOST /pets")
    );
    assert_eq!(
        tool["input_schema"]["required"],
        serde_json::json!(["name"])
    );
}

#[test]
fn a_swagger_2_in_yaml_reads_the_same_way() {
    let import = openapi::parse(SWAGGER, None).unwrap();
    assert_eq!(import.name, "Old Shop");
    assert_eq!(
        import.base_url.as_deref(),
        Some("https://shop.example.com/api")
    ); // https, if present
    assert_eq!(import.auth.kind, AuthKind::Basic);
    assert!(import.version.starts_with("Swagger 2"));

    let order = import
        .endpoints
        .iter()
        .find(|e| e.name == "addOrder")
        .unwrap();
    assert_eq!(
        names(order.parameters.iter().map(|p| &p.name)),
        ["item", "quantity"]
    );
    assert_eq!(order.parameters[1].default.as_deref(), Some("1"));
    assert_eq!(order.parameters[1].kind, ParamType::Integer);
    assert!(order.parameters[0].required);

    let login = import.endpoints.iter().find(|e| e.name == "login").unwrap();
    assert_eq!(login.body, Some(BodyKind::Form));
    assert_eq!(login.parameters[1].kind, ParamType::Boolean);
    assert!(login.body_required);
    assert!(import.skipped.iter().any(|s| s.starts_with("POST /files")));
}

#[test]
fn a_bad_document_says_why() {
    for (text, expected) in [
        ("{}", "Not an OpenAPI document"),
        ("openapi: 3.0.0\npaths: {}", "no endpoints"),
        ("{ nope", "JSON isn't valid"),
        ("a: [1, 2", "Neither JSON nor valid YAML"),
        ("", "empty"),
        ("- a\n- b", "isn't an object"),
        (
            r#"{"openapi": "3.0.0", "openapi": "3.1.0"}"#,
            "JSON isn't valid",
        ),
    ] {
        let error = openapi::parse(text, None).unwrap_err();
        assert!(error.0.contains(expected), "{text:?}: {error}");
    }
}

#[test]
fn json_accepts_comments_and_trailing_commas() {
    let import = openapi::parse(
        r#"{
          // a comment
          "openapi": "3.0.0", /* another, with "quotes" */
          "info": { "title": "C // not a comment", },
          "paths": { "/x": { "get": { "operationId": "x", }, }, },
        }"#,
        None,
    )
    .unwrap();
    assert_eq!(import.name, "C // not a comment");
    assert_eq!(import.endpoints[0].name, "x");
    assert_eq!(import.base_url, None);
}

#[test]
fn an_openapi_with_oauth_brings_urls_and_scopes() {
    let source = Url::parse("https://hub.test/openapi.json").unwrap();
    let code = openapi::parse(
        r#"{
          "openapi": "3.0.0", "info": { "title": "Hub" },
          "servers": [ { "url": "https://api.hub.test/v2" } ],
          "components": { "securitySchemes": {
            "oauth": { "type": "oauth2", "flows": {
              "clientCredentials": { "tokenUrl": "https://hub.test/token", "scopes": {} },
              "authorizationCode": { "authorizationUrl": "/login/authorize", "tokenUrl": "https://hub.test/token", "scopes": { "repo": "", "user": "" } } } } } },
          "paths": {
            "/user": { "get": { "operationId": "me", "security": [ { "oauth": ["user"] } ] } },
            "/repos": { "get": { "operationId": "repos", "security": [ { "oauth": ["repo", "user"] } ] } }
          }
        }"#,
        Some(&source),
    )
    .unwrap();
    // The browser code before "app only"; the relative URL, against the document;
    // the scopes the operations require, not all the declared ones.
    assert_eq!(
        code.auth,
        ApiAuth::oauth(
            AuthKind::OAuth,
            Some("https://hub.test/login/authorize"),
            Some("https://hub.test/token"),
            Some("user repo"),
            None
        )
    );
    assert_eq!(code.base_url.as_deref(), Some("https://api.hub.test/v2"));
    assert_eq!(code.auth.label(), "OAuth at hub.test");
    assert!(code.auth.problem().unwrap().contains("client ID")); // the user registers the client

    let app = openapi::parse(
        r#"swagger: "2.0"
info: { title: Old }
host: old.test
security: [ { app: [read] } ]
securityDefinitions:
  app: { type: oauth2, flow: application, tokenUrl: "https://old.test/oauth/token", scopes: { read: "" } }
paths:
  /x: { get: { operationId: x } }
"#,
        None,
    )
    .unwrap();
    assert_eq!(
        app.auth,
        ApiAuth::oauth(
            AuthKind::OAuthApp,
            None,
            Some("https://old.test/oauth/token"),
            Some("read"),
            None
        )
    );

    // A sign-in service over plain http isn't accepted.
    let insecure = ApiAuth::oauth(
        AuthKind::OAuth,
        Some("http://hub.test/a"),
        Some("https://hub.test/t"),
        None,
        Some("c"),
    );
    assert!(insecure.problem().unwrap().contains("https"));
    let loopback = ApiAuth::oauth(
        AuthKind::OAuth,
        Some("http://127.0.0.1:9/a"),
        Some("http://localhost/t"),
        None,
        Some("c"),
    );
    assert_eq!(loopback.problem(), None);
}

#[test]
fn other_access_kinds_and_notes() {
    let doc = |schemes: &str, security: &str| {
        format!(
            r#"{{"openapi": "3.0.0", "security": {security}, "components": {{"securitySchemes": {schemes}}},
                "paths": {{"/x": {{"get": {{"operationId": "x"}}}}}}}}"#
        )
    };
    let auth = |schemes: &str| openapi::parse(&doc(schemes, "[]"), None).unwrap();
    assert_eq!(
        auth(r#"{"k": {"type": "apiKey", "in": "header", "name": "Authorization"}}"#)
            .auth
            .kind,
        AuthKind::Bearer
    );
    assert_eq!(
        auth(r#"{"k": {"type": "apiKey", "in": "query", "name": "api_key"}}"#).auth,
        ApiAuth::named(AuthKind::Query, "api_key")
    );
    assert_eq!(
        auth(r#"{"k": {"type": "http", "scheme": "Bearer"}}"#)
            .auth
            .kind,
        AuthKind::Bearer
    );
    assert_eq!(
        auth(r#"{"k": {"type": "http", "scheme": "basic"}}"#)
            .auth
            .kind,
        AuthKind::Basic
    );
    assert_eq!(
        auth(r#"{"k": {"type": "openIdConnect"}}"#).auth.kind,
        AuthKind::Bearer
    );
    assert_eq!(
        auth(r#"{"k": {"type": "mutualTLS"}}"#).auth.kind,
        AuthKind::None
    );
    let cookie = auth(r#"{"k": {"type": "apiKey", "in": "cookie", "name": "sid"}}"#);
    assert_eq!(cookie.auth.kind, AuthKind::None);
    assert!(
        cookie
            .skipped
            .iter()
            .any(|s| s.contains("cookie isn't supported"))
    );
}

#[test]
fn endpoints_left_out() {
    let import = openapi::parse(
        r#"{"openapi": "3.0.0", "paths": {
          "/a": {"get": {"operationId": "a", "parameters": [{"name": "sid", "in": "cookie", "required": true}]}},
          "/b": {"post": {"operationId": "b", "requestBody": {"required": true, "content": {"application/xml": {}}}}},
          "/c": {"post": {"operationId": "c", "requestBody": {"content": {"application/x-www-form-urlencoded": {"schema": {"type": "object"}}}}}},
          "/d": {"get": {"operationId": "d"}},
          "/e": {"get": {"operationId": "d"}},
          "/f": {"post": {"operationId": "f", "requestBody": {"content": {"application/x-www-form-urlencoded": {"schema": {"type": "object",
              "properties": {"q": {"type": "string"}, "n": {"type": "integer", "format": "int32"}}}}}}}}
        }}"#,
        None,
    )
    .unwrap();
    assert!(
        import
            .skipped
            .iter()
            .any(|s| s == "GET /a: needs the cookie “sid”")
    );
    assert!(
        import
            .skipped
            .iter()
            .any(|s| s == "POST /b: sends application/xml, not JSON")
    );
    assert!(
        import
            .skipped
            .iter()
            .any(|s| s == "POST /c: sends a form without fields")
    );
    // Two operations with the same name: the second gets a number.
    assert_eq!(
        names(import.endpoints.iter().map(|e| &e.name)),
        ["d", "d_2", "f"]
    );
    let form = &import.endpoints[2];
    assert_eq!(form.body, Some(BodyKind::Form));
    assert_eq!(form.parameters[1].kind, ParamType::Integer);
    assert!(form.parameters[1].schema.is_some()); // the format says more than the type
    assert!(form.parameters[0].schema.is_none());
    // Without servers and without a source URL, there's no base URL.
    assert_eq!(import.base_url, None);
    assert_eq!(import.name, "API");
}

#[tokio::test]
async fn a_document_loads_from_a_url_or_a_file() {
    let rest = FakeRest::start().await;
    rest.set_document(common::PETSTORE);
    let http = rest::new_http_client().unwrap();
    let import = openapi::load(&http, &format!(" {}/openapi.json ", rest.origin))
        .await
        .unwrap();
    assert_eq!(import.base_url, Some(format!("{}/v1", rest.origin)));
    assert_eq!(import.endpoints.len(), 5);

    rest.set_document(SWAGGER);
    let yaml = openapi::load(&http, &format!("{}/openapi.yaml", rest.origin))
        .await
        .unwrap();
    assert_eq!(yaml.name, "Old Shop");

    rest.respond(|_| common::Reply::new(404, "nope"));
    let missing = openapi::load(&http, &format!("{}/other.json", rest.origin))
        .await
        .unwrap_err();
    assert_eq!(missing.0, "HTTP 404 Not Found");

    let folder = std::env::temp_dir().join(format!("clak-apis-{}", clak_wire::json::random_hex(6)));
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("shop.yaml");
    std::fs::write(&file, format!("\u{FEFF}{SWAGGER}")).unwrap();
    let from_file = openapi::load(&http, file.to_str().unwrap()).await.unwrap();
    assert_eq!(from_file.endpoints.len(), 2);
    let nothing = openapi::load(&http, folder.join("none.yaml").to_str().unwrap())
        .await
        .unwrap_err();
    assert!(
        nothing
            .0
            .contains("Neither an http(s) address nor an existing file")
    );
    std::fs::remove_dir_all(&folder).unwrap();

    let refused = openapi::load(&http, "http://127.0.0.1:1/openapi.json")
        .await
        .unwrap_err();
    assert!(refused.0.starts_with("Not reached"), "{refused}");
}
