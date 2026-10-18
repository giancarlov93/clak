//! The questions the user reads. Titles, names and descriptions are written by the program
//! that connects: here they are cleaned up and shortened, because they end up in a window
//! that asks for a permission.

use crate::consent::{
    AgentIdentity, ConsentKind, ConsentLine, ConsentRequest, ConsentTool, SettingsChange,
};
use crate::core::AppConn;
use crate::wire::{ResourceDef, ToolDef, origin_label};
use chrono::{Local, TimeZone};
use serde_json::Value;
use unicode_general_category::{GeneralCategory, get_general_category};

pub fn session(id: String, agent: &AgentIdentity, cwd: Option<&str>) -> ConsentRequest {
    ConsentRequest {
        id,
        kind: ConsentKind::Session,
        title: format!("{} wants to connect to Clak", agent.name),
        question: format!("Allow {} to see which programs are connected?", agent.name),
        who: who(agent, cwd),
        what: vec![],
        tools: vec![],
        warning: None,
        note: "Valid while this process stays open. Using a program will need separate consent."
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

pub fn use_app(
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
    if app.is_hub() {
        return ConsentRequest {
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
        };
    }
    let title = clean(Some(&app.resource.title), 80);
    let program = clean(Some(&app.resource.app), 80);
    ConsentRequest {
        id,
        kind: ConsentKind::Use,
        title: format!("{} wants to use “{title}”", agent.name),
        question: format!("Allow {} to use {program} — {title}?", agent.name),
        who: who(agent, cwd),
        what: what(app),
        tools: tools_of(app),
        warning: insecure(app),
        note: format!(
            "{}{also}",
            if app.is_connector() {
                "Valid for this agent while the connector stays active; you can revoke it from the panel."
            } else {
                "Valid for this agent while the document stays open; you can revoke it from the panel."
            }
        ),
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

pub fn tool(
    id: String,
    agent: &AgentIdentity,
    cwd: Option<&str>,
    app: &AppConn,
    tool: &ToolDef,
    changed: bool,
) -> ConsentRequest {
    let title = clean(Some(&app.resource.title), 80);
    ConsentRequest {
        id,
        kind: ConsentKind::Tool,
        title: if changed {
            format!("“{title}” changed a command")
        } else {
            format!("“{title}” has a new command")
        },
        question: format!("Allow {} to use “{}”?", agent.name, tool.name),
        who: who(agent, cwd),
        what: what(app),
        tools: vec![ConsentTool {
            name: tool.name.clone(),
            description: clean(Some(&tool.description), 400),
            note: Some(if changed { "changed" } else { "new" }.into()),
        }],
        warning: Some(
            if changed {
                "The description or parameters changed after you gave consent: review them again."
            } else {
                "This command wasn't there when you gave consent."
            }
            .into(),
        ),
        note: if app.is_connector() {
            "Valid for this agent while the connector stays active.".into()
        } else {
            "Valid for this agent while the document stays open.".into()
        },
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
            "Clak will remember this for this executable; you can remove it from the panel. Each agent will still have to ask permission to use it.".into()
        } else {
            "Valid this time only. Each agent will still have to ask permission to use it.".into()
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
    fn texts_are_cleaned() {
        assert_eq!(clean(Some("  a\n\tb  \u{202E}c "), 80), "a b c");
        assert_eq!(clean(Some("abcdef"), 4), "abc…");
        assert_eq!(clean(None, 4), "");
    }
}
