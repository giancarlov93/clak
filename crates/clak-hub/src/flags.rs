//! From the command line to a command's JSON arguments: `--text hello --count 3` becomes
//! `{"text":"hello","count":3}`, according to the schema the program declared. It is done
//! here, in the hub, because the hub has the schema: this way the command line makes a
//! single request, and the user sees a single question.
//!
//! Each flag arrives as `{name, value}`: without a name it is a positional value, without a
//! value it is a switch (`--force` = true).

use crate::wire::ToolDef;
use clak_wire::names;
use serde_json::{Map, Value};

/// The arguments with the flags applied, or the problem to tell the caller (with the usage line).
pub fn apply(
    tool: &ToolDef,
    given: Option<&Map<String, Value>>,
    flags: &[Value],
) -> Result<Map<String, Value>, String> {
    let mut arguments = given.cloned().unwrap_or_default();
    let empty = Map::new();
    let schema = tool.schema();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut positional: Vec<String> = Vec::new();
    for flag in flags.iter().filter_map(Value::as_object) {
        let name = flag.get("name").and_then(Value::as_str);
        let value = flag.get("value").and_then(Value::as_str);
        let Some(name) = name else {
            if let Some(value) = value {
                positional.push(value.to_owned());
            }
            continue;
        };
        let Some(key) = matching(properties, name) else {
            let near = names::closest(Some(name), properties.keys());
            return Err(format!(
                "'{}' has no parameter --{name}{} {}",
                tool.name,
                near.map_or(".".to_owned(), |n| format!(": did you mean --{n}?")),
                usage(tool)
            ));
        };
        let property = properties.get(&key).and_then(Value::as_object);
        let converted = convert(property, value)
            .map_err(|problem| format!("--{name}: {problem}. {}", usage(tool)))?;
        // A repeated list parameter accumulates: --tag a --tag b.
        match (
            type_of(property).as_deref(),
            arguments.get_mut(&key),
            converted,
        ) {
            (Some("array"), Some(Value::Array(list)), Value::Array(more)) => list.extend(more),
            (_, _, converted) => {
                arguments.insert(key, converted);
            }
        }
    }

    // Unnamed values go, in order, to the required parameters still empty, then to the others.
    let mut free: Vec<String> = Vec::new();
    for key in required
        .iter()
        .copied()
        .chain(properties.keys().map(String::as_str))
    {
        if !free.iter().any(|k| k == key) && !arguments.contains_key(key) {
            free.push(key.to_owned());
        }
    }
    if positional.len() > free.len() {
        return Err(format!(
            "too many unnamed values for '{}'. {}",
            tool.name,
            usage(tool)
        ));
    }
    for (value, key) in positional.iter().zip(free.iter()) {
        let property = properties.get(key).and_then(Value::as_object);
        let converted = convert(property, Some(value))
            .map_err(|problem| format!("{key}: {problem}. {}", usage(tool)))?;
        arguments.insert(key.clone(), converted);
    }

    let missing: Vec<String> = required
        .iter()
        .filter(|k| !arguments.contains_key(**k))
        .map(|k| format!("--{k}"))
        .collect();
    if !missing.is_empty() {
        return Err(format!("missing {}. {}", missing.join(", "), usage(tool)));
    }
    Ok(arguments)
}

/// The name as a command-line user writes it: `--file-path` finds `file_path` and `filePath`.
fn matching(properties: &Map<String, Value>, name: &str) -> Option<String> {
    if properties.contains_key(name) {
        return Some(name.to_owned());
    }
    let plain = |text: &str| text.replace(['-', '_'], "").to_lowercase();
    let wanted = plain(name);
    properties.keys().find(|k| plain(k) == wanted).cloned()
}

fn type_of(property: Option<&Map<String, Value>>) -> Option<String> {
    match property?.get("type")? {
        Value::String(t) => Some(t.clone()),
        Value::Array(types) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .map(str::to_owned),
        _ => None,
    }
}

fn convert(property: Option<&Map<String, Value>>, text: Option<&str>) -> Result<Value, String> {
    let kind = type_of(property);
    let Some(text) = text else {
        return match kind.as_deref() {
            Some("boolean") | None => Ok(Value::Bool(true)),
            _ => Err("missing value".into()),
        };
    };
    match kind.as_deref() {
        Some("string") => Ok(Value::String(text.to_owned())),
        Some("integer") => text
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("'{text}' is not an integer")),
        Some("number") => match text.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(clak_wire::json::float(n)),
            _ => Err(format!("'{text}' is not a number")),
        },
        Some("boolean") => match text.trim().to_lowercase().as_str() {
            "true" | "1" | "si" | "sì" | "yes" | "vero" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "falso" => Ok(Value::Bool(false)),
            _ => Err(format!("'{text}' is not yes or no")),
        },
        // JSON if it is JSON, otherwise a single element.
        Some("array") => Ok(match parse(text) {
            Some(Value::Array(list)) => Value::Array(list),
            _ => Value::Array(vec![Value::String(text.to_owned())]),
        }),
        Some("object") => match parse(text) {
            Some(Value::Object(o)) => Ok(Value::Object(o)),
            _ => Err("a JSON object is required".into()),
        },
        _ => Ok(parse(text).unwrap_or_else(|| Value::String(text.to_owned()))),
    }
}

fn parse(text: &str) -> Option<Value> {
    clak_wire::json::parse_str(text).ok()
}

/// A line that reminds how to call the command.
pub fn usage(tool: &ToolDef) -> String {
    let empty = Map::new();
    let schema = tool.schema();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let parts: Vec<String> = properties
        .iter()
        .map(|(key, value)| {
            let flag = format!("--{key} {}", placeholder(value.as_object()))
                .trim_end()
                .to_owned();
            if required.contains(&key.as_str()) {
                flag
            } else {
                format!("[{flag}]")
            }
        })
        .collect();
    format!("Usage: {} {}", tool.name, parts.join(" "))
        .trim_end()
        .to_owned()
}

/// What goes after `--name`: the type, or the allowed values if they are few and short.
pub fn placeholder(property: Option<&Map<String, Value>>) -> String {
    if let Some(choices) = choices(property) {
        return choices;
    }
    match type_of(property).as_deref() {
        Some("string") => "TEXT",
        Some("integer") => "N",
        Some("number") => "NUMBER",
        Some("boolean") => "",
        Some("array") => "VALUE",
        Some("object") => "JSON",
        _ => "VALUE",
    }
    .to_owned()
}

/// `read|write`: so the usage line already says which values are accepted.
pub fn choices(property: Option<&Map<String, Value>>) -> Option<String> {
    let values = property?.get("enum")?.as_array()?;
    if values.is_empty() || values.len() > 8 {
        return None;
    }
    let texts: Vec<String> = values
        .iter()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => clak_wire::json::text(other),
        })
        .collect();
    let joined = texts.join("|");
    (joined.chars().count() <= 60
        && texts
            .iter()
            .all(|t| !t.is_empty() && !t.chars().any(char::is_whitespace)))
    .then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool() -> ToolDef {
        ToolDef::new(
            "append",
            "d",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "count": {"type": "integer"},
                    "tags": {"type": "array"},
                    "force": {"type": "boolean"},
                    "intent": {"type": "string", "enum": ["read", "write"]},
                },
                "required": ["text"],
            }),
        )
    }

    fn flags(list: Value) -> Vec<Value> {
        list.as_array().unwrap().clone()
    }

    #[test]
    fn i_flag_si_convertono_secondo_lo_schema() {
        let args = apply(
            &tool(),
            None,
            &flags(json!([
                {"name":"text","value":"ciao"}, {"name":"count","value":"3"},
                {"name":"tags","value":"a"}, {"name":"tags","value":"b"}, {"name":"force"}
            ])),
        )
        .unwrap();
        assert_eq!(
            Value::Object(args),
            json!({"text":"ciao","count":3,"tags":["a","b"],"force":true})
        );
    }

    #[test]
    fn i_valori_senza_nome_vanno_agli_obbligatori() {
        let args = apply(&tool(), None, &flags(json!([{"value":"eggs"}]))).unwrap();
        assert_eq!(args["text"], "eggs");
    }

    #[test]
    fn gli_errori_dicono_l_uso_e_il_suggerimento() {
        let error = apply(&tool(), None, &flags(json!([{"name":"txet","value":"x"}]))).unwrap_err();
        assert!(error.contains("did you mean --text?"), "{error}");
        assert!(
            error.contains("Usage: append --text TEXT [--count N]"),
            "{error}"
        );
        let error = apply(
            &tool(),
            None,
            &flags(json!([{"name":"text","value":"x"},{"name":"count","value":"tre"}])),
        )
        .unwrap_err();
        assert!(
            error.starts_with("--count: 'tre' is not an integer."),
            "{error}"
        );
        let error = apply(&tool(), None, &flags(json!([]))).unwrap_err();
        assert!(error.starts_with("missing --text."), "{error}");
        assert!(usage(&tool()).contains("[--intent read|write]"));
    }
}
