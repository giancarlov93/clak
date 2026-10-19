//! Agents launched from Clak: the user pressed Launch, so the agent that shows up is
//! authorized without a question, with the programs of the preset it was launched with.
//!
//! A command-line agent carries a secret in `CLAK_LAUNCH`, which `clak` and `clak-shim`
//! send with every request (`launch`): the first request of the expected agent that carries
//! it redeems it, once. An app (Claude Desktop, VS Code) gets no new variables if it is
//! already open: for it the next *new* session counts, within a few minutes — the app
//! itself with a valid signature, or an agent running inside it (Claude Code in VS Code).
//! A session already connected stays as it is.
//!
//! A preset names its programs in a way that survives restarts: a known application by
//! its key (executable and signature, as remembered in `apps.json`), which covers all its
//! documents, now and later; a connector or an API by the name agents call it.

use crate::consent::AgentIdentity;
use crate::core::AppConn;
use serde_json::{Map, Value, json};

/// How long a command-line agent's secret waits for its first request, in seconds.
pub const TOKEN_TTL: f64 = 12.0 * 3600.0;
/// How long an app's launch waits for its first new session, in seconds.
pub const APP_TTL: f64 = 180.0;
/// A process that started this much before the launch still counts as started after it:
/// the clocks of the hub and of the process table are not the same.
const SLACK: f64 = 2.0;

/// What a preset program points to.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A known application, by its key in `apps.json`: all its documents.
    App(String),
    /// A connector, an API or the settings, by the name agents call it.
    Name(String),
}

/// A program of a preset: all its commands, or only the ones listed.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetProgram {
    pub target: Target,
    pub tools: Option<Vec<String>>,
}

impl PresetProgram {
    /// `{"app": key}` or `{"name": name}`, with `tools` if only some commands.
    pub fn from_json(value: &Value) -> Option<PresetProgram> {
        let map = value.as_object()?;
        let text = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let target = match (text("app"), text("name")) {
            (Some(key), _) => Target::App(key),
            (None, Some(name)) => Target::Name(name),
            (None, None) => return None,
        };
        let tools = match map.get("tools") {
            Some(Value::Array(list)) => Some(
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
            _ => None,
        };
        Some(PresetProgram { target, tools })
    }

    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        match &self.target {
            Target::App(key) => map.insert("app".into(), json!(key)),
            Target::Name(name) => map.insert("name".into(), json!(name)),
        };
        if let Some(tools) = &self.tools {
            map.insert("tools".into(), json!(tools));
        }
        Value::Object(map)
    }

    /// Whether `app` is this program: a document of the known application, or the
    /// connector with that name.
    pub fn matches(&self, app: &AppConn) -> bool {
        match &self.target {
            Target::App(key) => app.key.as_deref() == Some(key.as_str()),
            Target::Name(name) => app.key.is_none() && app.name == *name,
        }
    }

    /// Whether the preset approves this command of the program.
    pub fn covers(&self, tool: &str) -> bool {
        self.tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|t| t == tool))
    }
}

/// The preset a session was launched with.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    pub programs: Vec<PresetProgram>,
}

impl Preset {
    /// The preset program that covers `app`, if any.
    pub fn program_for(&self, app: &AppConn) -> Option<&PresetProgram> {
        self.programs.iter().find(|p| p.matches(app))
    }
}

/// A launch waiting for its agent.
#[derive(Debug, Clone)]
pub struct Launch {
    pub id: String,
    /// The agent expected, by the name the walk gives it: "Claude Code", "Claude Desktop".
    pub agent: String,
    /// The secret of a command-line agent. None for an app.
    pub token: Option<String>,
    pub preset: Option<Preset>,
    /// When the user pressed Launch, in seconds since the epoch.
    pub created: f64,
    pub expires: f64,
}

impl Launch {
    /// Whether `agent` is the one this launch waits for. `token` is what the request
    /// carries, `above` the names of the processes above the agent (for an app: the agent
    /// may run inside it).
    pub fn admits(&self, agent: &AgentIdentity, token: Option<&str>, above: &[String]) -> bool {
        match &self.token {
            Some(secret) => {
                agent.name == self.agent && token.is_some_and(|given| same(secret, given))
            }
            None => {
                // Only a session that starts after the launch: one already open stays as it is.
                let new = agent
                    .started_at
                    .is_some_and(|at| at + SLACK >= self.created);
                let itself = agent.name == self.agent && agent.signed == "valid";
                let inside = above.contains(&self.agent);
                new && (itself || inside)
            }
        }
    }
}

/// Compares two secrets in the same time whatever the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, signed: &str, started_at: f64) -> AgentIdentity {
        AgentIdentity {
            pid: 42,
            started: 1,
            started_at: Some(started_at),
            exe: None,
            name: name.into(),
            chain: vec![],
            signed: signed.into(),
            signer: None,
        }
    }

    fn launch(name: &str, token: Option<&str>) -> Launch {
        Launch {
            id: "launch_1".into(),
            agent: name.into(),
            token: token.map(str::to_owned),
            preset: None,
            created: 1000.0,
            expires: 2000.0,
        }
    }

    #[test]
    fn a_command_line_agent_needs_its_secret() {
        let l = launch("Claude Code", Some("abc123"));
        let me = agent("Claude Code", "unsigned", 1001.0);
        assert!(l.admits(&me, Some("abc123"), &[]));
        assert!(!l.admits(&me, Some("abc124"), &[]));
        assert!(!l.admits(&me, None, &[]));
        // The secret of another agent's launch is no use.
        assert!(!l.admits(&agent("Codex", "valid", 1001.0), Some("abc123"), &[]));
    }

    #[test]
    fn an_app_counts_if_signed_and_new() {
        let l = launch("Claude Desktop", None);
        assert!(l.admits(&agent("Claude Desktop", "valid", 1001.0), None, &[]));
        assert!(!l.admits(&agent("Claude Desktop", "unsigned", 1001.0), None, &[]));
        // Open before the launch: it stays as it is.
        assert!(!l.admits(&agent("Claude Desktop", "valid", 900.0), None, &[]));
    }

    #[test]
    fn an_agent_inside_the_launched_app_counts() {
        let l = launch("Visual Studio Code", None);
        let inside = ["zsh".to_owned(), "Visual Studio Code".to_owned()];
        assert!(l.admits(&agent("Claude Code", "unsigned", 1005.0), None, &inside));
        assert!(!l.admits(&agent("Claude Code", "unsigned", 1005.0), None, &[]));
        assert!(!l.admits(&agent("Claude Code", "unsigned", 500.0), None, &inside));
    }

    #[test]
    fn a_preset_program_reads_and_writes_itself() {
        for value in [
            json!({ "app": "/applications/word.app|Word|firma:Microsoft" }),
            json!({ "name": "notion", "tools": ["search", "fetch"] }),
        ] {
            let program = PresetProgram::from_json(&value).unwrap();
            assert_eq!(program.to_json(), value);
        }
        assert_eq!(PresetProgram::from_json(&json!({ "tools": [] })), None);
        let some = PresetProgram::from_json(&json!({ "name": "n", "tools": ["a"] })).unwrap();
        assert!(some.covers("a") && !some.covers("b"));
        let all = PresetProgram::from_json(&json!({ "name": "n" })).unwrap();
        assert!(all.covers("anything"));
    }
}
