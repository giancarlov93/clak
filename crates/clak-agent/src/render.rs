//! The hub's responses, written for whoever reads them: a model, almost always. Short,
//! without decoration, and with the next step always written at the end.
//!
//! The command line and the shim use them; the app can reuse them to show in the panel
//! what an agent sees.

use crate::util::{Object, compact, flag, object_of, objects, pretty, str_of, utf16_len};
use serde_json::Value;
use std::collections::HashSet;

/// For the command line commands are written as flags; for MCP as JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Cli,
    Mcp,
}

/// The connected programs, with the name to use them by.
pub fn programs(response: &Object, style: Style) -> String {
    let list = objects(response.get("resources"));
    if list.is_empty() {
        return "No programs connected to Clak. A program connects by itself, usually with a \
                command or a button inside it: ask the user to do that."
            .into();
    }

    let width = list
        .iter()
        .map(|r| utf16_len(str_of(r, "name").unwrap_or("")))
        .max()
        .unwrap_or(0);
    let mut text = format!("Connected programs ({}):\n\n", list.len());
    for r in &list {
        let tools = r
            .get("tools")
            .and_then(Value::as_i64)
            .filter(|n| i32::try_from(*n).is_ok())
            .unwrap_or(0);
        let name = str_of(r, "name").unwrap_or("");
        text.push_str(&format!(
            "  {}  {} — {}\n",
            pad(name, width),
            str_of(r, "app").unwrap_or(""),
            str_of(r, "title").unwrap_or("")
        ));
        text.push_str(&format!(
            "  {}  {} · {}{} · {}\n",
            " ".repeat(width),
            str_of(r, "origin").unwrap_or(""),
            tools,
            if tools == 1 { " command" } else { " commands" },
            if flag(r, "allowed") {
                "permission granted"
            } else {
                "the first command will ask the user for permission"
            }
        ));
    }
    text.push('\n');
    text.push_str(match style {
        Style::Cli => "A program's commands: clak <name> list",
        Style::Mcp => "A program's commands: commands {\"program\": \"<name>\"}",
    });
    text
}

/// A program's commands. Its instructions, if it has any, come first: for MCP always;
/// for the command line with `clak <program> -h` (`help`), while `list` only says they
/// exist, because they can be long.
pub fn commands(response: &Object, style: Style, help: bool) -> String {
    let name = str_of(response, "name").unwrap_or("");
    let tools = objects(response.get("tools"));
    let shown = match style {
        Style::Cli => format!("clak {name}"),
        Style::Mcp => name.to_owned(),
    };
    let mut text = format!(
        "{} — {}  ({shown})\n\n",
        str_of(response, "app").unwrap_or(""),
        str_of(response, "title").unwrap_or("")
    );
    if let Some(instructions) = str_of(response, "instructions").filter(|i| !i.is_empty()) {
        if style == Style::Mcp || help {
            let normalized = instructions.trim().replace("\r\n", "\n");
            let lines: Vec<String> = normalized
                .split('\n')
                .map(|l| {
                    if l.is_empty() {
                        String::new()
                    } else {
                        format!("  {l}")
                    }
                })
                .collect();
            text.push_str("How it wants to be used:\n");
            text.push_str(&lines.join("\n"));
            text.push_str("\n\n");
        } else {
            text.push_str(&format!(
                "It has instructions: read them before using it, clak {name} -h\n\n"
            ));
        }
    }
    if tools.is_empty() {
        text.push_str("  No commands.\n");
    }
    let allowed = flag(response, "allowed");
    let mut changed = false;
    for tool in &tools {
        let mark = if allowed && !flag(tool, "approved") {
            "*"
        } else {
            ""
        };
        changed |= !mark.is_empty();
        text.push_str(&format!(
            "  {}{mark} — {}\n",
            str_of(tool, "name").unwrap_or(""),
            one_line(str_of(tool, "description"))
        ));
        let detail = match style {
            Style::Cli => usage(name, tool),
            Style::Mcp => format!(
                "parameters: {}",
                compact(tool.get("input_schema").unwrap_or(&Value::Null))
            ),
        };
        text.push_str(&format!("      {detail}\n"));
    }
    text.push('\n');
    if !allowed {
        text.push_str("The first command will ask the user for permission, in a Clak window.\n");
    } else if changed {
        text.push_str("* new or changed since permission was granted: the first use will ask the user again.\n");
    }
    text.push_str(&match style {
        Style::Cli => format!("Details of a command: clak {name} <command> -h"),
        Style::Mcp => format!("To run one: run {{\"program\": \"{name}\", \"command\": \"<command>\", \"arguments\": {{...}}}}"),
    });
    text
}

/// Everything about a command: what it is for, the parameters, an example.
pub fn command(response: &Object, command: &str) -> Option<String> {
    let name = str_of(response, "name").unwrap_or("");
    let tool = objects(response.get("tools"))
        .into_iter()
        .find(|t| str_of(t, "name") == Some(command))?;
    let mut text = format!(
        "{}\n\n{}\n",
        usage(name, tool),
        str_of(tool, "description").unwrap_or("")
    );
    let (properties, required) = schema(tool);
    if !properties.is_empty() {
        text.push_str("\nParameters:\n");
        for (key, value) in properties {
            let property = value.as_object();
            let kind = match property.and_then(|p| p.get("type")) {
                Some(Value::Array(kinds)) => Some(
                    kinds
                        .iter()
                        .map(|k| k.as_str().unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join("|"),
                ),
                Some(other) => other.as_str().map(str::to_owned),
                None => None,
            };
            text.push_str(&format!("  --{key}"));
            if let Some(kind) = kind {
                text.push_str(&format!(" ({kind})"));
            }
            if required.contains(key.as_str()) {
                text.push_str(", required");
            }
            if let Some(description) = property
                .and_then(|p| str_of(p, "description"))
                .filter(|d| !d.is_empty())
            {
                text.push_str(&format!(": {}", one_line(Some(description))));
            }
            text.push('\n');
            if let Some(Value::Array(choices)) = property.and_then(|p| p.get("enum")) {
                let listed: Vec<String> = choices.iter().map(compact).collect();
                text.push_str(&format!("      values: {}\n", listed.join(", ")));
            }
            // The default says what happens if the parameter is not passed: often that is enough.
            if let Some(given) = property.and_then(|p| p.get("default")) {
                text.push_str(&format!("      default: {}\n", compact(given)));
            }
        }
    }
    // The file before stdin: no heredoc or quoting, and accented letters arrive intact.
    text.push_str(
        "\nLong text, multi-line text or text with accented letters: write it to a file (UTF-8) and pass it with --parameter @file. \
         Short text: --parameter - reads it from stdin. --args @file reads all arguments as JSON.",
    );
    Some(text)
}

/// The properties of a command's schema, and which are required.
fn schema(tool: &Object) -> (serde_json::Map<String, Value>, HashSet<&str>) {
    let input = object_of(tool, "input_schema");
    let properties = input
        .and_then(|s| object_of(s, "properties"))
        .cloned()
        .unwrap_or_default();
    let required = input
        .and_then(|s| s.get("required"))
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    (properties, required)
}

/// The usage line: `clak revit run_python --code TEXT --intent read|write [--timeout N]`.
fn usage(program: &str, tool: &Object) -> String {
    let (properties, required) = schema(tool);
    let mut parts = vec![format!(
        "clak {program} {}",
        str_of(tool, "name").unwrap_or("")
    )];
    for (key, value) in &properties {
        let flag = format!("--{key} {}", placeholder(value.as_object()))
            .trim_end()
            .to_owned();
        parts.push(if required.contains(key.as_str()) {
            flag
        } else {
            format!("[{flag}]")
        });
    }
    parts.join(" ")
}

/// What goes after `--name`: the allowed values if they are few and short (`read|write`),
/// otherwise the type. This way the usage line from `list` is enough to write the right call.
fn placeholder(property: Option<&Object>) -> String {
    if let Some(choices) = choices(property) {
        return choices;
    }
    match property.and_then(|p| str_of(p, "type")) {
        Some("string") => "TEXT",
        Some("integer") => "N",
        Some("number") => "NUMBER",
        Some("boolean") => "",
        Some("object") => "JSON",
        _ => "VALUE",
    }
    .into()
}

fn choices(property: Option<&Object>) -> Option<String> {
    let values = property?.get("enum")?.as_array()?;
    if values.is_empty() || values.len() > 8 {
        return None;
    }
    let texts: Vec<String> = values
        .iter()
        .map(|v| v.as_str().map_or_else(|| compact(v), str::to_owned))
        .collect();
    let joined = texts.join("|");
    let plain = texts
        .iter()
        .all(|t| !t.is_empty() && !t.chars().any(char::is_whitespace));
    (utf16_len(&joined) <= 60 && plain).then_some(joined)
}

/// A command's result as it is: a text stays text, text-only blocks become their text,
/// the rest is readable JSON.
pub fn result(content: Option<&Value>) -> String {
    match content {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(scalar @ (Value::Bool(_) | Value::Number(_))) => compact(scalar),
        Some(other) => match blocks(Some(other)) {
            Some(list) if list.iter().all(|b| str_of(b, "type") == Some("text")) => list
                .iter()
                .map(|b| str_of(b, "text").unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => pretty(other),
        },
    }
}

/// A result made of blocks like MCP's, or nothing if it isn't: all the blocks must be
/// well formed, otherwise the result is shown as it is.
///
/// ```text
///   {"type":"text", "text":…}
///   {"type":"image"|"audio", "data":base64, "mimeType":…}
///   {"type":"resource", "resource":{"uri":…, "mimeType"?:…, "blob":base64 | "text":…}}
/// ```
///
/// The last form is the one for any file: a PDF, an archive, a .blend.
pub fn blocks(content: Option<&Value>) -> Option<Vec<&Object>> {
    let list = content?.as_array()?;
    if list.is_empty() {
        return None;
    }
    let blocks: Vec<&Object> = list.iter().filter_map(Value::as_object).collect();
    if blocks.len() != list.len() {
        return None;
    }
    let well_formed = blocks.iter().all(|b| match str_of(b, "type") {
        Some("text") => str_of(b, "text").is_some(),
        Some("image" | "audio") => {
            str_of(b, "data").is_some() && str_of(b, "mimeType").is_some_and(|m| !m.is_empty())
        }
        Some("resource") => object_of(b, "resource").is_some_and(|r| {
            str_of(r, "uri").is_some_and(|u| !u.is_empty())
                && (str_of(r, "blob").is_some() || str_of(r, "text").is_some())
        }),
        _ => false,
    });
    well_formed.then_some(blocks)
}

/// A block that carries a binary file: an image, an audio, a resource with `blob`.
pub fn is_binary(block: &Object) -> bool {
    match str_of(block, "type") {
        Some("image" | "audio") => true,
        Some("resource") => object_of(block, "resource")
            .and_then(|r| str_of(r, "blob"))
            .is_some(),
        _ => false,
    }
}

/// A number with at most three decimals, without trailing zeros: `1.5`, `60`, `0.333`.
pub fn number(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

fn one_line(text: Option<&str>) -> String {
    let flat = text
        .unwrap_or("")
        .split(['\r', '\n', '\t'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let flat = flat.trim();
    if utf16_len(flat) <= 160 {
        flat.to_owned()
    } else {
        format!("{}…", crate::util::utf16_prefix(flat, 159))
    }
}

/// Pads with spaces up to `width`, counting in UTF-16 units.
fn pad(text: &str, width: usize) -> String {
    let missing = width.saturating_sub(utf16_len(text));
    format!("{text}{}", " ".repeat(missing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn content(text: &str) -> Value {
        clak_wire::json::parse_str(text).unwrap()
    }

    fn object(value: Value) -> Object {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn mcp_blocks_are_recognized_only_when_well_formed() {
        assert!(blocks(Some(&content(r#"[{"type":"text","text":"a"},{"type":"image","data":"AA==","mimeType":"image/png"}]"#))).is_some());
        assert!(
            blocks(Some(&content(
                r#"[{"type":"resource","resource":{"uri":"file:///x/a.pdf","blob":"AA=="}}]"#
            )))
            .is_some()
        );
        assert!(
            blocks(Some(&content(
                r#"[{"type":"audio","data":"AA==","mimeType":"audio/wav"}]"#
            )))
            .is_some()
        );
        assert!(blocks(Some(&content(r#"[{"type":"image","data":"AA=="}]"#))).is_none()); // without mimeType
        assert!(blocks(Some(&content(r#"[{"type":"text","text":"a"}, 3]"#))).is_none());
        assert!(blocks(Some(&content(r#"[{"name":"any data"}]"#))).is_none());
        assert!(blocks(Some(&content("[]"))).is_none());
        // Text only: the text is printed, not the JSON.
        assert_eq!(
            result(Some(&content(
                r#"[{"type":"text","text":"one"},{"type":"text","text":"two"}]"#
            ))),
            "one\ntwo"
        );
        assert_eq!(result(Some(&json!(3))), "3");
        assert_eq!(result(Some(&json!(null))), "");
        assert_eq!(result(Some(&json!({"a": 1}))), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn the_list_says_the_next_step() {
        let response = object(content(
            r#"{"resources":[{"name":"word","app":"Word","title":"contract.docx","origin":"winword.exe","tools":3,"allowed":false}]}"#,
        ));
        let text = programs(&response, Style::Cli);
        assert!(text.contains("word  Word — contract.docx"));
        assert!(text.contains("3 commands · the first command will ask the user for permission"));
        assert!(text.ends_with("clak <name> list"));
        assert!(programs(&response, Style::Mcp).contains("commands {"));
        assert!(programs(&object(json!({"resources": []})), Style::Cli).contains("ask the user"));
    }

    #[test]
    fn the_list_is_enough_to_write_the_call_and_the_help_gives_defaults() {
        let response = object(content(
            r#"{"name":"revit","app":"Revit","title":"model.rvt","allowed":true,"tools":[{"name":"run_python","description":"Runs a script.",
             "approved":true,"input_schema":{"type":"object","properties":{"code":{"type":"string"},
             "intent":{"type":"string","enum":["read","write"]},"timeout":{"type":"integer","default":60}},"required":["code","intent"]}}]}"#,
        ));
        assert!(
            commands(&response, Style::Cli, false)
                .contains("clak revit run_python --code TEXT --intent read|write [--timeout N]")
        );
        let help = command(&response, "run_python").unwrap();
        assert!(help.contains("default: 60"));
        assert!(help.contains("--parameter @file"));
        assert!(
            help.contains("  --intent (string), required\n      values: \"read\", \"write\"\n")
        );
        assert!(command(&response, "other").is_none());
    }

    #[test]
    fn a_programs_instructions_are_in_its_help() {
        let mut response = object(content(
            r#"{"name":"petstore","app":"Petstore","title":"API","allowed":true,"instructions":"Read the pets first.\nThen sell.","tools":[]}"#,
        ));
        let list = commands(&response, Style::Cli, false);
        assert!(list.contains("It has instructions: read them before using it, clak petstore -h"));
        assert!(!list.contains("Read the pets first."));
        let help = commands(&response, Style::Cli, true);
        assert!(help.contains("How it wants to be used:\n  Read the pets first.\n  Then sell.\n"));
        assert!(commands(&response, Style::Mcp, false).contains("  Read the pets first."));
        // Without instructions, -h is the list.
        response.remove("instructions");
        assert_eq!(
            commands(&response, Style::Cli, false),
            commands(&response, Style::Cli, true)
        );
    }

    #[test]
    fn new_commands_are_marked_and_mcp_shows_the_schema() {
        let response = object(json!({
            "name": "word", "app": "Word", "title": "a.docx", "allowed": true,
            "tools": [{"name": "append_text", "description": "Adds\ntext.", "approved": false,
                       "input_schema": {"type": "object"}}]
        }));
        let cli = commands(&response, Style::Cli, false);
        assert!(cli.starts_with("Word — a.docx  (clak word)\n\n  append_text* — Adds text.\n      clak word append_text\n"));
        assert!(cli.contains("* new or changed since permission was granted"));
        let mcp = commands(&response, Style::Mcp, false);
        assert!(mcp.contains("      parameters: {\"type\":\"object\"}\n"));
        assert!(mcp.ends_with(
            r#"To run one: run {"program": "word", "command": "<command>", "arguments": {...}}"#
        ));
    }

    #[test]
    fn numbers_have_at_most_three_decimals() {
        assert_eq!(number(60.0), "60");
        assert_eq!(number(1.5), "1.5");
        assert_eq!(number(1.0 / 3.0), "0.333");
    }
}
