//! How much the agents use the programs: per hour, per agent, per program, the commands
//! that ran. Kept in `usage.json` in the hub's folder for the last 31 days, so the panel can
//! say "today" and "this week" after the agents are gone and the hub has restarted; the
//! hours are UTC, and the panel turns them into the user's days.
//!
//! Only names and counts: never what was sent or what came back.

use clak_wire::json::{self as wire, float};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How long the counts are kept.
const KEEP_HOURS: u64 = 31 * 24;

/// How often the file is written at most: a burst of commands is one write.
const WRITE_EVERY: f64 = 5.0;

/// Hour (since 1970) → agent → program → commands.
type Hours = BTreeMap<u64, BTreeMap<String, BTreeMap<String, u64>>>;

#[derive(Debug, Default)]
pub struct Usage {
    /// Where it is kept; `None`: only in memory.
    path: Option<PathBuf>,
    hours: Hours,
    dirty: bool,
    written: f64,
}

impl Usage {
    /// What `usage.json` has; nothing, or a damaged file, is a start from zero.
    pub fn load(path: PathBuf) -> Usage {
        let mut hours = Hours::new();
        if let Some(saved) = std::fs::read(&path)
            .ok()
            .and_then(|b| wire::parse(&b).ok())
            .and_then(|v| v.get("hours").and_then(Value::as_object).cloned())
        {
            for (hour, agents) in saved {
                let (Ok(hour), Some(agents)) = (hour.parse::<u64>(), agents.as_object()) else {
                    continue;
                };
                for (agent, programs) in agents {
                    for (program, calls) in programs.as_object().into_iter().flatten() {
                        if let Some(calls) = calls.as_u64() {
                            *hours
                                .entry(hour)
                                .or_default()
                                .entry(agent.clone())
                                .or_default()
                                .entry(program.clone())
                                .or_default() += calls;
                        }
                    }
                }
            }
        }
        Usage {
            path: Some(path),
            hours,
            dirty: false,
            written: 0.0,
        }
    }

    /// A command that ran, at `at` (seconds since 1970).
    pub fn count(&mut self, at: f64, agent: &str, program: &str) {
        let hour = (at.max(0.0) / 3600.0) as u64;
        *self
            .hours
            .entry(hour)
            .or_default()
            .entry(agent.to_owned())
            .or_default()
            .entry(program.to_owned())
            .or_default() += 1;
        if let Some(oldest) = hour.checked_sub(KEEP_HOURS) {
            self.hours = self.hours.split_off(&oldest);
        }
        self.dirty = true;
    }

    /// Writes the file if something changed, at most every few seconds unless `now_too`.
    pub fn flush(&mut self, now: f64, now_too: bool) {
        if !self.dirty || (!now_too && now - self.written < WRITE_EVERY) {
            return;
        }
        let Some(path) = &self.path else {
            return;
        };
        let draft = path.with_extension("json.tmp");
        let text = wire::encode_pretty(&json!({ "hours": self.to_hours() }));
        if std::fs::write(&draft, text).is_ok() && std::fs::rename(&draft, path).is_ok() {
            self.dirty = false;
            self.written = now;
        }
    }

    fn to_hours(&self) -> Value {
        Value::Object(
            self.hours
                .iter()
                .map(|(hour, agents)| (hour.to_string(), json!(agents)))
                .collect(),
        )
    }

    /// For the panel: one row per hour, agent and program.
    pub fn panel(&self) -> Value {
        let rows: Vec<Value> = self
            .hours
            .iter()
            .flat_map(|(hour, agents)| {
                agents.iter().flat_map(move |(agent, programs)| {
                    programs.iter().map(move |(program, calls)| {
                        json!({
                            "at": float(*hour as f64 * 3600.0),
                            "agent": agent,
                            "program": program,
                            "calls": calls,
                        })
                    })
                })
            })
            .collect();
        json!({ "type": "usage", "rows": rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_by_hour_agent_and_program_and_forgets_after_a_month() {
        let mut usage = Usage::default();
        let day = 86400.0;
        usage.count(1000.0 * day, "Claude Code", "notion");
        usage.count(1000.0 * day + 60.0, "Claude Code", "notion");
        usage.count(1000.0 * day + 120.0, "Codex", "excel");
        let rows = usage.panel()["rows"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["agent"], "Claude Code");
        assert_eq!(rows[0]["calls"], 2);
        usage.count(1032.0 * day, "Codex", "excel");
        let rows = usage.panel()["rows"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 1, "{rows:?}");
    }

    #[test]
    fn kept_in_a_file_and_read_back() {
        let dir = std::env::temp_dir().join(format!("clak-usage-{}", wire::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage.json");
        let mut usage = Usage::load(path.clone());
        usage.count(3600.0 * 500_000.0, "Claude Code", "notion");
        usage.flush(10.0, false);
        usage.count(3600.0 * 500_000.0, "Claude Code", "notion");
        usage.flush(11.0, false);
        let read = Usage::load(path.clone());
        assert_eq!(
            read.panel()["rows"][0]["calls"],
            1,
            "written once, not again within seconds"
        );
        usage.flush(11.0, true);
        let read = Usage::load(path);
        assert_eq!(read.panel()["rows"][0]["calls"], 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
