//! The MCP servers of Claude Desktop, Cursor and VS Code, read to be added to Clak: programs
//! and addresses, the token when it is a Bearer, Clak's own entry left out, what can't be
//! taken as it is said why, and the values never shown to the panel.

mod common;

use clak_setup::clients::import::{self, Server};
use common::Sandbox;
use std::collections::BTreeMap;

#[cfg(windows)]
const DESKTOP: &str = "appdata/Claude/claude_desktop_config.json";
#[cfg(not(windows))]
const DESKTOP: &str = "home/Library/Application Support/Claude/claude_desktop_config.json";
#[cfg(windows)]
const VSCODE: &str = "appdata/Code/User/mcp.json";
#[cfg(not(windows))]
const VSCODE: &str = "home/Library/Application Support/Code/User/mcp.json";
const CURSOR: &str = "home/.cursor/mcp.json";

#[test]
fn nothing_configured_finds_nothing() {
    let sandbox = Sandbox::new();
    assert!(import::found(&sandbox.places).is_empty());
}

#[test]
fn programs_and_addresses_from_the_three_apps_without_clak() {
    let sandbox = Sandbox::new();
    sandbox.write(
        DESKTOP,
        r#"{ "mcpServers": {
            "clak": { "command": "/Applications/Clak.app/Contents/MacOS/clak-shim" },
            "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/work"],
                       "env": { "DEBUG": "1" } }
        }, "preferences": {} }"#,
    );
    sandbox.write(
        CURSOR,
        r#"{ "mcpServers": {
            "linear": { "url": "https://mcp.linear.app/mcp", "headers": { "Authorization": "Bearer sk-123" } },
            "shim": { "command": "C:\\Program Files\\Clak\\clak-shim.exe" }
        } }"#,
    );
    sandbox.write(
        VSCODE,
        "{\n  // My servers\n  \"servers\": {\n    \"github\": { \"type\": \"http\", \"url\": \"https://api.githubcopilot.com/mcp/\" },\n  },\n}\n",
    );
    let all = import::found(&sandbox.places);
    let keys: Vec<String> = all.iter().map(|f| f.key()).collect();
    assert_eq!(
        keys,
        ["Claude Desktop/files", "Cursor/linear", "VS Code/github"]
    );
    assert_eq!(
        all[0].server,
        Server::Program {
            program: "npx".into(),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
                "/work".into()
            ],
            env: BTreeMap::from([("DEBUG".to_owned(), "1".to_owned())]),
        }
    );
    assert_eq!(
        all[1].server,
        Server::Address {
            url: "https://mcp.linear.app/mcp".into(),
            token: Some("sk-123".into()),
        }
    );
    assert!(all.iter().all(|f| f.problem.is_none()));
}

#[test]
fn the_panel_sees_the_names_not_the_values() {
    let sandbox = Sandbox::new();
    sandbox.write(
        CURSOR,
        r#"{ "mcpServers": {
            "db": { "command": "db-mcp", "env": { "DB_PASSWORD": "hunter2" } },
            "api": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer secret-token" } }
        } }"#,
    );
    let text = import::found(&sandbox.places)
        .iter()
        .map(|f| f.to_json().to_string())
        .collect::<String>();
    assert!(text.contains("DB_PASSWORD"));
    assert!(!text.contains("hunter2"));
    assert!(!text.contains("secret-token"));
}

#[test]
fn what_clak_cant_take_as_it_is_says_why() {
    let sandbox = Sandbox::new();
    sandbox.write(
        VSCODE,
        r#"{ "inputs": [{ "id": "token", "type": "promptString" }], "servers": {
            "asks": { "type": "stdio", "command": "x-mcp", "env": { "TOKEN": "${input:token}" } },
            "header": { "type": "http", "url": "https://example.com/mcp", "headers": { "X-Api-Key": "k" } },
            "file": { "type": "stdio", "command": "y-mcp", "envFile": "${workspaceFolder}/.env" },
            "fine": { "type": "stdio", "command": "z-mcp" }
        } }"#,
    );
    let all = import::found(&sandbox.places);
    let problem = |name: &str| all.iter().find(|f| f.name == name).unwrap().problem.clone();
    assert!(problem("asks").unwrap().contains("${…}"));
    assert!(problem("header").unwrap().contains("X-Api-Key"));
    assert!(problem("file").unwrap().contains("file"));
    assert_eq!(problem("fine"), None);
}

#[test]
fn a_file_that_isnt_json_is_skipped() {
    let sandbox = Sandbox::new();
    sandbox.write(CURSOR, "{ not json");
    sandbox.write(
        DESKTOP,
        r#"{ "mcpServers": { "a": { "command": "a-mcp" } } }"#,
    );
    let keys: Vec<String> = import::found(&sandbox.places)
        .iter()
        .map(|f| f.key())
        .collect();
    assert_eq!(keys, ["Claude Desktop/a"]);
}
