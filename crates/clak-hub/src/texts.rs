//! The questions the user reads. Titles, names and descriptions are written by the program
//! that connects: here they are cleaned up and shortened, because they end up in a window
//! that asks for a permission.

use crate::commands::Status;
use crate::consent::{
    AgentIdentity, Asks, Choices, ConsentKind, ConsentLine, ConsentRequest, ConsentTool, Scope,
    SettingsChange,
};
use crate::core::{AppConn, slim};
use crate::wire::{ResourceDef, ToolDef, origin_label};
use chrono::{Local, TimeZone};
use serde_json::{Map, Value};
use unicode_general_category::{GeneralCategory, get_general_category};

pub fn session(id: String, agent: &AgentIdentity, cwd: Option<&str>) -> ConsentRequest {
    ConsentRequest {
        asks: None,
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
        note: "Valid while this process stays open. Using a program will ask you on its own."
            .into(),
    }
}

fn tools_of(app: &AppConn) -> Vec<ConsentTool> {
    app.tools
        .iter()
        .map(|t| ConsentTool {
            name: t.name.clone(),
            description: clean(Some(&t.description), 400),
            ..ConsentTool::default()
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
        asks: None,
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

/// What the question to use a program is about.
pub struct UseQuestion<'a> {
    pub agent: &'a AgentIdentity,
    pub cwd: Option<&'a str>,
    /// The agent isn't connected yet: a yes connects it too.
    pub new_session: bool,
    pub app: &'a AppConn,
    /// Each command as Clak knows it: approved, or asking.
    pub statuses: Vec<Status>,
    /// A yes can become a standing rule: the agent and the program can be recognized next
    /// time.
    pub standing: bool,
}

/// An agent wants to use a program: asked the first time, for its session, even when all
/// the program's commands are approved. The commands that aren't still ask on their own.
pub fn use_app(id: String, q: &UseQuestion) -> ConsentRequest {
    let (agent, app) = (q.agent, q.app);
    let program = clean(Some(&app.resource.app), 80);
    let title = clean(Some(&app.resource.title), 80);
    let approved = q
        .statuses
        .iter()
        .filter(|s| **s == Status::Approved)
        .count();
    let off = q.statuses.iter().filter(|s| **s == Status::Off).count();
    let asking = q.statuses.len() - approved - off;
    let mut what = what(app);
    let mut counts = vec![format!("{approved} approved")];
    if asking > 0 {
        counts.push(format!("{asking} will ask you"));
    }
    if off > 0 {
        counts.push(format!("{off} turned off"));
    }
    what.push(ConsentLine::new("Its commands", counts.join(" · ")));
    let tools = app
        .tools
        .iter()
        .zip(&q.statuses)
        .filter(|(_, s)| **s != Status::Off)
        .map(|(t, s)| ConsentTool {
            name: t.name.clone(),
            description: clean(Some(&t.description), 400),
            note: match s {
                Status::Approved => None,
                Status::Changed => Some("changed".into()),
                Status::New => Some("new".into()),
                Status::Ask | Status::Off => Some("asks every time".into()),
            },
            ..ConsentTool::default()
        })
        .collect();
    let also = if q.new_session {
        " If you allow, the agent also connects to Clak."
    } else {
        ""
    };
    let instances = if app.key.is_some() {
        format!(", for all the instances of {program}")
    } else {
        String::new()
    };
    ConsentRequest {
        asks: None,
        id,
        kind: ConsentKind::Use,
        title: format!("{} wants to use {program}", agent.name),
        question: format!("Allow {} to use {program} — {title}?", agent.name),
        who: who(agent, q.cwd),
        what,
        tools,
        warning: insecure(app),
        note: format!(
            "Valid while the agent's process stays open{instances}; you can revoke it from the panel. The commands you haven't approved will still ask you.{also}"
        ),
        arguments: vec![],
        choices: Some(Choices {
            scopes: vec![],
            always: q.standing.then(|| always(agent, q.cwd, app)),
            block: Some(format!("Block {program} for {}", agent.name)),
        }),
    }
}

/// What a command's question is about.
pub struct CallQuestion<'a> {
    pub agent: &'a AgentIdentity,
    pub cwd: Option<&'a str>,
    pub app: &'a AppConn,
    pub tool: &'a ToolDef,
    pub arguments: Option<&'a Map<String, Value>>,
    /// The command as Clak knows it: new, changed since the user approved it, or set to ask
    /// every time.
    pub status: Status,
    /// What changed since it was approved.
    pub changes: Vec<ConsentLine>,
    /// Clak keeps the program's commands: approving is offered (not for Clak's settings).
    pub approvable: bool,
    /// The agent's preset or rule lists the commands it runs without a question, and not
    /// this one: it asks, approved or not.
    pub restricted: bool,
}

/// An agent wants to run a command the user hasn't approved: the answer says how far a yes
/// goes — this call, the command for the agent's session, or the command approved in Clak
/// for every agent allowed to use the program.
pub fn call(id: String, q: &CallQuestion) -> ConsentRequest {
    let (agent, app, tool) = (q.agent, q.app, q.tool);
    let program = clean(Some(&app.resource.app), 80);
    let title = clean(Some(&app.resource.title), 80);
    let command = clean(Some(&tool.name), 64);
    let warning = [
        match q.status {
            Status::Changed => Some(
                "The description or parameters of this command changed since you approved it: see what changed.",
            ),
            Status::New => Some("A command you haven't approved yet."),
            _ => None,
        }
        .map(str::to_owned),
        insecure(app),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    ConsentRequest {
        asks: None,
        id,
        kind: ConsentKind::Call,
        title: format!("{} wants to run “{command}” in {program}", agent.name),
        question: format!(
            "Allow {} to run “{command}” in {program} — {title}?",
            agent.name
        ),
        who: who(agent, q.cwd),
        what: what(app),
        tools: vec![ConsentTool {
            name: command.clone(),
            description: clean(Some(&tool.description), FULL_TEXT),
            note: match q.status {
                Status::Changed => Some("changed".into()),
                Status::New => Some("new".into()),
                _ if q.restricted => Some("not in the preset's list".into()),
                _ => Some("asks every time".into()),
            },
            changes: q.changes.clone(),
            parameters: parameters(tool),
        }],
        warning: (!warning.is_empty()).then(|| warning.join(" ")),
        note: if q.restricted {
            "The preset or the rule this agent uses lists the commands it runs without asking, and not this one: it asks even when approved. A yes for the session holds while the agent's process stays open."
                .into()
        } else {
            "A yes for the session holds while the agent's process stays open. An approved command runs, as it is, for every agent you let use the program; if it changes, it asks again."
                .into()
        },
        arguments: arguments(q.arguments, tool),
        choices: Some(Choices {
            scopes: [
                Some((Scope::Once, "Only this call".to_owned())),
                Some((
                    Scope::Session,
                    format!("“{command}” for this agent's session"),
                )),
                // Approving wouldn't make it run here: the preset or the rule leaves it out.
                (q.approvable && !q.restricted).then(|| {
                    (
                        Scope::Approve,
                        format!("Approve “{command}”, as it is, for {program}"),
                    )
                }),
            ]
            .into_iter()
            .flatten()
            .collect(),
            always: None,
            block: Some(format!("Block {program} for {}", agent.name)),
        }),
    }
}

/// The offer under the question to use a program: make the yes standing, for this agent in
/// this folder (anywhere, if it doesn't say where it works).
fn always(agent: &AgentIdentity, cwd: Option<&str>, app: &AppConn) -> String {
    let program = if app.key.is_some() {
        format!("{} (all its instances)", clean(Some(&app.resource.app), 80))
    } else {
        app.name.clone()
    };
    match cwd.filter(|c| !c.is_empty()) {
        Some(folder) => format!(
            "Always allow {} to use {program} in {}",
            agent.name,
            clean(Some(folder), 200)
        ),
        None => format!("Always allow {} to use {program}", agent.name),
    }
}

/// How much of a command's description a question shows: all of it, short of a flood. What
/// is approved is what the user read.
const FULL_TEXT: usize = 4000;

/// A command's parameters as the user reads them: name, type, required, description.
pub fn parameters(tool: &ToolDef) -> Vec<ConsentLine> {
    let schema = tool.schema();
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return vec![];
    };
    properties
        .iter()
        .map(|(name, p)| {
            let kind = match p.get("type") {
                Some(Value::String(t)) => t.clone(),
                Some(other) => serde_json::to_string(other).unwrap_or_default(),
                None => "any".into(),
            };
            let mut text = kind;
            if required.contains(&name.as_str()) {
                text.push_str(", required");
            }
            if let Some(values) = p.get("enum") {
                text.push_str(&format!(
                    ", one of {}",
                    serde_json::to_string(values).unwrap_or_default()
                ));
            }
            if let Some(description) = p.get("description").and_then(Value::as_str) {
                text.push_str(" — ");
                text.push_str(description);
            }
            ConsentLine::new(clean(Some(name), 64), clean(Some(&text), FULL_TEXT))
        })
        .collect()
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
pub(crate) fn secret(name: &str) -> bool {
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

/// An MCP server asks the user something halfway through a command (elicitation): to
/// confirm, to fill in a form, or to open a page (and, for a page the command waits for, to
/// say when they are done). The message is its own: it is cleaned up, and the hub says who
/// launched the command. `others` are the agents with another command running on the same
/// server, when the server can't tell which one asks (a program on this computer).
pub fn elicit(
    id: String,
    app: &AppConn,
    agent: Option<&AgentIdentity>,
    cwd: Option<&str>,
    others: &[String],
    message: &str,
    asks: Asks,
) -> ConsentRequest {
    let name = clean(Some(&app.resource.app), 80);
    let opened = "Clak opens the page in your browser: what you do there goes to the site, not to Clak or the agent.";
    let (title, fallback, note) = match &asks {
        Asks::Page(page) if page.wait => (
            format!("{name} needs you to finish in your browser"),
            "Open the page and finish there?",
            format!("{opened} When you are done, the command runs again."),
        ),
        Asks::Page(_) => (
            format!("{name} asks you to open a page"),
            "Open this page?",
            format!("{opened} Your answer applies to this command only."),
        ),
        Asks::Form(form) if !form.fields.is_empty() => (
            format!("{name} asks for information"),
            "Fill in the form?",
            "What you send goes to this server only. Your answer applies to this command only."
                .to_owned(),
        ),
        Asks::Form(_) => (
            format!("{name} asks for confirmation"),
            "Confirm?",
            "Your answer applies to this command only. If you cancel, the server doesn't run it."
                .to_owned(),
        ),
    };
    let mut warnings: Vec<String> = insecure(app).into_iter().collect();
    match &asks {
        Asks::Form(form) => {
            let secrets = form.secrets();
            if !secrets.is_empty() {
                warnings.push(format!(
                    "The server asks for something that looks like a secret ({}): a server must not ask for passwords, keys or card numbers in a form. Decline unless you are sure.",
                    secrets.join(", ")
                ));
            }
        }
        Asks::Page(page) => warnings.extend(page.warnings.iter().cloned()),
    }
    if !others.is_empty() {
        let names: Vec<String> = others.iter().map(|n| clean(Some(n), 40)).collect();
        warnings.push(format!(
            "This server is also running a command for {}: Clak can't tell which command this question comes from.",
            names.join(", ")
        ));
    }
    let text = clean(Some(message), 600);
    ConsentRequest {
        arguments: vec![],
        choices: None,
        id,
        kind: ConsentKind::Confirm,
        title,
        question: if text.is_empty() {
            fallback.into()
        } else {
            text
        },
        who: agent.map(|a| who(a, cwd)).unwrap_or_default(),
        what: what(app),
        tools: vec![],
        warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
        note,
        asks: Some(asks),
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
        asks: None,
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
                ..ConsentTool::default()
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
    // Its commands can be approved at once, as they are, only for a program Clak will know
    // again; never ticked from the start.
    let full = remembered && !tools.is_empty();
    let choices = full.then(|| Choices {
        always: Some(format!(
            "Also approve its {}, as {}: {} for the agents you let use {program}",
            if tools.len() == 1 {
                "command".to_owned()
            } else {
                format!("{} commands", tools.len())
            },
            if tools.len() == 1 {
                "it is"
            } else {
                "they are"
            },
            if tools.len() == 1 {
                "it will run"
            } else {
                "they will run"
            },
        )),
        ..Choices::default()
    });
    ConsentRequest {
        asks: None,
        arguments: vec![],
        choices,
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
                // Approving them is offered: in full, with what they take.
                description: clean(Some(&t.description), if full { FULL_TEXT } else { 400 }),
                parameters: if full { parameters(t) } else { vec![] },
                ..ConsentTool::default()
            })
            .collect(),
        warning,
        note: if remembered {
            "Clak will remember this for this executable; you can remove it from the panel. Each agent will still have to ask to use it.".into()
        } else {
            "Valid this time only. Each agent will still have to ask to use it.".into()
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
