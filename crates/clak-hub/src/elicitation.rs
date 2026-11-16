//! What an MCP server asks the user halfway through a command (elicitation, MCP
//! 2025-11-25): a form to fill in, or a page to open in the browser.
//!
//! The hub reads the server's request into a [`Form`] the interfaces draw, or a [`Page`]
//! they show, and checks the answer before it goes back. The interfaces check too, to say at
//! once what is wrong; this is the check that counts.
//!
//! A form is flat: text (with a length, a pattern, or a format: email, uri, date,
//! date-time), a number or an integer (with its range), yes or no, one choice among a list,
//! several choices among a list. Anything else is not a form Clak shows, and the server is
//! told so. A page is a web address (`https` or `http`): the interfaces show it whole, with
//! the site set apart, and open it only when the user says so.

use crate::texts::{clean, secret};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::net::IpAddr;
use url::{Position, Url};

/// Past these, the form isn't shown.
const MAX_FIELDS: usize = 50;
const MAX_OPTIONS: usize = 500;
/// A text holds at most this many characters, whatever the server says.
const MAX_TEXT: usize = 10_000;
/// An address longer than this isn't one to show the user.
const MAX_URL: usize = 4_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The key in the answer, as the server named it.
    pub name: String,
    /// What the user reads: the server's title, or else the name.
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: Kind,
    /// The value it starts with: the server's default, when it fits the field.
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text {
        min: Option<usize>,
        max: Option<usize>,
        /// A regular expression the text must contain a match of (JSON Schema's `pattern`).
        pattern: Option<String>,
        format: Option<Format>,
    },
    Number {
        integer: bool,
        min: Option<f64>,
        max: Option<f64>,
    },
    Boolean,
    /// One of the options.
    Choice {
        options: Vec<Choice>,
    },
    /// Some of the options.
    Choices {
        options: Vec<Choice>,
        min: Option<usize>,
        max: Option<usize>,
    },
}

/// An option: the value that goes back, and the words the user reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Email,
    Uri,
    Date,
    DateTime,
}

impl Format {
    fn parse(text: &str) -> Option<Format> {
        match text {
            "email" => Some(Format::Email),
            "uri" => Some(Format::Uri),
            "date" => Some(Format::Date),
            "date-time" => Some(Format::DateTime),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Email => "email",
            Format::Uri => "uri",
            Format::Date => "date",
            Format::DateTime => "date-time",
        }
    }
}

impl Form {
    /// The form of a `requestedSchema`. No schema, or one without fields, is a form without
    /// fields: a plain confirmation.
    pub fn parse(schema: Option<&Value>) -> Result<Form, String> {
        let none = Form { fields: vec![] };
        let Some(schema) = schema.filter(|s| !s.is_null()) else {
            return Ok(none);
        };
        let Some(schema) = schema.as_object() else {
            return Err("the form isn't a JSON object".into());
        };
        if schema
            .get("type")
            .is_some_and(|t| t.as_str() != Some("object"))
        {
            return Err("the form isn't an object with fields".into());
        }
        let properties = match schema.get("properties") {
            None | Some(Value::Null) => return Ok(none),
            Some(Value::Object(properties)) => properties,
            Some(_) => return Err("the form's fields aren't an object".into()),
        };
        if properties.len() > MAX_FIELDS {
            return Err(format!("the form has more than {MAX_FIELDS} fields"));
        }
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        for (name, node) in properties {
            let Some(node) = node.as_object() else {
                return Err(format!(
                    "the field “{}” isn't described",
                    clean(Some(name), 40)
                ));
            };
            let kind = kind_of(name, node)?;
            let title = text_of(node.get("title"), 80).unwrap_or_else(|| clean(Some(name), 80));
            let mut field = Field {
                name: name.clone(),
                title,
                description: text_of(node.get("description"), 300),
                required: required.contains(&name.as_str()),
                kind,
                default: None,
            };
            field.default = node
                .get("default")
                .filter(|d| !d.is_null())
                .and_then(|d| field.check(d).ok());
            fields.push(field);
        }
        Ok(Form { fields })
    }

    /// The answer as the server gets it: the fields it asked for and nothing else, each one
    /// as its field wants it. A required field left empty, or a value that doesn't fit, is an
    /// error the user reads.
    pub fn check(&self, content: &Map<String, Value>) -> Result<Map<String, Value>, String> {
        let mut checked = Map::new();
        for field in &self.fields {
            let value = content.get(&field.name).filter(|v| !empty(v));
            match value {
                None if field.required => {
                    return Err(format!("“{}” is required.", field.title));
                }
                None => {}
                Some(value) => {
                    checked.insert(field.name.clone(), field.check(value)?);
                }
            }
        }
        Ok(checked)
    }

    /// The titles of the fields that look like they ask for a secret: a password, a key, a
    /// token, a card. A server must not ask for those in a form.
    pub fn secrets(&self) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|f| looks_secret(&f.name) || looks_secret(&f.title))
            .map(|f| f.title.as_str())
            .collect()
    }

    pub fn to_json(&self) -> Value {
        json!({ "fields": self.fields.iter().map(Field::to_json).collect::<Vec<_>>() })
    }
}

impl Field {
    /// The value, if it fits the field: as the server will get it.
    pub fn check(&self, value: &Value) -> Result<Value, String> {
        let title = &self.title;
        match &self.kind {
            Kind::Text {
                min,
                max,
                pattern,
                format,
            } => {
                let Some(text) = value.as_str() else {
                    return Err(format!("“{title}” must be text."));
                };
                let length = text.chars().count();
                if min.is_some_and(|min| length < min) {
                    return Err(format!(
                        "“{title}” needs at least {} characters.",
                        min.unwrap_or(0)
                    ));
                }
                let max = max.unwrap_or(MAX_TEXT).min(MAX_TEXT);
                if length > max {
                    return Err(format!("“{title}” takes at most {max} characters."));
                }
                if let Some(pattern) = pattern
                    && let Ok(regex) = Regex::new(pattern)
                    && !regex.is_match(text)
                {
                    return Err(format!("“{title}” isn't in the form the server expects."));
                }
                if let Some(format) = format
                    && !fits(*format, text)
                {
                    let what = match format {
                        Format::Email => "an email address",
                        Format::Uri => "an address (URI)",
                        Format::Date => "a date (YYYY-MM-DD)",
                        Format::DateTime => "a date and time",
                    };
                    return Err(format!("“{title}” must be {what}."));
                }
                Ok(Value::String(text.to_owned()))
            }
            Kind::Number { integer, min, max } => {
                let Some(number) = value.as_f64().filter(|n| n.is_finite()) else {
                    return Err(format!("“{title}” must be a number."));
                };
                if *integer && number.fract() != 0.0 {
                    return Err(format!("“{title}” must be a whole number."));
                }
                if let Some(min) = min
                    && number < *min
                {
                    return Err(format!("“{title}” must be at least {}.", shown(*min)));
                }
                if let Some(max) = max
                    && number > *max
                {
                    return Err(format!("“{title}” must be at most {}.", shown(*max)));
                }
                Ok(if *integer && number.abs() < 9.0e15 {
                    json!(number as i64)
                } else {
                    value.clone()
                })
            }
            Kind::Boolean => value
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| format!("“{title}” must be yes or no.")),
            Kind::Choice { options } => match value.as_str() {
                Some(chosen) if options.iter().any(|o| o.value == chosen) => {
                    Ok(Value::String(chosen.to_owned()))
                }
                _ => Err(format!("“{title}”: choose one of the options.")),
            },
            Kind::Choices { options, min, max } => {
                let Some(list) = value.as_array() else {
                    return Err(format!("“{title}”: choose among the options."));
                };
                let mut chosen: Vec<&str> = Vec::new();
                for item in list {
                    match item.as_str() {
                        Some(value) if options.iter().any(|o| o.value == value) => {
                            if !chosen.contains(&value) {
                                chosen.push(value);
                            }
                        }
                        _ => return Err(format!("“{title}”: choose among the options.")),
                    }
                }
                if let Some(min) = min
                    && chosen.len() < *min
                {
                    return Err(format!("“{title}”: choose at least {min}."));
                }
                if let Some(max) = max
                    && chosen.len() > *max
                {
                    return Err(format!("“{title}”: choose at most {max}."));
                }
                Ok(json!(chosen))
            }
        }
    }

    fn to_json(&self) -> Value {
        let mut field = json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "required": self.required,
            "default": self.default,
        });
        let more = match &self.kind {
            Kind::Text {
                min,
                max,
                pattern,
                format,
            } => json!({
                "kind": "text",
                "min_length": min,
                "max_length": max.unwrap_or(MAX_TEXT).min(MAX_TEXT),
                "pattern": pattern,
                "format": format.map(Format::as_str),
            }),
            Kind::Number { integer, min, max } => json!({
                "kind": "number",
                "integer": integer,
                "minimum": min,
                "maximum": max,
            }),
            Kind::Boolean => json!({ "kind": "boolean" }),
            Kind::Choice { options } => {
                json!({ "kind": "choice", "options": options_json(options) })
            }
            Kind::Choices { options, min, max } => json!({
                "kind": "choices",
                "options": options_json(options),
                "min_items": min,
                "max_items": max,
            }),
        };
        if let (Value::Object(field), Value::Object(more)) = (&mut field, more) {
            field.extend(more);
        }
        field
    }
}

/// A web address the server asks the user to open: shown whole, with the site set apart,
/// and opened only when the user says so.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub url: String,
    /// The address in three parts, so that the site stands out: «https://», «example.com»,
    /// «/connect?id=…». What comes before an «@» is in the first part: it isn't the site.
    pub before: String,
    pub host: String,
    pub after: String,
    /// Once opened, the command waits for the user to finish there (a server that answered
    /// it can't go on without it): then it runs again.
    pub wait: bool,
    /// What the user should know before opening it.
    pub warnings: Vec<String>,
}

impl Page {
    pub fn parse(address: &str, wait: bool) -> Result<Page, String> {
        let address = address.trim();
        if address.is_empty() {
            return Err("the request has no address".into());
        }
        if address.len() > MAX_URL {
            return Err("the address is too long".into());
        }
        let url = Url::parse(address).map_err(|_| "the address isn't a valid URL".to_owned())?;
        if !matches!(url.scheme(), "https" | "http") {
            return Err(format!(
                "Clak opens only web pages, not “{}:” addresses",
                clean(Some(url.scheme()), 20)
            ));
        }
        let Some(host) = url.host_str().map(str::to_owned) else {
            return Err("the address has no site".into());
        };
        let mut warnings = Vec::new();
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let loopback =
            bare == "localhost" || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
        if url.scheme() == "http" && !loopback {
            warnings.push(
                "The page isn't encrypted (http): what you type there travels over the network in clear text."
                    .to_owned(),
            );
        }
        if host.split('.').any(|label| label.starts_with("xn--")) {
            warnings.push(format!(
                "The site's name is written with letters of other alphabets ({host}): make sure it is the site you expect."
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            warnings.push(format!(
                "What comes before “@” in the address isn't the site: the site is {host}."
            ));
        }
        Ok(Page {
            before: url[..Position::BeforeHost].to_owned(),
            host: url[Position::BeforeHost..Position::AfterPort].to_owned(),
            after: url[Position::AfterPort..].to_owned(),
            url: url.to_string(),
            wait,
            warnings,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "url": self.url,
            "before": self.before,
            "host": self.host,
            "after": self.after,
            "wait": self.wait,
            "warnings": self.warnings,
        })
    }
}

/// A value that says nothing: absent, an empty text, an empty list.
fn empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(list) => list.is_empty(),
        _ => false,
    }
}

fn text_of(value: Option<&Value>, limit: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|text| clean(Some(text), limit))
        .filter(|text| !text.is_empty())
}

fn kind_of(name: &str, node: &Map<String, Value>) -> Result<Kind, String> {
    let unsupported = || {
        Err(format!(
            "the field “{}” asks for a kind of value Clak can't show",
            clean(Some(name), 40)
        ))
    };
    let count = |key: &str| {
        node.get(key)
            .and_then(Value::as_u64)
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
    };
    let number = |key: &str| node.get(key).and_then(Value::as_f64);
    match node.get("type").and_then(Value::as_str) {
        Some("string") => {
            if let Some(options) = titled(node.get("oneOf"))? {
                return Ok(Kind::Choice { options });
            }
            if let Some(options) = plain(node.get("enum"), node.get("enumNames"))? {
                return Ok(Kind::Choice { options });
            }
            Ok(Kind::Text {
                min: count("minLength"),
                max: count("maxLength"),
                pattern: node
                    .get("pattern")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                format: node
                    .get("format")
                    .and_then(Value::as_str)
                    .and_then(Format::parse),
            })
        }
        Some(kind @ ("number" | "integer")) => Ok(Kind::Number {
            integer: kind == "integer",
            min: number("minimum"),
            max: number("maximum"),
        }),
        Some("boolean") => Ok(Kind::Boolean),
        Some("array") => {
            let Some(items) = node.get("items").and_then(Value::as_object) else {
                return unsupported();
            };
            let options = match titled(items.get("anyOf").or_else(|| items.get("oneOf")))? {
                Some(options) => options,
                None => match plain(items.get("enum"), None)? {
                    Some(options) => options,
                    None => return unsupported(),
                },
            };
            Ok(Kind::Choices {
                options,
                min: count("minItems"),
                max: count("maxItems"),
            })
        }
        _ => unsupported(),
    }
}

/// Options given as `[{ "const": "#FF0000", "title": "Red" }, …]`.
fn titled(list: Option<&Value>) -> Result<Option<Vec<Choice>>, String> {
    let Some(list) = list else { return Ok(None) };
    let Some(list) = list.as_array() else {
        return Err("a list of options isn't a list".into());
    };
    let options = list
        .iter()
        .map(|item| {
            let value = item.get("const").and_then(Value::as_str)?;
            let label = text_of(item.get("title"), 80).unwrap_or_else(|| clean(Some(value), 80));
            Some(Choice {
                value: value.to_owned(),
                label,
            })
        })
        .collect::<Option<Vec<Choice>>>()
        .ok_or_else(|| "an option isn't a text with a title".to_owned())?;
    options_of(options).map(Some)
}

/// Options given as `["Red", "Green"]`, with the old `enumNames` for their labels.
fn plain(list: Option<&Value>, names: Option<&Value>) -> Result<Option<Vec<Choice>>, String> {
    let Some(list) = list else { return Ok(None) };
    let Some(list) = list.as_array() else {
        return Err("a list of options isn't a list".into());
    };
    let names: Vec<Option<&str>> = names
        .and_then(Value::as_array)
        .map(|n| n.iter().map(Value::as_str).collect())
        .unwrap_or_default();
    let options = list
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let value = item.as_str()?;
            let label = names
                .get(i)
                .copied()
                .flatten()
                .map(|n| clean(Some(n), 80))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| clean(Some(value), 80));
            Some(Choice {
                value: value.to_owned(),
                label,
            })
        })
        .collect::<Option<Vec<Choice>>>()
        .ok_or_else(|| "an option isn't a text".to_owned())?;
    options_of(options).map(Some)
}

/// At least one, not too many, each once.
fn options_of(options: Vec<Choice>) -> Result<Vec<Choice>, String> {
    if options.is_empty() {
        return Err("a list of options is empty".into());
    }
    if options.len() > MAX_OPTIONS {
        return Err(format!("a list has more than {MAX_OPTIONS} options"));
    }
    let mut kept: Vec<Choice> = Vec::new();
    for option in options {
        if !kept.iter().any(|k| k.value == option.value) {
            kept.push(option);
        }
    }
    Ok(kept)
}

fn options_json(options: &[Choice]) -> Value {
    options
        .iter()
        .map(|o| json!({ "value": o.value, "label": o.label }))
        .collect()
}

fn fits(format: Format, text: &str) -> bool {
    match format {
        Format::Email => {
            let mut parts = text.split('@');
            matches!(
                (parts.next(), parts.next(), parts.next()),
                (Some(user), Some(domain), None)
                    if !user.is_empty()
                        && domain.contains('.')
                        && !domain.starts_with('.')
                        && !domain.ends_with('.')
                        && !text.chars().any(char::is_whitespace)
            )
        }
        Format::Uri => Url::parse(text).is_ok(),
        Format::Date => chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok(),
        Format::DateTime => chrono::DateTime::parse_from_rfc3339(text).is_ok(),
    }
}

/// A number as people write it: 5, not 5.0.
fn shown(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 9.0e15 {
        format!("{}", number as i64)
    } else {
        format!("{number}")
    }
}

/// A field that asks for a secret: by the hub's names for them, and a PIN, a card's
/// number or its code.
fn looks_secret(text: &str) -> bool {
    if secret(text) {
        return true;
    }
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words
        .iter()
        .any(|w| matches!(*w, "pin" | "cvv" | "cvc" | "otp" | "iban"))
        || (words.contains(&"card") && words.iter().any(|w| matches!(*w, "number" | "code")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(schema: Value) -> Form {
        Form::parse(Some(&schema)).unwrap()
    }

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn every_kind_of_field_is_read_in_the_server_s_order() {
        let form = form(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "title": "Display name", "description": "Shown to others", "minLength": 3, "maxLength": 50, "pattern": "^[A-Za-z]+$", "default": "Ada" },
                "email": { "type": "string", "format": "email" },
                "age": { "type": "integer", "minimum": 18, "maximum": 130, "default": 30 },
                "ratio": { "type": "number", "minimum": 0, "maximum": 1 },
                "news": { "type": "boolean", "default": false },
                "color": { "type": "string", "enum": ["Red", "Green", "Blue"], "default": "Red" },
                "hex": { "type": "string", "oneOf": [{ "const": "#FF0000", "title": "Red" }, { "const": "#00FF00", "title": "Green" }] },
                "old": { "type": "string", "enum": ["a", "b"], "enumNames": ["Option A", "Option B"] },
                "many": { "type": "array", "minItems": 1, "maxItems": 2, "items": { "type": "string", "enum": ["x", "y", "z"] }, "default": ["x"] },
                "titled": { "type": "array", "items": { "anyOf": [{ "const": "1", "title": "One" }, { "const": "2", "title": "Two" }] } },
            },
            "required": ["name", "email"],
        }));
        let names: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "name", "email", "age", "ratio", "news", "color", "hex", "old", "many", "titled"
            ]
        );
        let name = &form.fields[0];
        assert_eq!(name.title, "Display name");
        assert_eq!(name.description.as_deref(), Some("Shown to others"));
        assert!(name.required && form.fields[1].required && !form.fields[2].required);
        assert_eq!(name.default, Some(json!("Ada")));
        assert_eq!(form.fields[2].default, Some(json!(30)));
        assert_eq!(form.fields[4].default, Some(json!(false)));
        assert!(matches!(
            form.fields[1].kind,
            Kind::Text {
                format: Some(Format::Email),
                ..
            }
        ));
        assert!(matches!(
            form.fields[2].kind,
            Kind::Number { integer: true, .. }
        ));
        let Kind::Choice { options } = &form.fields[6].kind else {
            panic!()
        };
        assert_eq!(
            (options[0].value.as_str(), options[0].label.as_str()),
            ("#FF0000", "Red")
        );
        let Kind::Choice { options } = &form.fields[7].kind else {
            panic!()
        };
        assert_eq!(options[1].label, "Option B");
        assert!(matches!(
            form.fields[8].kind,
            Kind::Choices {
                min: Some(1),
                max: Some(2),
                ..
            }
        ));
        assert_eq!(form.fields[8].default, Some(json!(["x"])));
        let Kind::Choices { options, .. } = &form.fields[9].kind else {
            panic!()
        };
        assert_eq!(options[1].label, "Two");
        let json = form.to_json();
        assert_eq!(json["fields"][0]["kind"], "text");
        assert_eq!(json["fields"][1]["format"], "email");
        assert_eq!(json["fields"][6]["options"][1]["label"], "Green");
    }

    #[test]
    fn no_schema_is_a_plain_confirmation_and_others_are_refused() {
        assert!(Form::parse(None).unwrap().fields.is_empty());
        assert!(
            form(json!({ "type": "object", "properties": {} }))
                .fields
                .is_empty()
        );
        for schema in [
            json!("text"),
            json!({ "type": "array" }),
            json!({ "properties": { "a": { "type": "object" } } }),
            json!({ "properties": { "a": { "type": "array", "items": { "type": "object" } } } }),
            json!({ "properties": { "a": { "type": "string", "enum": [] } } }),
            json!({ "properties": { "a": { "type": "string", "enum": [1, 2] } } }),
            json!({ "properties": { "a": {} } }),
        ] {
            assert!(Form::parse(Some(&schema)).is_err(), "{schema}");
        }
    }

    #[test]
    fn a_default_that_does_not_fit_is_dropped() {
        let form = form(json!({ "properties": {
            "n": { "type": "integer", "maximum": 5, "default": 9 },
            "c": { "type": "string", "enum": ["a"], "default": "b" },
        }}));
        assert_eq!(form.fields[0].default, None);
        assert_eq!(form.fields[1].default, None);
    }

    #[test]
    fn the_answer_is_checked_field_by_field() {
        let form = form(json!({
            "properties": {
                "name": { "type": "string", "title": "Name", "minLength": 2, "maxLength": 5, "pattern": "^[a-z]+$" },
                "email": { "type": "string", "format": "email" },
                "day": { "type": "string", "format": "date" },
                "when": { "type": "string", "format": "date-time" },
                "site": { "type": "string", "format": "uri" },
                "age": { "type": "integer", "minimum": 18 },
                "share": { "type": "number", "maximum": 1 },
                "news": { "type": "boolean" },
                "color": { "type": "string", "enum": ["red", "blue"] },
                "tags": { "type": "array", "items": { "enum": ["a", "b", "c"] }, "maxItems": 2 },
            },
            "required": ["name"],
        }));
        let checked = form
            .check(&map(json!({
                "name": "ada", "email": "ada@example.com", "day": "2026-11-16",
                "when": "2026-11-16T10:30:00+01:00", "site": "https://example.com",
                "age": 30.0, "share": 0.5, "news": true, "color": "blue", "tags": ["a", "c", "a"],
                "extra": "dropped",
            })))
            .unwrap();
        assert_eq!(
            Value::Object(checked),
            json!({
                "name": "ada", "email": "ada@example.com", "day": "2026-11-16",
                "when": "2026-11-16T10:30:00+01:00", "site": "https://example.com",
                "age": 30, "share": 0.5, "news": true, "color": "blue", "tags": ["a", "c"],
            })
        );
        let wrong = |answer: Value| form.check(&map(answer)).unwrap_err();
        assert_eq!(wrong(json!({})), "“Name” is required.");
        assert_eq!(wrong(json!({ "name": "" })), "“Name” is required.");
        assert!(wrong(json!({ "name": "a" })).contains("at least 2"));
        assert!(wrong(json!({ "name": "abcdef" })).contains("at most 5"));
        assert!(wrong(json!({ "name": "AB" })).contains("form the server expects"));
        assert!(wrong(json!({ "name": "ab", "email": "no" })).contains("email"));
        assert!(wrong(json!({ "name": "ab", "day": "16/11/2026" })).contains("date"));
        assert!(
            wrong(json!({ "name": "ab", "when": "2026-11-16 10:30" })).contains("date and time")
        );
        assert!(wrong(json!({ "name": "ab", "site": "not a uri" })).contains("URI"));
        assert!(wrong(json!({ "name": "ab", "age": 17 })).contains("at least 18"));
        assert!(wrong(json!({ "name": "ab", "age": 20.5 })).contains("whole number"));
        assert!(wrong(json!({ "name": "ab", "share": 2 })).contains("at most 1"));
        assert!(wrong(json!({ "name": "ab", "news": "yes" })).contains("yes or no"));
        assert!(wrong(json!({ "name": "ab", "color": "green" })).contains("one of the options"));
        assert!(wrong(json!({ "name": "ab", "tags": ["a", "b", "c"] })).contains("at most 2"));
        assert!(wrong(json!({ "name": "ab", "tags": ["d"] })).contains("among the options"));
        // Optional and empty: left out.
        assert_eq!(
            Value::Object(
                form.check(&map(json!({ "name": "ab", "email": "", "tags": [] })))
                    .unwrap()
            ),
            json!({ "name": "ab" })
        );
    }

    #[test]
    fn fields_that_ask_for_secrets_are_told() {
        let form = form(json!({ "properties": {
            "username": { "type": "string" },
            "password": { "type": "string" },
            "key": { "type": "string", "title": "API key" },
            "pin": { "type": "string", "title": "PIN" },
            "card": { "type": "string", "title": "Card number" },
            "keyword": { "type": "string" },
        }}));
        assert_eq!(
            form.secrets(),
            ["password", "API key", "PIN", "Card number"]
        );
    }

    #[test]
    fn a_page_is_shown_with_its_site_apart() {
        let page = Page::parse("https://mcp.example.com/connect?id=1", true).unwrap();
        assert_eq!(page.before, "https://");
        assert_eq!(page.host, "mcp.example.com");
        assert_eq!(page.after, "/connect?id=1");
        assert!(page.wait && page.warnings.is_empty());

        let page = Page::parse("https://google.com@evil.example:8443/x", false).unwrap();
        assert_eq!(page.before, "https://google.com@");
        assert_eq!(page.host, "evil.example:8443");
        assert!(page.warnings[0].contains("the site is evil.example"));

        let page = Page::parse("http://xn--80ak6aa92e.com/", false).unwrap();
        assert_eq!(page.warnings.len(), 2, "{:?}", page.warnings);
        assert!(
            Page::parse("http://localhost:3000/", false)
                .unwrap()
                .warnings
                .is_empty()
        );
        assert!(
            Page::parse("http://[::1]/", false)
                .unwrap()
                .warnings
                .is_empty()
        );

        for wrong in ["", "javascript:alert(1)", "file:///etc/passwd", "not a url"] {
            assert!(Page::parse(wrong, false).is_err(), "{wrong}");
        }
    }
}
