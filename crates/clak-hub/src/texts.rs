//! The questions the user reads. Titles, names and descriptions are written by the program
//! that connects: here they are cleaned up and shortened, because they end up in a window
//! that asks for a permission.

use crate::consent::{
    AgentIdentity, CallChoices, ConsentKind, ConsentLine, ConsentRequest, ConsentTool,
    SettingsChange,
};
use crate::core::{AppConn, slim};
use crate::wire::{ResourceDef, ToolDef, origin_label};
use chrono::{Local, TimeZone};
use serde_json::{Map, Value};
use unicode_general_category::{GeneralCategory, get_general_category};

pub fn session(id: String, agent: &AgentIdentity, cwd: Option<&str>) -> ConsentRequest {
    ConsentRequest {
        arguments: vec![],
        choices: None,
        id,
        kind: ConsentKind::Session,
        title: format!("{} wants to connect to Clak", agent.name),
        question: format!("Allow {} to see which programs are connected?", agent.name),
        who: who(agent, cwd),
        what: vec![],
        tools: vec![],
        warning: None,
        note: "Valid while this process stays open. Each command of a program will ask you on its own."
            .into(),
    }
}

fn tools_of(app: &AppConn) -> Vec<ConsentTool> {
    app.tools
        .iter()
        .map(|t| ConsentTool {
            name: t.name.clone(),
            description: clean(Some(&t.description), 400),
            note: None,
        })
        .collect()
}

/// An agent wants to use Clak's own settings: once for its session, because every change it
/// proposes asks again.
pub fn use_settings(
    id: String,
    agent: &AgentIdentity,
    cwd: Option<&str>,
    new_session: bool,
    app: &AppConn,
) -> ConsentRequest {
    let also = if new_session {
        " If you allow, the agent also connects to Clak."
    } else {
        ""
    };
    ConsentRequest {
        arguments: vec![],
        choices: None,
        id,
        kind: ConsentKind::Use,
        title: format!("{} wants to use Clak's settings", agent.name),
        question: format!(
            "Allow {} to see the MCP servers and REST APIs of Clak, and to propose changes?",
            agent.name
        ),
        who: who(agent, cwd),
        what: what(app),
        tools: tools_of(app),
        warning: None,
        note: format!(
            "Valid for this agent while its process stays open. Every change will ask you again; tokens and passwords are never shown.{also}"
        ),
    }
}

/// What a command's question is about.
pub struct CallQuestion<'a> {
    pub agent: &'a AgentIdentity,
    pub cwd: Option<&'a str>,
    /// The agent isn't connected yet: a yes connects it too.
    pub new_session: bool,
    pub app: &'a AppConn,
    pub tool: &'a ToolDef,
    pub arguments: Option<&'a Map<String, Value>>,
    /// Why the command asks although the user said yes before: it `changed` since they
    /// allowed it, or it is `new` since they allowed all the program's commands.
    pub note: Option<&'static str>,
    /// A yes can become a standing rule: the agent and the program can be recognized next
    /// time.
    pub standing: bool,
}

/// An agent wants to run a command: every call asks, and the answer says how far a yes goes.
pub fn call(id: String, q: &CallQuestion) -> ConsentRequest {
    let (agent, app, tool) = (q.agent, q.app, q.tool);
    let program = clean(Some(&app.resource.app), 80);
    let title = clean(Some(&app.resource.title), 80);
    let command = clean(Some(&tool.name), 64);
    let names: Vec<String> = app.tools.iter().map(|t| clean(Some(&t.name), 64)).collect();
    let mut what = what(app);
    what.push(ConsentLine::new(
        if names.len() == 1 {
            "Its command"
        } else {
            "Its commands"
        },
        list(&names),
    ));
    let warning = [
        match q.note {
            Some("changed") => Some(
                "The description or parameters of this command changed since you allowed it: review them again.",
            ),
            Some("new") => Some(
                "This command wasn't there when you allowed all of the program's commands.",
            ),
            _ => None,
        }
        .map(str::to_owned),
        insecure(app),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let also = if q.new_session {
        " If you allow, the agent also connects to Clak."
    } else {
        ""
    };
    let lasts = if app.is_connector() {
        "while the agent's process stays open and the connector stays active"
    } else {
        "while the agent's process stays open and this instance stays connected"
    };
    ConsentRequest {
        id,
        kind: ConsentKind::Call,
        title: format!("{} wants to run “{command}” in {program}", agent.name),
        question: format!(
            "Allow {} to run “{command}” in {program} — {title}?",
            agent.name
        ),
        who: who(agent, q.cwd),
        what,
        tools: vec![ConsentTool {
            name: command.clone(),
            description: clean(Some(&tool.description), 400),
            note: q.note.map(str::to_owned),
        }],
        warning: (!warning.is_empty()).then(|| warning.join(" ")),
        note: format!(
            "A yes for this session holds {lasts}; you can revoke it from the panel.{also}"
        ),
        arguments: arguments(q.arguments, tool),
        choices: Some(CallChoices {
            command: format!("“{command}” for this session"),
            program: format!("All the commands of {program} for this session"),
            block: format!("Block {program} for {}", agent.name),
            always_command: q
                .standing
                .then(|| always(agent, q.cwd, app, Some(&command))),
            always_program: q.standing.then(|| always(agent, q.cwd, app, None)),
        }),
    }
}

/// The offer under a command's question: make the yes standing, for this agent in this
/// folder (anywhere, if it doesn't say where it works), for one command or for all.
fn always(
    agent: &AgentIdentity,
    cwd: Option<&str>,
    app: &AppConn,
    command: Option<&str>,
) -> String {
    let program = if app.key.is_some() {
        format!("{} (all its instances)", clean(Some(&app.resource.app), 80))
    } else {
        app.name.clone()
    };
    let what = match command {
        Some(command) => format!("run “{command}” of {program}"),
        None => format!("use {program}"),
    };
    match cwd.filter(|c| !c.is_empty()) {
        Some(folder) => format!(
            "Always allow {} to {what} in {}",
            agent.name,
            clean(Some(folder), 200)
        ),
        None => format!("Always allow {} to {what}", agent.name),
    }
}

/// How many arguments a question shows with their value; the others are named.
const MAX_ARGUMENTS: usize = 30;
/// The longest value shown, in UTF-16 units; a longer one says how long it is.
const MAX_VALUE: usize = 300;

/// The arguments as the user reads them: one line per argument, its value as text (JSON
/// when it isn't one), cut short and saying so. Those the command declares come first, in
/// its order: the agent can't push one out of sight behind others it made up. Past
/// [`MAX_ARGUMENTS`] the rest are named. A value whose name says it is a secret — a token, a
/// password, a key — or that looks like one ("Bearer …") is masked, also inside an object:
/// the question may be on a web page.
fn arguments(arguments: Option<&Map<String, Value>>, tool: &ToolDef) -> Vec<ConsentLine> {
    let Some(arguments) = arguments else {
        return vec![];
    };
    let declared: Vec<&String> = tool
        .schema()
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| p.keys().collect())
        .unwrap_or_default();
    let mut names: Vec<&String> = declared
        .iter()
        .copied()
        .filter(|n| arguments.contains_key(*n))
        .collect();
    names.extend(arguments.keys().filter(|n| !declared.contains(n)));
    let mut lines: Vec<ConsentLine> = names
        .iter()
        .take(MAX_ARGUMENTS)
        .map(|name| {
            let value = &arguments[*name];
            let text = if secret(name) {
                masked(value)
            } else {
                match unveiled(&slim(value)) {
                    Value::String(text) => text,
                    other => serde_json::to_string(&other).unwrap_or_default(),
                }
            };
            let units = text.encode_utf16().count();
            let shown = if units > MAX_VALUE {
                format!(
                    "{} [cut: {units} characters in all]",
                    clean(Some(&text), MAX_VALUE)
                )
            } else {
                clean(Some(&text), MAX_VALUE)
            };
            ConsentLine::new(clean(Some(name), 64), shown)
        })
        .collect();
    if names.len() > MAX_ARGUMENTS {
        let rest: Vec<String> = names[MAX_ARGUMENTS..]
            .iter()
            .map(|n| clean(Some(n), 64))
            .collect();
        lines.push(ConsentLine::new(
            "…",
            format!("{} more: {}", rest.len(), rest.join(", ")),
        ));
    }
    lines
}

/// A name that says its value is a secret: a token, a password, a key, a credential, a
/// cookie. "max_tokens" and "keyword" aren't.
fn secret(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // "apiKey" and "api_key" alike: the words, and the words run together.
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    const WORDS: [&str; 14] = [
        "auth",
        "authorization",
        "bearer",
        "cookie",
        "cookies",
        "credential",
        "credentials",
        "jwt",
        "passphrase",
        "passwd",
        "password",
        "pwd",
        "secret",
        "secrets",
    ];
    let counted = ["max", "num", "count", "limit", "total", "used"];
    words.iter().any(|w| {
        WORDS.contains(w)
            || w.ends_with("key")
            || w.ends_with("token")
            || w.ends_with("secret")
            || w.ends_with("password")
            || (*w == "tokens" && !words.iter().any(|x| counted.contains(x)))
    }) || ["sessionid", "session_id", "session-id"]
        .iter()
        .any(|s| lower.contains(s))
}

/// A text that carries a credential whatever its name: an HTTP authorization.
fn looks_secret(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("bearer ")
        || lower.starts_with("basic ")
        || lower.contains("authorization: ")
        || lower.starts_with("-----begin")
}

fn masked(value: &Value) -> String {
    match value {
        Value::String(text) => format!("•••• ({} characters)", text.chars().count()),
        _ => "••••".to_owned(),
    }
}

/// The value with the secrets inside it masked.
fn unveiled(value: &Value) -> Value {
    match value {
        Value::String(text) if looks_secret(text) => Value::String(masked(value)),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if secret(k) {
                        Value::String(masked(v))
                    } else {
                        unveiled(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(unveiled).collect()),
        _ => value.clone(),
    }
}

/// Names in a line, the first 20 and how many more.
fn list(names: &[String]) -> String {
    if names.len() <= 20 {
        names.join(", ")
    } else {
        format!("{} and {} more", names[..20].join(", "), names.len() - 20)
    }
}

/// An MCP server asks for confirmation halfway through a command. The message is its own:
/// it is cleaned up, and the hub says who launched it.
pub fn confirm(
    id: String,
    app: &AppConn,
    agent: Option<&AgentIdentity>,
    cwd: Option<&str>,
    message: &str,
) -> ConsentRequest {
    let text = clean(Some(message), 600);
    ConsentRequest {
        arguments: vec![],
        choices: None,
        id,
        kind: ConsentKind::Confirm,
        title: format!(
            "{} asks for confirmation",
            clean(Some(&app.resource.app), 80)
        ),
        question: if text.is_empty() {
            "Confirm?".into()
        } else {
            text
        },
        who: agent.map(|a| who(a, cwd)).unwrap_or_default(),
        what: what(app),
        tools: vec![],
        warning: insecure(app),
        note: "Your answer applies to this command only. If you cancel, the server doesn't run it."
            .into(),
    }
}

/// An agent proposes a change to the settings: the hub says what changes, line by line;
/// names and addresses come from the agent (or from an OpenAPI), and are cleaned up.
pub fn change(
    id: String,
    agent: Option<&AgentIdentity>,
    cwd: Option<&str>,
    change: &SettingsChange,
) -> ConsentRequest {
    ConsentRequest {
        arguments: vec![],
        choices: None,
        id,
        kind: ConsentKind::Change,
        title: clean(Some(&change.title), 120),
        question: match agent {
            None => "Allow this change?".into(),
            Some(agent) => format!("Allow {} to {}?", agent.name, change.action),
        },
        who: agent.map(|a| who(a, cwd)).unwrap_or_default(),
        what: change
            .lines
            .iter()
            .map(|l| ConsentLine::new(l.label.clone(), clean(Some(&l.value), 300)))
            .collect(),
        tools: change
            .tools
            .iter()
            .map(|t| ConsentTool {
                name: clean(Some(&t.name), 64),
                description: clean(Some(&t.description), 400),
                note: t.note.clone(),
            })
            .collect(),
        warning: change.warning.clone(),
        note: "Your answer applies to this change only; you can undo it from the panel.".into(),
    }
}

pub fn app(
    id: String,
    resource: &ResourceDef,
    origin: &Value,
    tools: &[ToolDef],
    remembered: bool,
) -> ConsentRequest {
    let exe = origin.get("exe").and_then(Value::as_str);
    let mut who = vec![ConsentLine::new(
        "Executable",
        exe.unwrap_or("not identified"),
    )];
    if exe.is_some() {
        who.push(ConsentLine::new(
            "Signature",
            signature_text(
                origin.get("signed").and_then(Value::as_str),
                origin.get("signer").and_then(Value::as_str),
            ),
        ));
    }
    if let Some(pid) = origin.get("pid").filter(|p| !p.is_null()) {
        who.push(ConsentLine::new("Process", format!("pid {pid}")));
    }
    if let Some(publisher) = resource.publisher.as_deref().filter(|p| !p.is_empty()) {
        who.push(ConsentLine::new("Claims to be", clean(Some(publisher), 80)));
    }
    let warning = if exe.is_none() {
        Some("Can't tell which process this is.".to_owned())
    } else {
        match origin.get("signed").and_then(Value::as_str) {
            Some("valid") => None,
            Some("invalid") => Some("The executable's signature is NOT valid.".into()),
            _ => Some("The executable isn't signed: there's no telling who wrote it.".into()),
        }
    };
    let program = clean(Some(&resource.app), 80);
    let title = clean(Some(&resource.title), 80);
    ConsentRequest {
        arguments: vec![],
        choices: None,
        id,
        kind: ConsentKind::App,
        title: format!("{program} wants to connect to Clak"),
        question: format!("Allow {program} to make “{title}” available to agents?"),
        who,
        what: vec![ConsentLine::new("Program", format!("{program} — {title}"))],
        tools: tools
            .iter()
            .map(|t| ConsentTool {
                name: t.name.clone(),
                description: clean(Some(&t.description), 400),
                note: None,
            })
            .collect(),
        warning,
        note: if remembered {
            "Clak will remember this for this executable; you can remove it from the panel. Each command an agent runs will still ask you.".into()
        } else {
            "Valid this time only. Each command an agent runs will still ask you.".into()
        },
    }
}

fn who(agent: &AgentIdentity, cwd: Option<&str>) -> Vec<ConsentLine> {
    let process = match agent.started_at {
        Some(at) if agent.started > 0 => match Local.timestamp_opt(at as i64, 0).single() {
            Some(local) => format!(
                "pid {}, started {}",
                agent.pid,
                local.format("%b %-d %H:%M")
            ),
            None => format!("pid {}", agent.pid),
        },
        _ => format!("pid {}", agent.pid),
    };
    let mut lines = vec![
        ConsentLine::new("Agent", agent.name.clone()),
        ConsentLine::new(
            "Executable",
            agent.exe.clone().unwrap_or_else(|| "not identified".into()),
        ),
        ConsentLine::new(
            "Signature",
            signature_text(Some(&agent.signed), agent.signer.as_deref()),
        ),
        ConsentLine::new("Process", process),
    ];
    if agent.chain.len() > 1 {
        lines.push(ConsentLine::new("Chain", agent.chain.join(" ← ")));
    }
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        lines.push(ConsentLine::new(
            "Folder",
            format!("{} (declared)", clean(Some(cwd), 160)),
        ));
    }
    lines
}

fn what(app: &AppConn) -> Vec<ConsentLine> {
    let mut lines = vec![
        ConsentLine::new(
            "Program",
            format!(
                "{} — {}",
                clean(Some(&app.resource.app), 80),
                clean(Some(&app.resource.title), 80)
            ),
        ),
        ConsentLine::new(
            if app.is_connector() || app.is_hub() {
                "Service"
            } else {
                "Executable"
            },
            origin_label(&app.origin),
        ),
    ];
    if let Some(publisher) = app.resource.publisher.as_deref().filter(|p| !p.is_empty()) {
        lines.push(ConsentLine::new("Claims to be", clean(Some(publisher), 80)));
    }
    lines
}

/// A connector over `http` to another computer: the token and the data travel in clear text.
fn insecure(app: &AppConn) -> Option<String> {
    (app.is_connector()
        && app.origin.get("secure").and_then(Value::as_bool) == Some(false)
        && app.origin.get("loopback").and_then(Value::as_bool) == Some(false))
    .then(|| {
        "The connection to this service isn't encrypted (http): everything sent, including the token, travels over the network in clear text.".into()
    })
}

pub fn signature_text(signed: Option<&str>, signer: Option<&str>) -> String {
    match (signed, signer) {
        (Some("valid"), Some(s)) if !s.is_empty() => format!("signed by {s}"),
        (Some("valid"), _) => "signed".into(),
        (Some("invalid"), _) => "INVALID SIGNATURE".into(),
        (Some("unsigned"), _) => "unsigned".into(),
        _ => "can't be verified".into(),
    }
}

/// A single short line, without control characters or text-direction tricks.
/// The limit is counted in UTF-16 units.
pub fn clean(text: Option<&str>, limit: usize) -> String {
    let mut kept = String::new();
    let mut space = false;
    for c in text.unwrap_or("").chars() {
        if c.is_whitespace() {
            space = !kept.is_empty();
            continue;
        }
        if matches!(
            get_general_category(c),
            GeneralCategory::Control
                | GeneralCategory::Format
                | GeneralCategory::Surrogate
                | GeneralCategory::PrivateUse
                | GeneralCategory::Unassigned
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
        ) {
            continue;
        }
        if space {
            kept.push(' ');
        }
        space = false;
        kept.push(c);
    }
    if kept.encode_utf16().count() <= limit {
        return kept;
    }
    let mut cut = String::new();
    let mut units = 0;
    for c in kept.chars() {
        if units + c.len_utf16() > limit - 1 {
            break;
        }
        units += c.len_utf16();
        cut.push(c);
    }
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_told_by_their_name_or_their_look() {
        for name in [
            "api_key",
            "apiKey",
            "private_key",
            "access_key",
            "ssh_key",
            "key",
            "access_token",
            "refresh_tokens",
            "credentials",
            "passphrase",
            "pwd",
            "auth",
            "Authorization",
            "session_id",
            "jwt",
            "client_secret",
        ] {
            assert!(secret(name), "{name}");
        }
        for name in [
            "max_tokens",
            "keyword",
            "keys",
            "text",
            "path",
            "author",
            "line",
        ] {
            assert!(!secret(name), "{name}");
        }
        assert!(looks_secret("Bearer abc"));
        assert!(looks_secret("Authorization: Basic xyz"));
        assert!(!looks_secret("milk and bread"));
    }

    #[test]
    fn texts_are_cleaned() {
        assert_eq!(clean(Some("  a\n\tb  \u{202E}c "), 80), "a b c");
        assert_eq!(clean(Some("abcdef"), 4), "abc…");
        assert_eq!(clean(None, 4), "");
    }
}
