//! The commands, as agents see them, and the instructions `clak settings -h` prints first.

use crate::tool_def;
use clak_apis::AuthKind;
use clak_hub::ToolDef;
use serde_json::{Map, Value, json};

/// What `clak settings -h` prints before the commands: the skill stays out of it.
pub const INSTRUCTIONS: &str = concat!(
    "Clak itself: when the user asks to connect an MCP server or a REST API, or to change one, do it here instead of sending them to the panel.\n",
    "- Every change is confirmed by the user in a Clak window, and the command waits: tell them before.\n",
    "- An API with an OpenAPI document: add-api --openapi <address>. Otherwise write the endpoints in a JSON file and pass --endpoints @file.json (add-api -h shows the format).\n",
    "- Tokens and keys are never shown back. An OAuth sign-in is done by the user in the panel: the answer says when to ask them.",
);

const ENDPOINT_FORMAT: &str = concat!(
    "An endpoint is a JSON object: {\"name\": \"list_repos\", \"method\": \"GET\", \"path\": \"/users/{user}/repos\", ",
    "\"description\": \"…\", \"parameters\": [{\"name\": \"user\", \"in\": \"path\"}, {\"name\": \"per_page\", \"in\": \"query\", ",
    "\"type\": \"integer\", \"default\": \"30\"}], \"body\": \"json\"}. A parameter's \"in\" is path, query, header or body; ",
    "\"type\" is string (default), integer, number, boolean, array or object; optional \"required\", \"description\", \"default\", ",
    "\"choices\" (a list). \"body\" is json, form or absent: with json and no body parameters, the command takes the whole JSON body as --body. ",
    "The name becomes the command: `clak <api> list_repos --user x`."
);

const CONFIRMED: &str =
    " The user confirms the change in a Clak window; the command waits for the answer.";

fn s(kind: &str, description: &str) -> Value {
    json!({ "type": kind, "description": description })
}

fn tool(
    name: &str,
    description: String,
    properties: Map<String, Value>,
    required: &[&str],
) -> ToolDef {
    let mut input = Map::new();
    input.insert("type".into(), "object".into());
    input.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        input.insert("required".into(), json!(required));
    }
    tool_def(json!({ "name": name, "description": description, "input_schema": input }))
}

fn props(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// The access fields, appended to those of add-api and edit-api.
fn with_auth(mut properties: Map<String, Value>) -> Map<String, Value> {
    let kinds: Vec<&str> = AuthKind::ALL.iter().map(|k| k.as_str()).collect();
    properties.insert(
        "auth".into(),
        json!({
            "type": "string",
            "enum": kinds,
            "description": concat!(
                "How the API recognizes the caller: none; bearer (Authorization: Bearer <secret>); header or query (a key, ",
                "named by --auth-name); basic (user --auth-name, password = the secret); oauth (sign-in in the browser) or ",
                "oauth-app (client credentials): the user signs in from the panel."
            ),
        }),
    );
    properties.insert(
        "auth_name".into(),
        s(
            "string",
            "The header or query parameter that carries the key, or the user name for basic.",
        ),
    );
    properties.insert(
        "authorization_url".into(),
        s(
            "string",
            "OAuth: the sign-in (authorization) address, https.",
        ),
    );
    properties.insert(
        "token_url".into(),
        s("string", "OAuth: the token address, https."),
    );
    properties.insert(
        "scope".into(),
        s("string", "OAuth: the scopes, separated by spaces."),
    );
    properties.insert(
        "client_id".into(),
        s(
            "string",
            "OAuth: the client ID of the app the user registered with the service.",
        ),
    );
    properties
}

/// The commands of `clak settings`.
pub fn tools() -> Vec<ToolDef> {
    vec![
        tool(
            "servers",
            "The MCP servers Clak reaches: name, command (clak <command>), address or program, state, access, number of commands. Tokens and environment values are never shown.".into(),
            Map::new(),
            &[],
        ),
        tool(
            "apis",
            "The REST APIs Clak calls: name, command (clak <command>), base URL, state, access, number of endpoints. Secrets are never shown.".into(),
            Map::new(),
            &[],
        ),
        tool(
            "show-api",
            "One REST API in full: its access settings and every endpoint with its parameters, in the JSON that edit-api and set-endpoint accept.".into(),
            props(&[("api", s("string", "Which API: its command, its name or its id."))]),
            &["api"],
        ),
        tool(
            "add-server",
            format!(
                "Add an MCP server: at an address (--url, Streamable HTTP), or a program on this computer that Clak starts and talks to over stdio (--stdio, the command line from the server's docs, e.g. \"npx -y @scope/server --flag\"). Its commands become `clak <command> …` for the agents the user allows.{CONFIRMED} If an HTTP server wants an OAuth sign-in, the user does it from the panel."
            ),
            props(&[
                ("name", s("string", "What to call it, e.g. Notion.")),
                ("url", s("string", "The server's address, e.g. https://mcp.example.com/mcp.")),
                (
                    "stdio",
                    s(
                        "string",
                        "Instead of --url: the program and its arguments, as one command line, e.g. \"agent-browser mcp --tools core,debug\". Quote an argument with spaces.",
                    ),
                ),
                (
                    "env",
                    s(
                        "object",
                        "With --stdio: environment variables for the program, a JSON object, e.g. {\"API_KEY\": \"…\"}. Saved encrypted, never shown again.",
                    ),
                ),
                (
                    "command",
                    s(
                        "string",
                        "The name on the command line, e.g. notion. Lowercase letters, digits, hyphens; empty: made from the name.",
                    ),
                ),
                (
                    "token",
                    s("string", "A bearer token, if the server wants one. It's saved encrypted and never shown again."),
                ),
            ]),
            &["name"],
        ),
        tool(
            "edit-server",
            format!(
                "Change an MCP server: name, address or program, command, token or environment, on or off. Only what you pass changes.{CONFIRMED} A new address on another host leaves the saved token or sign-in behind. An address can't become a program or the other way round: remove the server and add it again."
            ),
            props(&[
                ("server", s("string", "Which server: its command, its name or its id (see servers).")),
                ("name", s("string", "A new name.")),
                ("url", s("string", "A new address.")),
                ("stdio", s("string", "A new command line, for a server that is a program.")),
                (
                    "env",
                    s("object", "New environment variables for a program, replacing all the saved ones: a JSON object."),
                ),
                ("remove_env", s("boolean", "Remove the saved environment variables of a program.")),
                ("command", s("string", "A new command name; empty: back to the automatic one.")),
                ("token", s("string", "A new bearer token.")),
                ("remove_token", s("boolean", "Remove the saved token or sign-in.")),
                (
                    "enabled",
                    s("boolean", "false turns it off (agents no longer see it), true turns it back on."),
                ),
            ]),
            &["server"],
        ),
        tool(
            "remove-server",
            format!("Remove an MCP server, with its token, sign-in or environment variables.{CONFIRMED}"),
            props(&[("server", s("string", "Which server: its command, its name or its id."))]),
            &["server"],
        ),
        tool(
            "add-api",
            format!(
                "Add a REST API: each endpoint becomes a command, `clak <command> <endpoint> …`. Either from an OpenAPI 3 or Swagger 2 document (--openapi; name, base URL, access and endpoints come from it, and what you pass overrides them), or by hand (--name, --base-url, --endpoints).{CONFIRMED} {ENDPOINT_FORMAT}"
            ),
            with_auth(props(&[
                ("openapi", s("string", "The address (or the file) of an OpenAPI document, JSON or YAML.")),
                (
                    "only",
                    json!({
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "With --openapi: keep only these endpoints (their command names); the others are left out.",
                    }),
                ),
                ("name", s("string", "What to call it, e.g. GitHub.")),
                (
                    "base_url",
                    s("string", "The address the endpoint paths are added to, e.g. https://api.github.com."),
                ),
                ("command", s("string", "The name on the command line, e.g. github; empty: made from the name.")),
                (
                    "secret",
                    s(
                        "string",
                        "The token, key or password for bearer, header, query or basic. Saved encrypted, never shown again.",
                    ),
                ),
                (
                    "endpoints",
                    json!({
                        "type": "array",
                        "items": { "type": "object" },
                        "description": "By hand: the endpoints, a JSON list (see above). From a file: --endpoints @endpoints.json.",
                    }),
                ),
            ])),
            &[],
        ),
        tool(
            "edit-api",
            format!(
                "Change a REST API: name, base URL, command, access, secret, endpoints, on or off. Only what you pass changes; --endpoints replaces them all (for one, use set-endpoint).{CONFIRMED} A base URL on another host, or a change to OAuth, leaves the saved secret or sign-in behind."
            ),
            with_auth(props(&[
                ("api", s("string", "Which API: its command, its name or its id (see apis).")),
                ("name", s("string", "A new name.")),
                ("base_url", s("string", "A new base URL.")),
                ("command", s("string", "A new command name; empty: back to the automatic one.")),
                ("secret", s("string", "A new token, key or password.")),
                ("remove_secret", s("boolean", "Remove the saved secret or sign-in.")),
                (
                    "endpoints",
                    json!({
                        "type": "array",
                        "items": { "type": "object" },
                        "description": "All the endpoints, replacing the current ones (show-api prints them in this format).",
                    }),
                ),
                (
                    "enabled",
                    s("boolean", "false turns it off (agents no longer see it), true turns it back on."),
                ),
            ])),
            &["api"],
        ),
        tool(
            "set-endpoint",
            format!("Add an endpoint to a REST API, or replace the one with the same name.{CONFIRMED} {ENDPOINT_FORMAT}"),
            props(&[
                ("api", s("string", "Which API: its command, its name or its id.")),
                ("endpoint", s("object", "The endpoint, a JSON object.")),
            ]),
            &["api", "endpoint"],
        ),
        tool(
            "remove-endpoint",
            format!("Remove an endpoint from a REST API.{CONFIRMED}"),
            props(&[
                ("api", s("string", "Which API: its command, its name or its id.")),
                ("name", s("string", "The endpoint's name (its command).")),
            ]),
            &["api", "name"],
        ),
        tool(
            "remove-api",
            format!("Remove a REST API, with its secret or sign-in.{CONFIRMED}"),
            props(&[("api", s("string", "Which API: its command, its name or its id."))]),
            &["api"],
        ),
    ]
}
