//! The MCP servers other apps already have (Claude Desktop, Cursor, VS Code), to add them to
//! Clak in one go instead of typing them again.
//!
//! Their files are only read. A server is a program to start (its command line and its
//! environment variables) or an address, with its token when the app sends one as
//! `Authorization: Bearer`. What Clak can't take as it is (a `${input:…}` VS Code asks for
//! when it starts, a header other than the token, an environment file) comes with the reason:
//! that one is added by hand.
//!
//! The values of the variables and the tokens stay on this side: the panel sees only their
//! names, and asks to import a server by its key.

use super::SERVER;
use super::catalog::{cursor_file, desktop_files, vscode_file};
use crate::Places;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a server is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Server {
    /// A program on this computer, talked to over stdio.
    Program {
        program: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    /// An address, with the token sent as `Authorization: Bearer`.
    Address { url: String, token: Option<String> },
}

/// A server found in another app's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The app: "Cursor".
    pub source: String,
    /// The name it has there.
    pub name: String,
    pub server: Server,
    /// Why Clak can't take it as it is.
    pub problem: Option<String>,
}

impl Found {
    /// How the panel asks for it.
    pub fn key(&self) -> String {
        format!("{}/{}", self.source, self.name)
    }

    /// For the panel: never the values of the variables, nor the token.
    pub fn to_json(&self) -> Value {
        let mut json = json!({
            "key": self.key(),
            "source": self.source,
            "name": self.name,
            "problem": self.problem,
        });
        match &self.server {
            Server::Program { program, args, env } => {
                json["transport"] = json!("stdio");
                json["program"] = json!(program);
                json["args"] = json!(args);
                json["env"] = json!(env.keys().collect::<Vec<_>>());
            }
            Server::Address { url, token } => {
                json["transport"] = json!("http");
                json["url"] = json!(url);
                json["token"] = json!(token.is_some());
            }
        }
        json
    }
}

/// The servers the other apps have, app by app, in the order of their files. Clak's own
/// entry is left out.
pub fn found(places: &Places) -> Vec<Found> {
    let sources: [(&str, Vec<PathBuf>, &str); 3] = [
        ("Claude Desktop", desktop_files(places), "mcpServers"),
        ("Cursor", vec![cursor_file(places)], "mcpServers"),
        ("VS Code", vec![vscode_file(places)], "servers"),
    ];
    let mut all = Vec::new();
    for (source, files, section) in sources {
        let Some(file) = files.iter().find(|f| f.is_file()) else {
            continue;
        };
        let Some(servers) = read(file)
            .and_then(|config| config.get(section).cloned())
            .and_then(|s| s.as_object().cloned())
        else {
            continue;
        };
        for (name, entry) in servers {
            if let Some(found) = entry
                .as_object()
                .and_then(|entry| server(source, &name, entry))
            {
                all.push(found);
            }
        }
    }
    all
}

/// One server, or `None` if it is Clak itself or nothing Clak can tell.
fn server(source: &str, name: &str, entry: &Map<String, Value>) -> Option<Found> {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::trim);
    let kind = text("type").unwrap_or("");
    let url = text("url")
        .or_else(|| text("serverUrl"))
        .filter(|u| !u.is_empty());
    let command = text("command").filter(|c| !c.is_empty());
    let mut problems = Vec::new();
    let server = match (command, url) {
        (Some(program), _) if kind.is_empty() || kind == "stdio" => {
            if name == SERVER || is_clak(program) {
                return None;
            }
            let args = match entry.get("args") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|a| match a {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect(),
                Some(_) => {
                    problems.push("Its arguments aren't a list.".to_owned());
                    Vec::new()
                }
            };
            let env = strings(entry.get("env"), "environment variable", &mut problems);
            if entry.contains_key("envFile") {
                problems.push("It reads its variables from a file.".to_owned());
            }
            Server::Program {
                program: program.to_owned(),
                args,
                env,
            }
        }
        (_, Some(url)) => {
            let headers = strings(entry.get("headers"), "header", &mut problems);
            let mut token = None;
            for (header, value) in &headers {
                let bearer = value
                    .strip_prefix("Bearer ")
                    .or_else(|| value.strip_prefix("bearer "));
                match bearer {
                    Some(t) if header.eq_ignore_ascii_case("authorization") => {
                        token = Some(t.trim().to_owned());
                    }
                    _ => problems.push(format!(
                        "It sends the header {header}, which Clak doesn't send."
                    )),
                }
            }
            Server::Address {
                url: url.to_owned(),
                token,
            }
        }
        _ => return None,
    };
    if mentions_variable(&server) {
        problems.push(format!(
            "It uses values {source} fills in when it starts (${{…}})."
        ));
    }
    Some(Found {
        source: source.to_owned(),
        name: name.to_owned(),
        server,
        problem: problems.into_iter().next(),
    })
}

/// An object of texts: the variables, the headers.
fn strings(
    value: Option<&Value>,
    what: &str,
    problems: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let mut all = BTreeMap::new();
    let Some(object) = value.and_then(Value::as_object) else {
        return all;
    };
    for (key, value) in object {
        match value {
            Value::String(s) => {
                all.insert(key.clone(), s.clone());
            }
            Value::Number(_) | Value::Bool(_) => {
                all.insert(key.clone(), value.to_string());
            }
            _ => problems.push(format!("The {what} {key} isn't a text.")),
        }
    }
    all
}

/// `${input:token}`, `${env:HOME}`, `${workspaceFolder}`: filled in by the app, not by Clak.
fn mentions_variable(server: &Server) -> bool {
    let has = |s: &str| s.contains("${");
    match server {
        Server::Program { program, args, env } => {
            has(program) || args.iter().any(|a| has(a)) || env.values().any(|v| has(v))
        }
        Server::Address { url, token } => has(url) || token.as_deref().is_some_and(has),
    }
}

/// The shim that brings Clak into the app.
fn is_clak(program: &str) -> bool {
    let file = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let stem = file.strip_suffix(".exe").unwrap_or(file);
    stem.eq_ignore_ascii_case("clak-shim")
}

/// A configuration, also with the comments and the trailing commas VS Code allows.
fn read(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    serde_json::from_str(text)
        .ok()
        .or_else(|| serde_json::from_str(&without_comments(text)).ok())
}

/// The text without `//` and `/* */` comments and without the commas before `}` or `]`,
/// strings left as they are.
fn without_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(next);
                    i += 1;
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            continue;
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}
