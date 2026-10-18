//! The YAML reader: it must understand OpenAPI documents as people write them, and stop on
//! what it doesn't understand or what would cost too much. Numbers become JSON numbers
//! (`3.10` is `3.1`).

use clak_apis::yaml::{self, YamlError};
use serde_json::{Value, json};

fn json(text: &str) -> String {
    clak_wire::json::text(&yaml::parse(text).unwrap_or_else(|e| panic!("{text:?}: {e}")))
}

fn doc(text: &str) -> Value {
    yaml::parse(text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
}

fn failure(text: &str) -> Option<YamlError> {
    yaml::parse(text).err()
}

// ---- structure ----

#[test]
fn block_mappings_and_sequences() {
    assert_eq!(
        json("a: 1\nb:\n  c: x\n  d:\n    - 1\n    - two\ne:\n- p\n- q\nf: last"),
        r#"{"a":1,"b":{"c":"x","d":[1,"two"]},"e":["p","q"],"f":"last"}"#
    );
}

#[test]
fn compact_entries_in_a_sequence() {
    assert_eq!(
        json(
            "- name: a\n  tags:\n  - x\n  - y\n- name: b\n  more:\n    deep: true\n- - inner\n  - list\n-\n  late: 1\n-\n- plain"
        ),
        r#"[{"name":"a","tags":["x","y"]},{"name":"b","more":{"deep":true}},["inner","list"],{"late":1},null,"plain"]"#
    );
}

#[test]
fn empty_values_are_null() {
    assert_eq!(
        json("a:\nb: # nothing\nc: ~\n"),
        r#"{"a":null,"b":null,"c":null}"#
    );
    assert_eq!(json(""), "null");
    assert_eq!(json("# just a comment\n"), "null");
}

#[test]
fn flow_collections_even_across_lines() {
    assert_eq!(
        json("a: {b: 1, c: [x, y]}"),
        r#"{"a":{"b":1,"c":["x","y"]}}"#
    );
    assert_eq!(
        json(r#"[a, "b", {c: d}, 'e', [], {}]"#),
        r#"["a","b",{"c":"d"},"e",[],{}]"#
    );
    assert_eq!(
        json(
            "a: {b: 1,   # comment\n  c: [x, y,\n    z],\n  d: }\ne: [1, 2, ]\nf: [k: v, \"q\": 1]\ng: {\"json\": \"like\", \"n\": 1.5}\nh: {a, b: 2}"
        ),
        r#"{"a":{"b":1,"c":["x","y","z"],"d":null},"e":[1,2],"f":[{"k":"v"},{"q":1}],"g":{"json":"like","n":1.5},"h":{"a":null,"b":2}}"#
    );
}

// ---- scalars ----

#[test]
fn plain_scalars_even_across_lines() {
    let text = [
        "url: https://example.com/a/b?x=1#frag",
        "time: 12:30",
        "hash: a#b # this is a comment",
        "spaced: two words   ",
        "long: this line",
        "  goes on here",
        "  and here",
        "",
        "  and after a blank line",
        "next: ok",
        "dash: -x",
    ]
    .join("\n");
    assert_eq!(
        json(&text),
        r#"{"url":"https://example.com/a/b?x=1#frag","time":"12:30","hash":"a#b","spaced":"two words","long":"this line goes on here and here\nand after a blank line","next":"ok","dash":"-x"}"#
    );
    assert_eq!(json("- one\n  two\n- three"), r#"["one two","three"]"#);
    assert_eq!(
        json("just text\non two lines"),
        r#""just text on two lines""#
    );
}

#[test]
fn plain_scalar_types() {
    let cases = [
        ("", "null"),
        ("~", "null"),
        ("null", "null"),
        ("Null", "null"),
        ("NULL", "null"),
        ("nULL", r#""nULL""#),
        ("true", "true"),
        ("True", "true"),
        ("TRUE", "true"),
        ("false", "false"),
        ("False", "false"),
        ("FALSE", "false"),
        ("yes", r#""yes""#),
        ("on", r#""on""#),
        ("tRUE", r#""tRUE""#),
        ("0", "0"),
        ("42", "42"),
        ("-17", "-17"),
        ("+5", "5"),
        ("007", "7"),
        ("-0", "0"),
        ("9223372036854775807", "9223372036854775807"),
        ("99999999999999999999", "1e+20"),
        ("2.0", "2.0"),
        ("3.10", "3.1"),
        (".5", "0.5"),
        ("-.5", "-0.5"),
        ("+1.5", "1.5"),
        ("1.", "1.0"),
        ("1e5", "100000.0"),
        ("1.5E-3", "0.0015"),
        ("-2e+10", "-20000000000.0"),
        ("1.e2", "100.0"),
        ("1.0.0", r#""1.0.0""#),
        ("0x1F", r#""0x1F""#),
        ("0o17", r#""0o17""#),
        (".inf", r#"".inf""#),
        (".nan", r#"".nan""#),
        ("1_000", r#""1_000""#),
        ("1e", r#""1e""#),
        ("+", r#""+""#),
        (".", r#"".""#),
        ("12:30", r#""12:30""#),
    ];
    for (text, expected) in cases {
        assert_eq!(
            json(&format!("v: {text}")),
            format!(r#"{{"v":{expected}}}"#),
            "{text}"
        );
    }
}

#[test]
fn quoted_scalars_are_always_text() {
    assert_eq!(json("a: '123'"), r#"{"a":"123"}"#);
    assert_eq!(json(r#"a: "true""#), r#"{"a":"true"}"#);
    assert_eq!(json("a: ''"), r#"{"a":""}"#);
    assert_eq!(
        json("a: 'it''s # not a comment'"),
        r#"{"a":"it's # not a comment"}"#
    );
    assert_eq!(
        doc(r#"a: "tab\there \"q\" back\\slash \/ \u00e8 \U0001F600 \x41 \ud83d\ude00 \0""#)["a"],
        json!("tab\there \"q\" back\\slash / è 😀 A 😀 \0")
    );
    assert_eq!(
        json("a: \"one\n  two  \n\n  three\""),
        r#"{"a":"one two\nthree"}"#
    );
    assert_eq!(json("a: 'one\n  two'"), r#"{"a":"one two"}"#);
    assert_eq!(json("a: \"joined\\\n   here\""), r#"{"a":"joinedhere"}"#);
    assert_eq!(
        json("\"quoted key\": 1\n'single key': 2"),
        r#"{"quoted key":1,"single key":2}"#
    );
}

#[test]
fn literal_and_folded_blocks() {
    let d = doc(
        "literal: |\n  one\n    two\n\n  three\nfolded: >\n  one\n  two\n\n  three\n    indented\n  four\nstrip: |-\n  x\n\nkeep: |+\n  x\n\n\nclip: >-\n  a\n  b\nexplicit: |2\n    indent\n  text\ncomment: | # a comment\n  c\nlast: |\n  end",
    );
    assert_eq!(d["literal"], "one\n  two\n\nthree\n");
    assert_eq!(d["folded"], "one two\nthree\n  indented\nfour\n");
    assert_eq!(d["strip"], "x");
    assert_eq!(d["keep"], "x\n\n\n");
    assert_eq!(d["clip"], "a b");
    assert_eq!(d["explicit"], "  indent\ntext\n");
    assert_eq!(d["comment"], "c\n");
    assert_eq!(d["last"], "end"); // no line break at the end of the file, so none to keep
    assert_eq!(
        json("- |\n  in a list\n- >-\n  folded\n  here\n"),
        r#"["in a list\n","folded here"]"#
    );
    assert_eq!(json("a: |\nb: 1"), r#"{"a":"","b":1}"#);
}

// ---- document ----

#[test]
fn comments_directives_and_only_the_first_document() {
    assert_eq!(
        json(
            "%YAML 1.2\n%TAG ! tag:example.com,2000:\n# at the top\n\n---   # start\na: 1\n\n   # indented\nb: 2\n...\n---\nc: 3"
        ),
        r#"{"a":1,"b":2}"#
    );
    assert_eq!(json("--- |\n  text\n---\nother"), r#""text\n""#);
    assert_eq!(json("---\n---\na: 1"), "null");
    assert_eq!(json("a: 1\r\nb: 2\r\n"), r#"{"a":1,"b":2}"#);
    assert_eq!(json("\u{FEFF}a: 1"), r#"{"a":1}"#);
}

#[test]
fn anchors_and_aliases() {
    assert_eq!(
        json(
            "base: &b\n  x: 1\n  y: [1, 2]\ncopy: *b\nlist: [&s hello, *s]\nseq:\n  - &item {k: v}\n  - *item\nmerge:\n  <<: *b\n  z: 3"
        ),
        r#"{"base":{"x":1,"y":[1,2]},"copy":{"x":1,"y":[1,2]},"list":["hello","hello"],"seq":[{"k":"v"},{"k":"v"}],"merge":{"<<":{"x":1,"y":[1,2]},"z":3}}"#
    );
    assert_eq!(
        failure("a: *nobody").unwrap().reason,
        "unknown alias “*nobody”"
    );
}

#[test]
fn tags_are_ignored_except_str() {
    assert_eq!(
        json(
            "a: !!str 123\nb: !!int \"5\"\nc: !custom 5\nd: !!str\ne: !!str true\nf: !<tag:yaml.org,2002:str> 1\ng: !!map {x: !!str 1}\nh: [!!str 2, !foo 3]"
        ),
        r#"{"a":"123","b":"5","c":5,"d":"","e":"true","f":"1","g":{"x":"1"},"h":["2",3]}"#
    );
}

#[test]
fn keys_are_the_scalar_text() {
    assert_eq!(
        json("200: a\n'201': b\ntrue: c\nnull: d\n1.50: e\n"),
        r#"{"200":"a","201":"b","true":"c","null":"d","1.50":"e"}"#
    );
}

// ---- what stops ----

#[test]
fn what_cannot_be_read_stops() {
    let cases = [
        (
            "a:\n\tb: 1",
            "tabs are not allowed for indentation at line 2",
        ),
        ("a: 1\nb: 2\na: 3", "duplicate key “a” at line 3"),
        ("a: {x: 1, x: 2}", "duplicate key “x” at line 1"),
        (
            "a: \"not closed\nb: 2",
            "unterminated quoted scalar at line 1",
        ),
        ("a: 'not closed", "unterminated quoted scalar at line 1"),
        ("a:\n  b: 1\n c: 2", "bad indentation at line 3"),
        ("a:\n  - 1\n - 2", "bad indentation at line 3"),
        (
            "a: 1\n  b: 2",
            "mapping values are not allowed here at line 2",
        ),
        ("? a\n: b", "complex keys are not supported at line 1"),
        ("a: {? b: c}", "complex keys are not supported at line 1"),
        ("[a]: b", "a mapping key must be a scalar at line 1"),
        ("{a: 1}: b", "a mapping key must be a scalar at line 1"),
        ("a: {[x]: b}", "a mapping key must be a scalar at line 1"),
        ("a: [1, 2", "unterminated flow sequence at line 1"),
        ("a: {b: 1", "unterminated flow mapping at line 1"),
        ("a: 1\njust text", "could not find expected ':' at line 2"),
        ("a: \"\\q\"", "invalid escape “\\q” at line 1"),
        ("a: - b", "unexpected character “-” at line 1"),
        ("- a\nb: 1", "unexpected text at line 2"),
        ("a: [1] 2", "unexpected text after a value at line 1"),
    ];
    for (text, message) in cases {
        assert_eq!(
            failure(text).map(|e| e.to_string()).as_deref(),
            Some(message),
            "{text:?}"
        );
    }
}

#[test]
fn an_alias_avalanche_stops() {
    let mut text = format!("l0: &l0 [{}]\n", ["x"; 10].join(", "));
    for level in 1..=9 {
        text += &format!(
            "l{level}: &l{level} [{}]\n",
            vec![format!("*l{}", level - 1); 10].join(", ")
        );
    }
    assert!(failure(&text).unwrap().reason.starts_with("too many nodes"));
    // Below the cap, aliases are expanded.
    assert_eq!(
        doc("a: &a [1, 2]\nb: [*a, *a]")["b"],
        json!([[1, 2], [1, 2]])
    );
}

#[test]
fn too_deep_stops() {
    assert_eq!(
        failure(&format!("{}{}", "[".repeat(300), "]".repeat(300)))
            .unwrap()
            .reason,
        "nested too deeply"
    );
    // Up to the cap it reads.
    assert!(yaml::parse(&format!("{}{}", "[".repeat(256), "]".repeat(256))).is_ok());
    let nested = |levels: usize| {
        (0..levels)
            .map(|i| format!("{}k:", " ".repeat(i)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(yaml::parse(&nested(255)).is_ok());
    assert_eq!(failure(&nested(300)).unwrap().reason, "nested too deeply");
    // Also an alias that brings a deep node under another deep node.
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    let text = format!("a: &d {deep}\nb: {}*d{}", "[".repeat(100), "]".repeat(100));
    assert_eq!(failure(&text).unwrap().reason, "nested too deeply");
}

// ---- real documents ----

#[test]
fn a_swagger_2_like_petstore() {
    let d = doc(r##"swagger: '2.0'
info:
  version: 1.0.0
  title: Swagger Petstore
  description: A sample API that uses a petstore as an example
    to demonstrate features in the swagger-2.0 specification
  license:
    name: MIT
host: petstore.swagger.io
basePath: /v1
schemes:
  - http
  - https
consumes:
- application/json
produces:
- application/json
paths:
  /pets:
    get:
      summary: List all pets
      operationId: listPets
      tags:
        - pets
      parameters:
        - name: limit
          in: query
          description: How many items to return at one time (max 100)
          required: false
          type: integer
          format: int32
      responses:
        "200":
          description: An paged array of pets
          headers:
            x-next:
              type: string
              description: A link to the next page of responses
          schema:
            $ref: '#/definitions/Pets'
        default:
          description: unexpected error
          schema:
            $ref: "#/definitions/Error"
  /pets/{petId}:
    get:
      parameters:
        - {name: petId, in: path, required: true, type: string}
      responses:
        200:
          description: Expected response to a valid request
definitions:
  Pet:
    type: "object"
    required: [id, name]
    properties:
      id:
        type: integer
        format: int64
      name:
        type: string
      tag:
        type: string
        x-nullable: true
      weight: {type: number, example: 3.10, minimum: 0.5}
  Pets:
    type: array
    items:
      $ref: '#/definitions/Pet'
"##);
    assert_eq!(d["swagger"], "2.0");
    assert_eq!(d["info"]["version"], "1.0.0");
    assert_eq!(
        d["info"]["description"],
        "A sample API that uses a petstore as an example to demonstrate features in the swagger-2.0 specification"
    );
    assert_eq!(d["schemes"], json!(["http", "https"]));
    assert_eq!(d["produces"], json!(["application/json"]));
    let list = &d["paths"]["/pets"]["get"];
    assert_eq!(list["tags"], json!(["pets"]));
    assert_eq!(list["parameters"][0]["required"], false);
    assert_eq!(
        list["responses"]["200"]["schema"]["$ref"],
        "#/definitions/Pets"
    );
    assert_eq!(
        list["responses"]["default"]["schema"]["$ref"],
        "#/definitions/Error"
    );
    let one = &d["paths"]["/pets/{petId}"]["get"];
    assert_eq!(
        one["parameters"],
        json!([{"name": "petId", "in": "path", "required": true, "type": "string"}])
    );
    assert_eq!(
        one["responses"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["200"]
    );
    let pet = &d["definitions"]["Pet"];
    assert_eq!(pet["required"], json!(["id", "name"]));
    assert_eq!(pet["properties"]["tag"]["x-nullable"], true);
    assert_eq!(
        clak_wire::json::text(&pet["properties"]["weight"]),
        r#"{"type":"number","example":3.1,"minimum":0.5}"#
    );
    assert_eq!(
        d["definitions"]["Pets"]["items"]["$ref"],
        "#/definitions/Pet"
    );
}

#[test]
fn an_openapi_3() {
    let d = doc(r##"openapi: 3.0.3
info:
  title: Orders
  version: "2.1"
  description: |
    Order management.

    Two paragraphs, with `code`.
servers:
  - url: https://{region}.api.example.com/{basePath}
    description: >-
      The production server,
      chosen by region.
    variables:
      region:
        default: eu
        enum: [eu, us]
      basePath:
        default: v2
security:
  - api_key: []
  - oauth:
      - read:orders
      - write:orders
paths:
  /orders/{id}:
    parameters:
      - $ref: '#/components/parameters/Id'
    get:
      operationId: getOrder
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Order'
        '404': {description: Not found}
    delete:
      responses:
        204:
          description: Deleted
components:
  parameters:
    Id:
      name: id
      in: path
      required: true
      schema: {type: string, format: uuid}
  schemas:
    Order:
      type: object
      properties:
        total: {type: number, multipleOf: 0.01}
        status:
          type: string
          enum:
            - open
            - 'null'
            - closed
          nullable: true
  securitySchemes:
    api_key: {type: apiKey, name: X-Key, in: header}
"##);
    assert_eq!(d["openapi"], "3.0.3");
    assert_eq!(d["info"]["version"], "2.1");
    assert_eq!(
        d["info"]["description"],
        "Order management.\n\nTwo paragraphs, with `code`.\n"
    );
    let server = &d["servers"][0];
    assert_eq!(server["url"], "https://{region}.api.example.com/{basePath}");
    assert_eq!(
        server["description"],
        "The production server, chosen by region."
    );
    assert_eq!(server["variables"]["region"]["enum"], json!(["eu", "us"]));
    assert_eq!(
        d["security"],
        json!([{"api_key": []}, {"oauth": ["read:orders", "write:orders"]}])
    );
    let path = &d["paths"]["/orders/{id}"];
    assert_eq!(
        path["parameters"],
        json!([{"$ref": "#/components/parameters/Id"}])
    );
    assert_eq!(
        path["get"]["responses"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["200", "404"]
    );
    assert_eq!(
        path["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/Order"
    );
    assert_eq!(path["delete"]["responses"]["204"]["description"], "Deleted");
    let order = &d["components"]["schemas"]["Order"]["properties"];
    assert_eq!(order["total"]["multipleOf"], json!(0.01));
    assert_eq!(order["status"]["enum"], json!(["open", "null", "closed"]));
    assert_eq!(
        d["components"]["securitySchemes"]["api_key"]["in"],
        "header"
    );

    // And the resulting OpenAPI: the server variables take their value.
    let import =
        clak_apis::openapi::parse(&format!("{}\n", serde_json::to_string(&d).unwrap()), None)
            .unwrap();
    assert_eq!(
        import.base_url.as_deref(),
        Some("https://eu.api.example.com/v2")
    );
    assert_eq!(
        import.auth,
        clak_apis::ApiAuth::named(clak_apis::AuthKind::Header, "X-Key")
    );
    assert_eq!(
        import
            .endpoints
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["getOrder", "delete_orders_id"]
    );
}
