//! A fake MCP server over stdio, for the tests: one JSON-RPC message per line on stdin and
//! stdout, the log on stderr. Commands:
//!
//! - `echo` repeats `text`;
//! - `env` tells the value of the environment variable `name`;
//! - `grow` adds the command `new` and announces that the list has changed;
//! - `ask` asks the user for a confirmation (`elicitation/create`) and tells how it went;
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
                let mut tools: Vec<Value> = ["echo", "env", "grow", "ask", "pid", "quit"]
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
                        next_id += 1;
                        send(
                            &json!({ "jsonrpc": "2.0", "id": next_id, "method": "elicitation/create",
                            "params": { "message": "Proceed?", "requestedSchema": { "type": "object", "properties": {} } } }),
                        );
                        let mut action = String::from("(no answer)");
                        while let Some(Ok(line)) = lines.next() {
                            let Ok(answer) = serde_json::from_str::<Value>(&line) else {
                                continue;
                            };
                            if answer["id"] == json!(next_id) {
                                action = answer["result"]["action"]
                                    .as_str()
                                    .unwrap_or("?")
                                    .to_owned();
                                break;
                            }
                        }
                        format!("action: {action}")
                    }
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
    }
}

fn send(message: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}
