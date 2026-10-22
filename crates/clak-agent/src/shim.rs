//! The shim: the MCP server for clients that have no shell (Claude Desktop, the chats).
//! It does the same three things as the command line, with three tools that never
//! change — so the tool list doesn't shift under the model's feet, and the prompt cache
//! isn't lost.
//!
//! ```text
//!     programs                          the connected programs
//!     commands {program, search?}       a program's commands, with their parameters
//!     run      {program, command, arguments}
//! ```
//!
//! It decides nothing and asks nothing: every request goes to the hub, which knows who is
//! behind it (the client that launched this process) and asks the user when needed.
//!
//! It speaks both MCP eras: the one with the handshake (`initialize`) and the 2026-07-28
//! one, in which every request carries its own version and capabilities.

use crate::clock::Local;
use crate::hub::{self, HubUnavailable};
use crate::render::{self, Style, Which};
use crate::util::{Object, flag, object_of, str_of};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const SERVER_NAME: &str = "clak";
const SERVER_VERSION: &str = "0.3.0";
const MODERN: &str = "2026-07-28";
const HANDSHAKE: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";
const META_SUBSCRIPTION: &str = "io.modelcontextprotocol/subscriptionId";

/// How long to wait for the hub, including the user's answer to one of its questions.
const WAIT_SECONDS: f64 = 110.0;
const MAX_WAIT_SECONDS: f64 = 3600.0;

/// The same rule as the skill, in a few lines.
pub const INSTRUCTIONS: &str = "The programs and services the user has connected to Clak. Whenever the user names a program or service, \
or asks for something done in a program or on a file you cannot edit directly, call `programs` before anything else. \
If the program is there: `commands` (its instructions come first), then `run`. If it is missing, or lacks what you need, \
do not look for other ways: tell the user what is missing and how to proceed (a program on the computer: open it and \
connect it to Clak from inside it; a network service: connect its MCP server or REST API, which you can do with the \
program `settings` once they agree), then ask whether to proceed another way. The first use of a program asks the user \
for permission in a Clak window, and the call waits: tell them before. If they refuse, do not retry until they ask.";

/// This process's shim: stdin and stdout are the MCP channel, everything else goes to stderr.
pub fn main() {
    let stdin = std::io::stdin();
    Shim::new().run(stdin.lock(), std::io::stdout());
}

fn log(text: &str) {
    eprintln!("[shim {}] {text}", Local::now().clock());
}

/// The MCP server.
pub struct Shim {
    home: Option<PathBuf>,
}

impl Default for Shim {
    fn default() -> Self {
        Shim::new()
    }
}

/// What the call threads share: where to write, and how many calls are in flight.
struct Shared {
    home: Option<PathBuf>,
    // Stdout is the MCP channel: one line per message, written in one go.
    output: Mutex<Box<dyn Write + Send>>,
    calls: Mutex<usize>,
    done: Condvar,
}

impl Shim {
    pub fn new() -> Shim {
        Shim { home: None }
    }

    /// A shim that talks to the hub of a given folder, not the user's.
    pub fn with_home(home: PathBuf) -> Shim {
        Shim { home: Some(home) }
    }

    /// Reads messages until the client closes; replies already in flight go out, waited
    /// for at most one second, then it exits.
    pub fn run(&self, input: impl BufRead, output: impl Write + Send + 'static) {
        let shared = Arc::new(Shared {
            home: self.home.clone(),
            output: Mutex::new(Box::new(output)),
            calls: Mutex::new(0),
            done: Condvar::new(),
        });
        for line in input.split(b'\n') {
            let Ok(line) = line else { break };
            let line = line.strip_suffix(b"\r").unwrap_or(&line);
            let Some(message) = clak_wire::json::object(line) else {
                continue;
            };
            let handled =
                std::panic::catch_unwind(AssertUnwindSafe(|| dispatch(&shared, &message)));
            if handled.is_err() {
                log("unhandled message");
                if let (Some(id), true) = (request_id(&message), message.contains_key("method")) {
                    fail(&shared, id, -32603, "internal error");
                }
            }
        }
        if let Ok(calls) = shared.calls.lock() {
            let _ = shared
                .done
                .wait_timeout_while(calls, Duration::from_secs(1), |n| *n > 0);
        }
        if let Ok(mut output) = shared.output.lock() {
            let _ = output.flush();
        }
    }
}

fn request_id(message: &Object) -> Option<Value> {
    message.get("id").filter(|id| !id.is_null()).cloned()
}

fn send(shared: &Shared, message: Value) {
    let line = clak_wire::json::encode_line(&message);
    if let Ok(mut output) = shared.output.lock() {
        // The client has gone: there is no one to tell.
        let _ = output.write_all(&line).and_then(|()| output.flush());
    }
}

fn reply(shared: &Shared, id: Value, result: Object) {
    send(
        shared,
        json!({"jsonrpc": "2.0", "id": id, "result": result}),
    );
}

fn fail(shared: &Shared, id: Value, code: i64, message: &str) {
    send(
        shared,
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    );
}

fn dispatch(shared: &Arc<Shared>, message: &Object) {
    // A response: this server never asks the client anything.
    let Some(method) = str_of(message, "method") else {
        return;
    };
    let id = request_id(message);
    let parameters = object_of(message, "params").cloned().unwrap_or_default();
    let modern =
        object_of(&parameters, "_meta").and_then(|m| str_of(m, META_VERSION)) == Some(MODERN);

    match (method, id) {
        ("initialize", Some(id)) => {
            let asked = str_of(&parameters, "protocolVersion")
                .filter(|v| HANDSHAKE.contains(v))
                .unwrap_or(HANDSHAKE[0]);
            reply(
                shared,
                id,
                object(json!({
                    "protocolVersion": asked,
                    "capabilities": capabilities(),
                    "serverInfo": server_info(),
                    "instructions": INSTRUCTIONS,
                })),
            );
        }
        ("server/discover", Some(id)) => {
            let result = object(json!({
                "capabilities": capabilities(),
                "instructions": INSTRUCTIONS,
                "supportedVersions": [MODERN],
            }));
            reply(shared, id, envelope(result, true));
        }
        ("ping", Some(id)) => reply(shared, id, envelope(Object::new(), modern)),
        ("tools/list", Some(id)) => reply(
            shared,
            id,
            envelope(object(json!({"tools": tools()})), modern),
        ),
        ("tools/call", Some(id)) => {
            // On its own thread: a call waiting for the user must not stop the rest.
            if let Ok(mut calls) = shared.calls.lock() {
                *calls += 1;
            }
            let shared = Arc::clone(shared);
            std::thread::spawn(move || {
                let called = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    call(shared.home.as_deref(), &parameters)
                }));
                match called {
                    Ok(result) => reply(&shared, id, envelope(result, modern)),
                    Err(_) => {
                        log("call failed");
                        fail(&shared, id, -32603, "internal error");
                    }
                }
                if let Ok(mut calls) = shared.calls.lock() {
                    *calls -= 1;
                }
                shared.done.notify_all();
            });
        }
        // A listener for changes: nothing ever changes here, but the subscription must be acknowledged.
        ("subscriptions/listen", Some(id)) => send(
            shared,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/subscriptions/acknowledged",
                "params": {"_meta": {META_SUBSCRIPTION: id}, "notifications": {}},
            }),
        ),
        ("notifications/initialized", _) => {}
        (_, Some(id)) => fail(shared, id, -32601, &format!("Method not found: {method}")),
        (_, None) => {}
    }
}

fn object(value: Value) -> Object {
    match value {
        Value::Object(map) => map,
        _ => Object::new(),
    }
}

fn capabilities() -> Value {
    json!({"tools": {"listChanged": false}})
}

fn server_info() -> Value {
    json!({"name": SERVER_NAME, "version": SERVER_VERSION})
}

/// In 2026-07-28 every result says what type it is, and who is responding.
fn envelope(mut result: Object, modern: bool) -> Object {
    if !modern {
        return result;
    }
    result
        .entry("resultType")
        .or_insert_with(|| "complete".into());
    result.insert("_meta".into(), json!({META_SERVER: server_info()}));
    result
}

fn text(kind: &str, description: &str) -> Value {
    json!({"type": kind, "description": description})
}

/// Always the same three: the tool list never changes.
pub fn tools() -> Value {
    json!([
        {
            "name": "programs",
            "description": "Lists the programs open on the user's computer that are connected to Clak (documents, \
                            spreadsheets, applications), with the name to use them by: by default those you can already use, \
                            and how many others there are. The first time, Clak asks the user for permission to connect this \
                            conversation: the call waits for their answer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "which": {
                        "type": "string",
                        "enum": ["usable", "others", "all"],
                        "description": "usable (default): the programs you can use without asking the user; others: the \
                                        other connected ones, whose first command asks the user; all: both.",
                    },
                },
            },
        },
        {
            "name": "commands",
            "description": "How to use a connected program: its instructions, if it has any, and its commands with their parameters (JSON schema).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "program": text("string", "The program's name, as given by `programs`."),
                    "search": text("string", "Optional: only the commands that mention this."),
                },
                "required": ["program"],
            },
        },
        {
            "name": "run",
            "description": "Runs a command of a connected program. The first time you use a program, and for a new or changed \
                            command, Clak asks the user for permission: the call waits for the answer. If \
                            the user refuses, do not retry until they ask you to.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "program": text("string", "The program's name, as given by `programs`."),
                    "command": text("string", "The command's name, as given by `commands`."),
                    "arguments": {
                        "type": "object",
                        "description": "The command's arguments, according to its schema.",
                    },
                    "wait_seconds": {
                        "type": "integer",
                        "description": format!(
                            "How long to wait for the answer, including the user's answer to a permission request. \
                             Default {}; longer only for a slow command, such as a render.",
                            WAIT_SECONDS as i64
                        ),
                    },
                },
                "required": ["program", "command"],
            },
        },
    ])
}

fn result(text: &str, is_error: bool) -> Object {
    object(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
}

/// A call to one of the three tools: the request to the hub, the response written for the model.
fn call(home: Option<&std::path::Path>, parameters: &Object) -> Object {
    let name = str_of(parameters, "name");
    let arguments = object_of(parameters, "arguments")
        .cloned()
        .unwrap_or_default();
    let mut request = Object::new();
    request.insert("via".into(), "mcp".into());
    let mut wait = WAIT_SECONDS;
    match name {
        Some("programs") => {
            request.insert("op".into(), "list".into());
        }
        Some("commands") => {
            request.insert("op".into(), "tools".into());
            request.insert(
                "resource".into(),
                arguments.get("program").cloned().unwrap_or(Value::Null),
            );
            if let Some(query) = str_of(&arguments, "search").filter(|q| !q.is_empty()) {
                request.insert("query".into(), query.into());
            }
        }
        Some("run") => {
            request.insert("op".into(), "call".into());
            request.insert(
                "resource".into(),
                arguments.get("program").cloned().unwrap_or(Value::Null),
            );
            request.insert(
                "tool".into(),
                arguments.get("command").cloned().unwrap_or(Value::Null),
            );
            let given = object_of(&arguments, "arguments")
                .cloned()
                .unwrap_or_default();
            request.insert("arguments".into(), Value::Object(given));
            if let Some(asked) = arguments
                .get("wait_seconds")
                .and_then(Value::as_f64)
                .filter(|s| *s > 0.0)
            {
                wait = asked.min(MAX_WAIT_SECONDS);
            }
        }
        _ => {
            return result(
                &format!(
                    "There is no tool '{}': the tools are programs, commands and run.",
                    name.unwrap_or("")
                ),
                true,
            );
        }
    }

    // The waiting lines are not shown: the model doesn't read the shim's stderr.
    // `ask` reads them anyway, and if time runs out it says what was being waited for.
    let home = home
        .map(PathBuf::from)
        .unwrap_or_else(clak_wire::location::home);
    let response = match hub::ask(request, hub::seconds(wait), &home, |_| {}) {
        Ok(response) => response,
        Err(HubUnavailable(message)) => return result(&message, true),
    };
    if !flag(&response, "ok") {
        return result(str_of(&response, "error").unwrap_or("unknown error"), true);
    }

    let is_error = flag(&response, "is_error");
    match name {
        Some("programs") => {
            let which = match str_of(&arguments, "which") {
                Some("others") => Which::Others,
                Some("all") => Which::All,
                _ => Which::Usable,
            };
            result(&render::programs(&response, Style::Mcp, which), false)
        }
        Some("commands") => result(&render::commands(&response, Style::Mcp, false), false),
        // Text and image blocks pass through as they are: MCP already knows them.
        _ => match render::blocks(response.get("content")) {
            Some(blocks) => {
                let content: Vec<Value> = blocks
                    .into_iter()
                    .map(|b| Value::Object(b.clone()))
                    .collect();
                object(json!({"content": content, "isError": is_error}))
            }
            None => result(&render::result(response.get("content")), is_error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_hub::FakeHub;
    use std::io::Cursor;

    /// A stdout to read afterwards.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs the shim on some lines, and returns the replies in order of arrival.
    fn talk(shim: &Shim, lines: &[Value]) -> Vec<Value> {
        let input: String = lines
            .iter()
            .map(|l| format!("{}\n", clak_wire::json::text(l)))
            .collect();
        let sink = Sink::default();
        shim.run(Cursor::new(input.into_bytes()), sink.clone());
        let bytes = sink.0.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|l| clak_wire::json::parse_str(l).unwrap())
            .collect()
    }

    fn by_id(replies: &[Value], id: i64) -> Value {
        replies
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .unwrap_or(Value::Null)
    }

    #[test]
    fn the_handshake_and_the_tool_list() {
        let replies = talk(
            &Shim::new(),
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
                json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}),
                json!({"jsonrpc": "2.0", "id": 4, "method": "nonexistent"}),
                json!({"jsonrpc": "2.0", "id": 5, "result": {}}),
            ],
        );
        assert_eq!(replies.len(), 4);
        let init = by_id(&replies, 1);
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(
            init["result"]["serverInfo"],
            json!({"name": "clak", "version": "0.3.0"})
        );
        assert_eq!(
            init["result"]["capabilities"],
            json!({"tools": {"listChanged": false}})
        );
        assert_eq!(init["result"]["instructions"], INSTRUCTIONS);
        let listed = by_id(&replies, 2);
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["programs", "commands", "run"]);
        assert!(by_id(&replies, 2)["result"].get("_meta").is_none());
        assert_eq!(
            by_id(&replies, 3)["result"]["protocolVersion"],
            "2025-11-25"
        );
        assert_eq!(
            by_id(&replies, 4)["error"],
            json!({"code": -32601, "message": "Method not found: nonexistent"})
        );
    }

    #[test]
    fn the_modern_protocol_brings_the_envelope() {
        let meta = json!({"_meta": {META_VERSION: MODERN}});
        let replies = talk(
            &Shim::new(),
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "ping", "params": meta}),
                json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": meta}),
                json!({"jsonrpc": "2.0", "id": 4, "method": "subscriptions/listen", "params": meta}),
            ],
        );
        let discover = &by_id(&replies, 1)["result"];
        assert_eq!(discover["supportedVersions"], json!([MODERN]));
        assert_eq!(discover["resultType"], "complete");
        assert_eq!(discover["_meta"][META_SERVER]["name"], "clak");
        assert_eq!(
            by_id(&replies, 2)["result"],
            json!({"resultType": "complete", "_meta": {META_SERVER: server_info()}})
        );
        assert_eq!(by_id(&replies, 3)["result"]["resultType"], "complete");
        let acknowledged = replies
            .iter()
            .find(|r| r["method"] == "notifications/subscriptions/acknowledged")
            .unwrap();
        assert_eq!(acknowledged["params"]["_meta"][META_SUBSCRIPTION], 4);
    }

    #[test]
    fn tools_ask_the_hub_and_answer_for_the_model() {
        let hub = FakeHub::new(vec![FakeHub::response(json!({
            "ok": true, "content": [{"type": "text", "text": "done"}, {"type": "image", "data": "AA==", "mimeType": "image/png"}],
        }))]);
        let replies = talk(
            &Shim::with_home(hub.home.clone()),
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "run", "arguments": {
                    "program": "revit", "command": "capture", "arguments": {"view": 3}, "wait_seconds": 5}}}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "other"}}),
            ],
        );
        let run = &by_id(&replies, 1)["result"];
        assert_eq!(run["isError"], false);
        assert_eq!(run["content"][1]["mimeType"], "image/png");
        let request = &hub.requests()[0];
        assert_eq!(request["op"], "call");
        assert_eq!(request["via"], "mcp");
        assert_eq!(request["resource"], "revit");
        assert_eq!(request["tool"], "capture");
        assert_eq!(request["arguments"], json!({"view": 3}));
        let other = &by_id(&replies, 2)["result"];
        assert_eq!(other["isError"], true);
        assert_eq!(
            other["content"][0]["text"],
            "There is no tool 'other': the tools are programs, commands and run."
        );
    }

    #[test]
    fn a_hub_error_is_a_result_with_is_error() {
        let hub = FakeHub::new(vec![FakeHub::response(
            json!({"ok": false, "code": "denied", "error": "The user refused."}),
        )]);
        let replies = talk(
            &Shim::with_home(hub.home.clone()),
            &[
                json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "programs"}}),
            ],
        );
        assert_eq!(
            by_id(&replies, 7)["result"],
            json!({"content": [{"type": "text", "text": "The user refused."}], "isError": true})
        );
    }
}
