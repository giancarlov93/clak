//! The permissions the user made standing, with "Always allow" in a consent window: an agent
//! may use a program without being asked again, in a folder or anywhere. Kept in
//! `rules.json` in the hub's folder; removed from the panel.
//!
//! The agent is the one the operating system names, as `apps.json` remembers a program: the
//! same executable and name, and the same signer, or — if it is unsigned — the same content.
//! An unsigned agent that updates is a different one, and is asked again. The program is
//! named as a preset names it (`launch::PresetProgram`): a known application by its key, which
//! covers all its documents; a connector or an API by the name agents call it.
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
    fn to_json(&self) -> Value {
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

    fn from_json(value: &Value) -> Option<Rule> {
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

    /// The rule that lets this agent, in this folder, use this program.
    pub fn find(&self, agent_key: &str, cwd: Option<&str>, app: &AppConn) -> Option<&Rule> {
        self.list.iter().find(|r| r.covers(agent_key, cwd, app))
    }

    /// A new rule; the same agent, folder and program already there is replaced.
    pub fn add(
        &mut self,
        agent_key: &str,
        agent: &AgentIdentity,
        folder: Option<&str>,
        app: &AppConn,
    ) -> &Rule {
        let program = PresetProgram {
            target: match &app.key {
                Some(key) => Target::App(key.clone()),
                None => Target::Name(app.name.clone()),
            },
            tools: None,
        };
        let folder = folder.filter(|f| !f.is_empty()).map(str::to_owned);
        self.list
            .retain(|r| !(r.agent_key == agent_key && r.folder == folder && r.program == program));
        self.list.push(Rule {
            id: new_id("rule"),
            agent_key: agent_key.to_owned(),
            agent: agent.name.clone(),
            exe: agent.exe.clone().unwrap_or_default(),
            folder,
            program,
            label: match &app.key {
                Some(_) => app.resource.app.clone(),
                None => app.name.clone(),
            },
            since: now(),
        });
        self.save();
        self.list.last().expect("just pushed")
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
