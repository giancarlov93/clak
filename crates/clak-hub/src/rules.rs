//! The permissions the user made standing, with "Always allow" in a command's question: an
//! agent may run a command of a program, or all of them, without being asked again, in a
//! folder or anywhere. Kept in `rules.json` in the hub's folder; removed from the panel.
//!
//! The agent is the one the operating system names, as `apps.json` remembers a program: the
//! same executable and name, and the same signer, or — if it is unsigned — the same content.
//! An unsigned agent that updates is a different one, and is asked again. The program is
//! named as a preset names it (`launch::PresetProgram`): a known application by its key, which
//! covers all its instances; a connector or an API by the name agents call it. One rule per
//! agent, folder and program: a second "always" for another command adds it to the rule.
//!
//! The folder is the one the agent says it works in: it narrows the rule, it does not prove
//! anything.

use crate::consent::{AgentIdentity, Platform};
use crate::core::AppConn;
use crate::launch::{PresetProgram, Target};
use clak_wire::json::{self as wire, float, new_id, now};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: String,
    /// The agent's identity: executable, name, signer or content.
    pub agent_key: String,
    /// What the panel shows of it.
    pub agent: String,
    pub exe: String,
    /// Only there and below; `None`: anywhere.
    pub folder: Option<String>,
    pub program: PresetProgram,
    /// The program's name when the rule was made.
    pub label: String,
    pub since: f64,
}

impl Rule {
    /// As `rules.json` keeps it: also what a backup carries.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "agent_key": self.agent_key,
            "agent": self.agent,
            "exe": self.exe,
            "folder": self.folder,
            "program": self.program.to_json(),
            "label": self.label,
            "since": float(self.since),
        })
    }

    pub fn from_json(value: &Value) -> Option<Rule> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Rule {
            id: text("id")?,
            agent_key: text("agent_key")?,
            agent: text("agent").unwrap_or_default(),
            exe: text("exe").unwrap_or_default(),
            folder: text("folder").filter(|f| !f.is_empty()),
            program: PresetProgram::from_json(value.get("program")?)?,
            label: text("label").unwrap_or_default(),
            since: value.get("since").and_then(Value::as_f64).unwrap_or(0.0),
        })
    }

    /// For the panel: who, where, which program. Not the key.
    pub fn panel(&self) -> Value {
        json!({
            "id": self.id,
            "agent": self.agent,
            "exe": self.exe,
            "folder": self.folder,
            "program": self.label,
            "commands": self.program.tools,
            "since": float(self.since),
        })
    }

    /// Whether it covers this agent, in this folder, on this program.
    fn covers(&self, agent_key: &str, cwd: Option<&str>, app: &AppConn) -> bool {
        self.agent_key == agent_key
            && self
                .folder
                .as_deref()
                .is_none_or(|f| cwd.is_some_and(|c| inside(c, f)))
            && self.program.matches(app)
    }
}

#[derive(Debug, Default)]
pub struct Rules {
    /// Where they are kept; `None`: only in memory.
    path: Option<PathBuf>,
    list: Vec<Rule>,
}

impl Rules {
    /// What `rules.json` has; nothing, or a damaged file, is no rule: the questions come back.
    pub fn load(path: PathBuf) -> Rules {
        let list = std::fs::read(&path)
            .ok()
            .and_then(|b| wire::parse(&b).ok())
            .and_then(|v| v.get("rules").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(Rule::from_json)
            .collect();
        Rules {
            path: Some(path),
            list,
        }
    }

    pub fn all(&self) -> &[Rule] {
        &self.list
    }

    /// A rule that lets this agent, in this folder, use this program (some of its commands,
    /// or all).
    pub fn find(&self, agent_key: &str, cwd: Option<&str>, app: &AppConn) -> Option<&Rule> {
        self.list.iter().find(|r| r.covers(agent_key, cwd, app))
    }

    /// What the rules for this agent, in this folder, on this program allow together: all
    /// the commands (`None`), or these. Nothing if no rule covers them.
    pub fn allowed(
        &self,
        agent_key: &str,
        cwd: Option<&str>,
        app: &AppConn,
    ) -> Option<Option<Vec<String>>> {
        let mut allowed: Option<Option<Vec<String>>> = None;
        for rule in self.list.iter().filter(|r| r.covers(agent_key, cwd, app)) {
            allowed = Some(merged(allowed, rule.program.tools.clone()));
        }
        allowed
    }

    /// A rule from "always": all the program's commands (`command` none), or one more. The
    /// same agent, folder and program already there takes it in.
    pub fn add(
        &mut self,
        agent_key: &str,
        agent: &AgentIdentity,
        folder: Option<&str>,
        app: &AppConn,
        command: Option<String>,
    ) -> &Rule {
        let target = match &app.key {
            Some(key) => Target::App(key.clone()),
            None => Target::Name(app.name.clone()),
        };
        let folder = folder.filter(|f| !f.is_empty()).map(str::to_owned);
        let label = match &app.key {
            Some(_) => app.resource.app.clone(),
            None => app.name.clone(),
        };
        let tools = command.map(|c| vec![c]);
        let at = match self.list.iter().position(|r| {
            r.agent_key == agent_key && r.folder == folder && r.program.target == target
        }) {
            Some(at) => {
                let rule = &mut self.list[at];
                rule.program.tools = merged(Some(rule.program.tools.take()), tools);
                rule.label = label;
                rule.since = now();
                at
            }
            None => {
                self.list.push(Rule {
                    id: new_id("rule"),
                    agent_key: agent_key.to_owned(),
                    agent: agent.name.clone(),
                    exe: agent.exe.clone().unwrap_or_default(),
                    folder,
                    program: PresetProgram { target, tools },
                    label,
                    since: now(),
                });
                self.list.len() - 1
            }
        };
        self.save();
        &self.list[at]
    }

    /// The user renamed an application, or pointed it to where it has moved: its rules follow
    /// it, under its new key and name.
    pub fn follow(&mut self, key: &str, to: &str, label: &str) {
        let mut changed = false;
        for rule in &mut self.list {
            if rule.program.target == Target::App(key.to_owned()) {
                rule.program.target = Target::App(to.to_owned());
                rule.label = label.to_owned();
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    /// A rule from a backup, as it was: false if the same agent, folder and program already
    /// have one that allows as much. A rule there that allows less takes in its commands. It
    /// gets an id of its own if its old one is taken.
    pub fn restore(&mut self, mut rule: Rule) -> bool {
        if let Some(there) = self.list.iter_mut().find(|r| {
            r.agent_key == rule.agent_key
                && r.folder == rule.folder
                && r.program.target == rule.program.target
        }) {
            let tools = merged(
                Some(there.program.tools.clone()),
                rule.program.tools.clone(),
            );
            if tools == there.program.tools {
                return false;
            }
            there.program.tools = tools;
            self.save();
            return true;
        }
        if self.list.iter().any(|r| r.id == rule.id) {
            rule.id = new_id("rule");
        }
        self.list.push(rule);
        self.save();
        true
    }

    pub fn remove(&mut self, id: &str) -> Option<Rule> {
        let at = self.list.iter().position(|r| r.id == id)?;
        let rule = self.list.remove(at);
        self.save();
        Some(rule)
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let content = json!({ "rules": self.list.iter().map(Rule::to_json).collect::<Vec<_>>() });
        let draft = path.with_extension("json.tmp");
        if std::fs::write(&draft, wire::encode_pretty(&content)).is_ok() {
            let _ = std::fs::rename(&draft, path);
        }
    }
}

/// Two lists of commands together; `None` is all of them. `known` is `None` when there is
/// nothing yet.
fn merged(known: Option<Option<Vec<String>>>, more: Option<Vec<String>>) -> Option<Vec<String>> {
    match (known, more) {
        (None, more) => more,
        (Some(None), _) | (_, None) => None,
        (Some(Some(mut list)), Some(more)) => {
            for tool in more {
                if !list.contains(&tool) {
                    list.push(tool);
                }
            }
            Some(list)
        }
    }
}

/// The key an agent is remembered by, or nothing if it can't be: without an executable, or
/// unsigned and unreadable. Slow for an unsigned one (its content is read): never under the
/// lock.
pub fn agent_key(platform: &dyn Platform, agent: &AgentIdentity) -> Option<String> {
    let exe = agent.exe.as_deref().filter(|e| !e.is_empty())?;
    let identity = match (agent.signed.as_str(), agent.signer.as_deref()) {
        ("valid", Some(signer)) if !signer.is_empty() => format!("signed:{signer}"),
        _ => format!("sha256:{}", platform.executable_hash(exe)?),
    };
    Some(format!("{}|{}|{identity}", exe.to_lowercase(), agent.name))
}

/// `dir` is `folder` or below it. Case counts only where the system's paths do.
fn inside(dir: &str, folder: &str) -> bool {
    let normal = |p: &str| {
        let p = p.trim_end_matches(['/', '\\']).replace('\\', "/");
        if cfg!(any(windows, target_os = "macos")) {
            p.to_lowercase()
        } else {
            p
        }
    };
    let (dir, folder) = (normal(dir), normal(folder));
    dir == folder || dir.starts_with(&format!("{folder}/"))
}

#[cfg(test)]
mod tests {
    use super::inside;

    #[test]
    fn a_folder_covers_itself_and_what_is_below_not_its_neighbours() {
        assert!(inside("/work/app", "/work/app"));
        assert!(inside("/work/app/src", "/work/app/"));
        assert!(!inside("/work/application", "/work/app"));
        assert!(!inside("/work", "/work/app"));
        assert!(inside(r"C:\Work\App\src", r"C:\Work\App"));
    }
}
