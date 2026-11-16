//! A fake MCP server over stdio, for the tests: one JSON-RPC message per line on stdin and
//! stdout, the log on stderr. Commands:
//!
//! - `echo` repeats `text`;
//! - `env` tells the value of the environment variable `name`;
//! - `grow` adds the command `new` and announces that the list has changed;
//! - `ask` asks the user (`elicitation/create`, with `params` if given, otherwise a plain
//!   "Proceed?") and tells how it went, with what the user sent; with `after_hold`, only
//!   once a `hold` has arrived;
//! - `hold` answers only after the next command has been answered; with `now`, at once;
//! - `files` the first time answers that the user must first finish in the browser
//!   (`-32042`), and a moment later says they did (`notifications/elicitation/complete`);
//!   then it lists 3 files;
//! - `pid` tells its process number;
//! - `quit` writes "bye" on stderr and exits with code 3.
//!
//! With the `--fail-start` argument it writes "missing API key" on stderr and exits right away.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn main() {
    if std::env::args().any(|a| a == "--fail-start") {
        eprintln!("starting…");
        eprintln!("missing API key");
        std::process::exit(2);
    }
    // A server that writes its log on stdout: the client skips the lines that aren't JSON.
    println!("fake stdio server ready");
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut grown = false;
    let mut next_id = 1000;
    let mut held: Vec<Value> = Vec::new();
    let mut connected = false;
    while let Some(Ok(line)) = lines.next() {
        let Ok(Value::Object(message)) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(method), Some(id)) = (
            message.get("method").and_then(Value::as_str),
            message.get("id"),
        ) else {
            continue; // notifications
        };
        let params = message.get("params").cloned().unwrap_or(json!({}));
        let result = match method {
            "initialize" => json!({
                "protocolVersion": params["protocolVersion"],
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": { "name": "fake-stdio", "version": "1" },
                "instructions": std::env::var("FAKE_INSTRUCTIONS").unwrap_or_default(),
            }),
            "tools/list" => {
                let mut tools: Vec<Value> = ["echo", "env", "grow", "ask", "hold", "files", "pid", "quit"]
                    .iter()
                    .map(|name| json!({ "name": name, "description": format!("The {name} command."), "inputSchema": { "type": "object" } }))
                    .collect();
                if grown {
                    tools.push(json!({ "name": "new", "description": "Arrived later.", "inputSchema": { "type": "object" } }));
                }
                json!({ "tools": tools })
            }
            "tools/call" => {
                let arguments = &params["arguments"];
                let text = match params["name"].as_str().unwrap_or("") {
                    "echo" => format!("echo: {}", arguments["text"].as_str().unwrap_or("")),
                    "env" => std::env::var(arguments["name"].as_str().unwrap_or(""))
                        .unwrap_or_else(|_| "(none)".into()),
                    "grow" => {
                        grown = true;
                        send(
                            &json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }),
                        );
                        "grown".into()
                    }
                    "ask" => {
                        // Two commands running, whatever order they came in.
                        while arguments["after_hold"] == json!(true) && held.is_empty() {
                            let Some(Ok(line)) = lines.next() else { break };
                            let Ok(other) = serde_json::from_str::<Value>(&line) else {
                                continue;
                            };
                            if other["params"]["name"] == "hold" {
                                held.push(other["id"].clone());
                            }
                        }
                        next_id += 1;
                        let asked = match arguments.get("params") {
                            Some(params) => params.clone(),
                            None => {
                                json!({ "message": "Proceed?", "requestedSchema": { "type": "object", "properties": {} } })
                            }
                        };
                        send(
                            &json!({ "jsonrpc": "2.0", "id": next_id, "method": "elicitation/create", "params": asked }),
                        );
                        let mut action = String::from("(no answer)");
                        while let Some(Ok(line)) = lines.next() {
                            let Ok(answer) = serde_json::from_str::<Value>(&line) else {
                                continue;
                            };
                            if answer["id"] == json!(next_id) {
                                action = match answer["result"]["action"].as_str() {
                                    Some(action) => match answer["result"].get("content") {
                                        Some(content) => format!("{action} {content}"),
                                        None => action.to_owned(),
                                    },
                                    None => "error".to_owned(),
                                };
                                break;
                            }
                        }
                        format!("action: {action}")
                    }
                    "hold" if arguments["now"] == json!(true) => "held".into(),
                    "hold" => {
                        held.push(id.clone());
                        continue;
                    }
                    "files" if !connected => {
                        connected = true;
                        std::thread::spawn(|| {
                            std::thread::sleep(std::time::Duration::from_millis(300));
                            send(
                                &json!({ "jsonrpc": "2.0", "method": "notifications/elicitation/complete", "params": { "elicitationId": "el-1" } }),
                            );
                        });
                        send(
                            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32042, "message": "This request requires more information.",
                            "data": { "elicitations": [{ "mode": "url", "elicitationId": "el-1", "url": "https://example.com/connect", "message": "Sign in to your files." }] } } }),
                        );
                        continue;
                    }
                    "files" => "3 files".into(),
                    "pid" => std::process::id().to_string(),
                    "quit" => {
                        eprintln!("bye");
                        std::process::exit(3);
                    }
                    other => {
                        send(
                            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": format!("unknown tool {other}") } }),
                        );
                        continue;
                    }
                };
                json!({ "content": [{ "type": "text", "text": text }] })
            }
            "ping" => json!({}),
            other => {
                send(
                    &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("unknown method {other}") } }),
                );
                continue;
            }
        };
        send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        if method == "tools/call" {
            for id in held.drain(..) {
                send(
                    &json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": "held" }] } }),
                );
            }
        }
    }
}

fn send(message: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}
