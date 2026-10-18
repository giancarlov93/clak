//! The shapes that travel on the wire: who connects (the resource), its commands, and how
//! to describe where it comes from. No object serialization: JSON trees, so what a program
//! declares crosses the hub intact.

use clak_wire::json::{self};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::fmt;
use std::sync::LazyLock;

/// A message that parses as JSON but does not say what it should. It carries the two forms
/// it is reported in: one for whoever sent it and one for the log (`KeyError('tools')`),
/// because the panel reads the log too.
#[derive(Debug, Clone)]
pub struct Malformed {
    kind: &'static str,
    detail: String,
}

impl Malformed {
    pub fn missing(key: &str) -> Self {
        Malformed {
            kind: "KeyError",
            detail: key.to_owned(),
        }
    }

    pub fn type_error(detail: impl Into<String>) -> Self {
        Malformed {
            kind: "TypeError",
            detail: detail.into(),
        }
    }

    pub fn value(detail: impl Into<String>) -> Self {
        Malformed {
            kind: "ValueError",
            detail: detail.into(),
        }
    }

    /// For whoever sent it.
    pub fn text(&self) -> String {
        if self.kind == "KeyError" {
            format!("'{}'", self.detail)
        } else {
            self.detail.clone()
        }
    }

    /// For the log.
    pub fn repr(&self) -> String {
        format!("{}('{}')", self.kind, self.detail)
    }
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

impl std::error::Error for Malformed {}

pub fn required<'a>(message: &'a Map<String, Value>, key: &str) -> Result<&'a Value, Malformed> {
    message.get(key).ok_or_else(|| Malformed::missing(key))
}

pub fn required_string<'a>(
    message: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, Malformed> {
    required(message, key)?
        .as_str()
        .ok_or_else(|| Malformed::type_error(format!("“{key}” must be a string")))
}

/// A value used as a registry key (an id). A scalar that is not a string simply finds
/// nothing; a list or an object is not a key.
pub fn key(value: Option<&Value>) -> Result<Option<&str>, Malformed> {
    match value {
        Some(Value::Array(_) | Value::Object(_)) => {
            Err(Malformed::type_error("an id can't be a list or an object"))
        }
        other => Ok(json::str(other)),
    }
}

/// What connects: not the application, the live instance.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceDef {
    pub app: String,
    pub title: String,
    pub instance_id: String,
    pub publisher: Option<String>,
}

impl ResourceDef {
    pub fn label(&self) -> String {
        format!("{} — {}", self.app, self.title)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "app": self.app,
            "title": self.title,
            "instance_id": self.instance_id,
            "publisher": self.publisher,
        })
    }

    pub fn from_json(value: &Value) -> Result<ResourceDef, Malformed> {
        let Value::Object(d) = value else {
            return Err(Malformed::type_error("“resource” must be an object"));
        };
        let publisher = match d.get("publisher") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(Malformed::type_error("“publisher” must be a string")),
        };
        Ok(ResourceDef {
            app: required_string(d, "app")?.to_owned(),
            title: required_string(d, "title")?.to_owned(),
            instance_id: required_string(d, "instance_id")?.to_owned(),
            publisher,
        })
    }
}

/// A command as the program declared it. The digest covers name, description and schema
/// verbatim: the description ends up in the model's context, so even a comma is attack
/// surface, and a rule the program can get around with a synonym is not a rule.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    json: Value,
    pub digest: String,
}

static TOOL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,64}$").unwrap());

impl ToolDef {
    /// A command built by the hub itself (settings, REST APIs, connectors).
    pub fn new(name: &str, description: &str, schema: Value) -> ToolDef {
        let json = json!({ "name": name, "description": description, "input_schema": schema });
        let digest = json::digest(&json);
        ToolDef {
            name: name.to_owned(),
            description: description.to_owned(),
            json,
            digest,
        }
    }

    /// The parameter schema. To read, not to modify: it is part of the digest.
    pub fn schema(&self) -> &Map<String, Value> {
        self.json["input_schema"]
            .as_object()
            .expect("input_schema is always an object")
    }

    /// A copy of the command as it goes on the wire.
    pub fn to_json(&self) -> Value {
        self.json.clone()
    }

    pub fn from_json(value: &Value) -> Result<ToolDef, Malformed> {
        let Value::Object(d) = value else {
            return Err(Malformed::type_error("a tool must be an object"));
        };
        let name = required_string(d, "name")?;
        let description = required_string(d, "description")?;
        let schema = match d.get("input_schema") {
            s if !json::truthy(s) => json!({ "type": "object", "properties": {} }),
            Some(Value::Object(s)) => Value::Object(s.clone()),
            _ => return Err(Malformed::type_error("“input_schema” must be an object")),
        };
        Ok(ToolDef::new(name, description, schema))
    }
}

pub fn parse_tools(raw: &Value) -> Result<Vec<ToolDef>, Malformed> {
    let Value::Array(list) = raw else {
        return Err(Malformed::type_error("“tools” must be a list"));
    };
    let tools = list
        .iter()
        .map(ToolDef::from_json)
        .collect::<Result<Vec<_>, _>>()?;
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if names.len() != tools.len() || !tools.iter().all(|t| TOOL_NAME.is_match(&t.name)) {
        return Err(Malformed::value("invalid or duplicate tool names"));
    }
    Ok(tools)
}

/// Of the manifest, the commands enabled right now. Without `active` all of them are.
pub fn active(manifest: Vec<ToolDef>, active: Option<&Value>) -> Result<Vec<ToolDef>, Malformed> {
    match active {
        None => Ok(manifest),
        Some(Value::Array(list)) => {
            let mut on = Vec::new();
            for item in list {
                if let Some(name) = key(Some(item))? {
                    on.push(name.to_owned());
                }
            }
            Ok(manifest
                .into_iter()
                .filter(|t| on.contains(&t.name))
                .collect())
        }
        Some(_) => Err(Malformed::type_error("“active” must be a list of names")),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Which service the hub reaches: an MCP server or a REST API.
fn service(origin: &Value) -> &'static str {
    if origin.get("protocol").and_then(Value::as_str) == Some("rest") {
        "REST API"
    } else {
        "MCP server"
    }
}

fn unencrypted(origin: &Value) -> &'static str {
    if origin.get("secure").and_then(Value::as_bool) == Some(false) {
        ", unencrypted"
    } else {
        ""
    }
}

/// Who is offering itself, according to the operating system and not to the program.
pub fn origin_label(origin: &Value) -> String {
    let kind = origin.get("kind").and_then(Value::as_str);
    if kind == Some("hub") {
        return "Clak itself".into();
    }
    if kind == Some("connector")
        && let Some(command) = local_program(origin, "command")
    {
        return format!("{}, local program: {command}", service(origin));
    }
    if kind == Some("connector") {
        let url = origin.get("url").and_then(Value::as_str).unwrap_or("");
        return format!("{}, {url}{}", service(origin), unencrypted(origin));
    }
    let Some(path) = origin
        .get("exe")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    else {
        return "unidentified process".into();
    };
    let exe = file_name(path);
    match (
        origin.get("signed").and_then(Value::as_str),
        origin.get("signer").and_then(Value::as_str),
    ) {
        (Some("valid"), Some(signer)) if !signer.is_empty() => format!("{exe}, signed by {signer}"),
        (Some("invalid"), _) => format!("{exe}, INVALID SIGNATURE ({path})"),
        _ => format!("{exe}, unsigned ({path})"),
    }
}

/// The same thing in short, for a list: without the path.
pub fn origin_short(origin: &Value) -> String {
    let kind = origin.get("kind").and_then(Value::as_str);
    if kind == Some("hub") {
        return "Clak itself".into();
    }
    if kind == Some("connector")
        && let Some(program) = local_program(origin, "program")
    {
        return format!("{}, local program {program}", service(origin));
    }
    if kind == Some("connector") {
        let site = origin
            .get("url")
            .and_then(Value::as_str)
            .and_then(site_of)
            .unwrap_or_else(|| "?".into());
        return format!("{}, {site}{}", service(origin), unencrypted(origin));
    }
    let Some(path) = origin
        .get("exe")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    else {
        return "unidentified process".into();
    };
    let exe = file_name(path);
    match (
        origin.get("signed").and_then(Value::as_str),
        origin.get("signer").and_then(Value::as_str),
    ) {
        (Some("valid"), Some(signer)) if !signer.is_empty() => format!("{exe}, signed by {signer}"),
        (Some("invalid"), _) => format!("{exe}, INVALID SIGNATURE"),
        _ => format!("{exe}, unsigned"),
    }
}

/// A connector that is a program on this computer (stdio): its command line (`command`)
/// or the program's name (`program`).
fn local_program<'a>(origin: &'a Value, field: &str) -> Option<&'a str> {
    if origin.get("transport").and_then(Value::as_str) != Some("stdio") {
        return None;
    }
    Some(origin.get(field).and_then(Value::as_str).unwrap_or("?"))
}

/// `scheme://host:port` of an absolute address: the part that says where it goes.
fn site_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_without_schema_gets_an_empty_one() {
        let tool = ToolDef::from_json(&json!({"name":"a","description":"d"})).unwrap();
        assert_eq!(tool.schema()["type"], "object");
        let same =
            ToolDef::from_json(&json!({"name":"a","description":"d","input_schema":{}})).unwrap();
        assert_eq!(tool.digest, same.digest);
        let other = ToolDef::from_json(&json!({"name":"a","description":"d."})).unwrap();
        assert_ne!(tool.digest, other.digest);
    }

    #[test]
    fn duplicate_or_odd_names_are_refused() {
        assert!(
            parse_tools(&json!([{"name":"a","description":""},{"name":"a","description":""}]))
                .is_err()
        );
        assert!(parse_tools(&json!([{"name":"a b","description":""}])).is_err());
        assert!(parse_tools(&json!({"name":"a"})).is_err());
    }

    #[test]
    fn the_origin_is_described() {
        let signed =
            json!({"exe": r"C:\x\blender.exe", "signed": "valid", "signer": "Blender Foundation"});
        assert_eq!(
            origin_short(&signed),
            "blender.exe, signed by Blender Foundation"
        );
        let connector =
            json!({"kind":"connector","url":"http://example.com:8080/mcp","secure":false});
        assert_eq!(
            origin_short(&connector),
            "MCP server, http://example.com:8080, unencrypted"
        );
        assert_eq!(origin_label(&json!({"kind":"hub"})), "Clak itself");
        let local = json!({"kind":"connector","transport":"stdio","program":"agent-browser","command":"agent-browser mcp --tools core"});
        assert_eq!(
            origin_short(&local),
            "MCP server, local program agent-browser"
        );
        assert_eq!(
            origin_label(&local),
            "MCP server, local program: agent-browser mcp --tools core"
        );
    }
}
