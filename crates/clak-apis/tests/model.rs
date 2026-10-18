//! The model: endpoints are checked before saving, command names come from method and
//! path, and the command for agents has a fixed shape.

use clak_apis::model::{
    ApiAuth, ApiDraft, ApiEndpoint, ApiEntry, ApiParameter, AuthKind, BodyKind, ParamIn, ParamType,
};
use serde_json::json;

#[test]
fn an_endpoint_is_checked_before_saving() {
    let cases: [(&str, &[&str], Option<&str>); 7] = [
        ("/users/{id}", &["id:path"], None),
        ("/users/{id}", &[], Some("isn't a parameter")),
        ("/users", &["id:path"], Some("has no {id}")),
        ("users", &[], Some("must start with /")),
        ("/a?b=1", &[], Some("must start with /")),
        ("/a", &["x:query", "x:header"], Some("two parameters")),
        ("/a", &["x:body"], Some("has none")),
    ];
    for (path, parameters, problem) in cases {
        let parameters = parameters
            .iter()
            .map(|p| {
                let (name, place) = p.split_once(':').unwrap();
                ApiParameter::new(name, ParamIn::parse(place).unwrap())
            })
            .collect();
        let endpoint = ApiEndpoint::new("e", "GET", path, None, parameters);
        match problem {
            None => assert_eq!(endpoint.problem(), None),
            Some(expected) => assert!(
                endpoint.problem().unwrap().contains(expected),
                "{path}: {:?}",
                endpoint.problem()
            ),
        }
    }
    assert!(
        ApiEndpoint::new("a b", "GET", "/", None, vec![])
            .problem()
            .unwrap()
            .contains("a command name")
    );
    assert!(
        ApiEndpoint::new("a", "FETCH", "/", None, vec![])
            .problem()
            .unwrap()
            .contains("unknown method FETCH")
    );
    // A header or a query can have names a command wouldn't have.
    let names = vec![
        ApiParameter::new("X-Trace!", ParamIn::Header),
        ApiParameter::new("filter[name]", ParamIn::Query),
    ];
    assert_eq!(
        ApiEndpoint::new("a", "GET", "/", None, names).problem(),
        None
    );
    let whole = ApiEndpoint::new(
        "a",
        "POST",
        "/",
        None,
        vec![ApiParameter::new("body", ParamIn::Query)],
    )
    .with_body(BodyKind::Json);
    assert!(whole.problem().unwrap().contains("can't be called “body”"));
}

#[test]
fn command_names_come_from_method_and_path() {
    assert_eq!(
        ApiEndpoint::name_from("GET", "/users/{id}/repos"),
        "get_users_id_repos"
    );
    assert_eq!(
        ApiEndpoint::name_from("POST", "/v2/items.json"),
        "post_v2_items_json"
    );
    assert_eq!(ApiEndpoint::name_from("GET", "/"), "get");
    assert_eq!(
        ApiEndpoint::sanitize("repos/list-for.user"),
        "repos_list-for.user"
    );
    assert_eq!(ApiEndpoint::sanitize("  __create  è__ "), "create");
    assert_eq!(ApiEndpoint::sanitize("a è b"), "a_b");
    assert_eq!(ApiEndpoint::path_names("/a/{id}/b/{name}"), ["id", "name"]);
    assert_eq!(ApiEndpoint::path_names("/a/{}/b/{x"), Vec::<String>::new());
    let long = format!("/{}", "segment/".repeat(20));
    assert!(ApiEndpoint::name_from("GET", &long).len() <= 64);
    assert!(!ApiEndpoint::name_from("GET", &long).ends_with('_'));
}

#[test]
fn the_agent_command_has_the_hub_shape() {
    let endpoint = ApiEndpoint {
        body_required: true,
        ..ApiEndpoint::new(
            "search",
            "POST",
            "/search/{kind}",
            Some("  Search things.  "),
            vec![
                ApiParameter::new("kind", ParamIn::Path).required(),
                ApiParameter {
                    default: Some("20".into()),
                    description: Some("How many.".into()),
                    ..ApiParameter::new("limit", ParamIn::Query).of_type(ParamType::Integer)
                },
                ApiParameter::new("tags", ParamIn::Query).of_type(ParamType::List),
                ApiParameter {
                    choices: Some(vec!["a".into(), "b".into()]),
                    default: Some("true".into()),
                    ..ApiParameter::new("on", ParamIn::Query).of_type(ParamType::Boolean)
                },
            ],
        )
        .with_body(BodyKind::Json)
    };
    let tool = endpoint.to_tool();
    assert_eq!(
        clak_wire::json::text(&tool),
        clak_wire::json::text(&json!({
            "name": "search",
            "description": "Search things.\n\nPOST /search/{kind}",
            "input_schema": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string"},
                    "limit": {"type": "integer", "description": "How many.", "default": 20},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "on": {"type": "boolean", "enum": ["a", "b"], "default": true},
                    "body": {"description": "The JSON body."}
                },
                "required": ["kind", "body"]
            }
        }))
    );
    // A long description is truncated at 1500, with an ellipsis.
    let long = ApiEndpoint::new("x", "GET", "/", Some(&"a".repeat(2000)), vec![]).to_tool();
    assert_eq!(long["description"].as_str().unwrap().chars().count(), 1501);
    // The whole-body schema keeps its description, after ours.
    let mut described = ApiEndpoint::new("x", "PUT", "/", None, vec![]).with_body(BodyKind::Json);
    described.body_schema = Some(
        json!({"type": "array", "description": "Notes."})
            .as_object()
            .cloned()
            .unwrap(),
    );
    assert_eq!(
        described.to_tool()["input_schema"]["properties"]["body"]["description"],
        "The JSON body. Notes."
    );
    assert!(
        described.to_tool()["input_schema"]
            .get("required")
            .is_none()
    );
}

#[test]
fn an_api_reads_back_from_disk_unchanged() {
    let entry = ApiEntry {
        id: "api_1".into(),
        name: "X".into(),
        base_url: "https://x.example".into(),
        enabled: true,
        since: 1.0,
        auth: ApiAuth::named(AuthKind::Query, "key"),
        endpoints: vec![ApiEndpoint::new("a", "GET", "/a", None, vec![])],
        command: Some("x-api".into()),
        source: None,
    };
    assert_eq!(ApiEntry::from_json(&entry.to_json()), Some(entry.clone()));
    // The file's shape, field by field and in the same order.
    assert_eq!(
        clak_wire::json::text(&entry.to_json()),
        r#"{"id":"api_1","name":"X","base_url":"https://x.example","enabled":true,"since":1.0,"auth":{"kind":"query","name":"key","authorization_url":null,"token_url":null,"scope":null,"client_id":null},"command":"x-api","source":null,"endpoints":[{"name":"a","method":"GET","path":"/a","description":null,"parameters":[],"body":null,"body_schema":null,"body_required":false}]}"#
    );
    // A hand-edited file: an invalid endpoint is discarded, an invalid command reverts to automatic.
    let loose = json!({
        "id": "api_2", "name": "Y", "base_url": "http://localhost:8123", "since": 1712345678.5,
        "auth": {"kind": "telepathy"}, "command": "Not Valid",
        "endpoints": [
            {"name": "ok", "method": "get", "path": "/states/{entity_id}", "parameters": [{"name": "entity_id", "in": "path", "type": "string", "required": false}]},
            {"name": "bad", "method": "GET", "path": "no-slash", "parameters": []}
        ]
    });
    let read = ApiEntry::from_json(&loose).unwrap();
    assert!(read.enabled);
    assert_eq!(read.auth, ApiAuth::default());
    assert_eq!(read.command, None);
    assert_eq!(read.endpoints.len(), 1);
    assert_eq!(read.endpoints[0].method, "GET");
    assert!(read.endpoints[0].parameters[0].required); // can't be missing in the path
    assert_eq!(read.authority(), "localhost:8123");
    assert_eq!(
        read.origin().unwrap(),
        json!({"kind": "connector", "protocol": "rest", "url": "http://localhost:8123", "host": "localhost", "secure": false, "loopback": true})
    );
}

#[test]
fn an_endpoint_written_by_an_agent_says_what_is_wrong() {
    let read = |v: serde_json::Value| ApiEndpoint::read(&v);
    assert_eq!(
        read(json!([])).unwrap_err(),
        "an endpoint must be a JSON object"
    );
    assert_eq!(
        read(json!({"method": "GET"})).unwrap_err(),
        "an endpoint needs a “name”"
    );
    assert_eq!(
        read(json!({"name": "x", "path": "/"})).unwrap_err(),
        "x: “method” is missing (GET, POST…)"
    );
    assert_eq!(
        read(json!({"name": "x", "method": "GET"})).unwrap_err(),
        "x: “path” is missing (/users/{id})"
    );
    assert_eq!(
        read(json!({"name": "x", "method": "GET", "path": "/", "parameters": {}})).unwrap_err(),
        "x: “parameters” must be a list"
    );
    assert_eq!(
        read(json!({"name": "x", "method": "GET", "path": "/", "parameters": [{"name": "q"}]}))
            .unwrap_err(),
        "x: “q” needs “in”: path, query, header, body"
    );
    assert_eq!(
        read(json!({"name": "x", "method": "GET", "path": "/", "parameters": [{"name": "q", "in": "query", "type": "date"}]})).unwrap_err(),
        "x: “q” has an unknown type “date” (string, integer, number, boolean, array, object)"
    );
    assert_eq!(
        read(json!({"name": "x", "method": "GET", "path": "/", "body": "xml"})).unwrap_err(),
        "x: “body” is “json”, “form” or nothing"
    );
    let ok = read(json!({"name": "x", "method": "post", "path": "/", "body": "none"})).unwrap();
    assert_eq!((ok.method.as_str(), ok.body), ("POST", None));
}

#[test]
fn access_is_checked_and_described() {
    assert_eq!(
        ApiAuth::new(AuthKind::Header).problem().unwrap(),
        "The key needs the name of its header."
    );
    assert_eq!(
        ApiAuth::new(AuthKind::Query).problem().unwrap(),
        "The key needs the name of its parameter."
    );
    assert_eq!(
        ApiAuth::named(AuthKind::Header, "X Key").problem().unwrap(),
        "That header name isn't valid."
    );
    assert_eq!(
        ApiAuth::new(AuthKind::Basic).problem().unwrap(),
        "Basic authentication needs a user name."
    );
    assert_eq!(
        ApiAuth::named(AuthKind::Basic, "ada").label(),
        "Basic, user ada"
    );
    assert_eq!(
        ApiAuth::named(AuthKind::Header, "X-Key").label(),
        "key in header X-Key"
    );
    assert_eq!(ApiAuth::named(AuthKind::Query, "k").label(), "key in ?k=");
    let app = ApiAuth::oauth(
        AuthKind::OAuthApp,
        None,
        Some("https://id.test/token"),
        None,
        Some("c"),
    );
    assert_eq!(app.problem(), None);
    assert_eq!(app.label(), "OAuth (app only) at id.test");
    assert_eq!(ApiAuth::from_json(Some(&app.to_json())), app);
}

#[test]
fn a_new_api_is_checked_before_saving() {
    let draft = |base: &str| ApiDraft {
        name: "My Service".into(),
        base_url: base.into(),
        auth: ApiAuth::default(),
        secret: None,
        endpoints: vec![ApiEndpoint::new("ping", "GET", "/ping", None, vec![])],
        command: Some(" svc ".into()),
        source: None,
        grant: None,
    };
    assert_eq!(draft("https://x.test/").problem(), None);
    assert_eq!(
        draft("").problem().unwrap(),
        "A base URL is required, http:// or https://."
    );
    assert_eq!(
        draft("ftp://x").problem().unwrap(),
        "The base URL must start with http:// or https://."
    );
    assert!(
        draft("https://u:p@x.test")
            .problem()
            .unwrap()
            .starts_with("The base URL can't contain a user name")
    );
    assert_eq!(
        draft("https://x.test/?a=1").problem().unwrap(),
        "The base URL can't have a query (?…): parameters go in the endpoints."
    );
    let mut twice = draft("https://x.test");
    twice.endpoints.push(twice.endpoints[0].clone());
    assert_eq!(twice.problem().unwrap(), "Two endpoints are called “ping”.");
    let mut none = draft("https://x.test");
    none.endpoints.clear();
    assert_eq!(none.problem().unwrap(), "Add at least one endpoint.");
    let mut command = draft("https://x.test");
    command.command = Some("list".into());
    assert!(command.problem().unwrap().contains("clak uses for itself"));

    let entry = draft(" https://x.test/ ").into_entry("api_1".into(), 2.0);
    assert_eq!(entry.base_url, "https://x.test");
    assert_eq!(entry.command.as_deref(), Some("svc"));
    assert!(entry.enabled);
}
